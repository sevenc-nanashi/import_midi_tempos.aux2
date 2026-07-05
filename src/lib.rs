use aviutl2::generic::BpmInfo;
use midly::{Format, MetaMessage, Timing, TrackEventKind};

static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();

#[aviutl2::plugin(GenericPlugin)]
struct ImportMidiTemposAux2 {}

impl aviutl2::generic::GenericPlugin for ImportMidiTemposAux2 {
    fn new(_info: aviutl2::common::AviUtl2Info) -> aviutl2::common::AnyResult<Self> {
        Ok(Self {})
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "import_midi_tempos.aux2".to_string(),
            information: format!(
                "Import MIDI file as BPM Grid / v{} / https://github.com/sevenc-nanashi/import_midi_tempos.aux2",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        EDIT_HANDLE.init(registry.create_edit_handle());
        registry.register_menus::<Self>();
    }
}

#[aviutl2::generic::menus]
impl ImportMidiTemposAux2 {
    #[import(name = "[import_midi_tempos.aux2] MIDIファイルからBPMグリッドを設定")]
    fn import_midi_tempos() -> aviutl2::common::AnyResult<()> {
        let Some(file) = native_dialog::FileDialogBuilder::default()
            .add_filter("MIDIファイル", ["mid", "midi"])
            .open_single_file()
            .show()?
        else {
            return Ok(());
        };
        let midi_data = std::fs::read(&file)?;
        let midi = midly::Smf::parse(&midi_data)?;
        let bpm_info = midi_to_bpm_info(&midi)?;

        EDIT_HANDLE
            .call_edit_section(move |edit_section| edit_section.set_grid_bpm_list(&bpm_info))??;
        Ok(())
    }

    #[edit(name = "import_midi_tempos.aux2\\BPMグリッドをリセット")]
    fn reset_bpm_grid(&mut self) -> aviutl2::common::AnyResult<()> {
        EDIT_HANDLE.call_edit_section(move |edit_section| {
            edit_section.set_grid_bpm_list(&[aviutl2::generic::BpmInfo {
                tempo: 120.0,
                beat: 4,
                start: 0.0,
                offset: 0.0,
            }])
        })??;
        Ok(())
    }
}

enum MidiGridEvent {
    Tempo(u32),
    TimeSignature { numerator: u8 },
}

struct TimedMidiGridEvent {
    tick: u64,
    event: MidiGridEvent,
}

fn midi_to_bpm_info(midi: &midly::Smf<'_>) -> aviutl2::common::AnyResult<Vec<BpmInfo>> {
    let (Format::SingleTrack | Format::Parallel) = midi.header.format else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sequential MIDI format is not supported",
        )
        .into());
    };
    let Timing::Metrical(ticks_per_quarter) = midi.header.timing else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SMPTE MIDI timing is not supported",
        )
        .into());
    };
    let ticks_per_quarter_ticks = u64::from(ticks_per_quarter.as_int());
    let ticks_per_quarter = ticks_per_quarter_ticks as f64;

    let mut events = midi
        .tracks
        .iter()
        .flat_map(|track| {
            let mut tick = 0_u64;
            track.iter().filter_map(move |event| {
                tick += u64::from(event.delta.as_int());
                match event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(tempo)) => Some(TimedMidiGridEvent {
                        tick,
                        event: MidiGridEvent::Tempo(tempo.as_int()),
                    }),
                    TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, _, _, _)) => {
                        Some(TimedMidiGridEvent {
                            tick,
                            event: MidiGridEvent::TimeSignature { numerator },
                        })
                    }
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| {
        let event_order = match event.event {
            MidiGridEvent::TimeSignature { .. } => 0,
            MidiGridEvent::Tempo(_) => 1,
        };
        (event.tick, event_order)
    });

    let mut bpm_info = Vec::new();
    let mut current_tick = 0_u64;
    let mut current_time = 0.0_f64;
    let mut current_tempo = 500_000_u32;
    let mut current_beat = 4_i32;
    let mut current_measure_start_tick = 0_u64;
    push_bpm_info(
        &mut bpm_info,
        bpm_info_at(0, 0, 0.0, current_tempo, current_beat, ticks_per_quarter),
    );
    for TimedMidiGridEvent { tick, event } in events {
        current_time += ticks_to_seconds(tick - current_tick, ticks_per_quarter, current_tempo);
        current_tick = tick;
        current_measure_start_tick = advance_measure_start_tick(
            current_tick,
            current_measure_start_tick,
            current_beat,
            ticks_per_quarter_ticks,
        );
        match event {
            MidiGridEvent::Tempo(tempo) => {
                if tempo == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "MIDI tempo must not be zero",
                    )
                    .into());
                }
                current_tempo = tempo;
                push_bpm_info(
                    &mut bpm_info,
                    bpm_info_at(
                        current_tick,
                        current_measure_start_tick,
                        current_time,
                        current_tempo,
                        current_beat,
                        ticks_per_quarter,
                    ),
                );
            }
            MidiGridEvent::TimeSignature { numerator } => {
                if numerator == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "MIDI time signature numerator must not be zero",
                    )
                    .into());
                }
                current_measure_start_tick = current_tick;
                current_beat = i32::from(numerator);
                push_bpm_info(
                    &mut bpm_info,
                    bpm_info_at(
                        current_tick,
                        current_measure_start_tick,
                        current_time,
                        current_tempo,
                        current_beat,
                        ticks_per_quarter,
                    ),
                );
            }
        }
    }

    Ok(bpm_info)
}

fn push_bpm_info(bpm_info: &mut Vec<BpmInfo>, item: BpmInfo) {
    if let Some(last) = bpm_info.last_mut()
        && last.start == item.start
    {
        *last = item;
        return;
    }
    bpm_info.push(item);
}

fn advance_measure_start_tick(
    tick: u64,
    measure_start_tick: u64,
    beat: i32,
    ticks_per_quarter: u64,
) -> u64 {
    assert!(measure_start_tick <= tick);
    assert!(beat > 0);
    let measure_ticks = ticks_per_quarter * beat as u64;
    measure_start_tick + (tick - measure_start_tick) / measure_ticks * measure_ticks
}

fn bpm_info_at(
    tick: u64,
    measure_start_tick: u64,
    start: f64,
    tempo: u32,
    beat: i32,
    ticks_per_quarter: f64,
) -> BpmInfo {
    assert!(measure_start_tick <= tick);
    let offset_ticks = tick - measure_start_tick;
    let seconds_per_quarter = f64::from(tempo) / 1_000_000.0;
    let offset = if offset_ticks == 0 {
        0.0
    } else {
        -(offset_ticks as f64 / ticks_per_quarter * seconds_per_quarter) as f32
    };

    BpmInfo {
        tempo: tempo_to_bpm(tempo),
        beat,
        start,
        offset,
    }
}

fn ticks_to_seconds(ticks: u64, ticks_per_quarter: f64, tempo: u32) -> f64 {
    ticks as f64 * f64::from(tempo) / 1_000_000.0 / ticks_per_quarter
}

fn tempo_to_bpm(tempo: u32) -> f32 {
    60_000_000.0 / tempo as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_info_uses_offset_for_mid_beat_tempo_change() {
        let bpm_info = bpm_info_at(240, 0, 0.25, 1_000_000, 4, 480.0);

        assert_eq!(bpm_info.tempo, 60.0);
        assert_eq!(bpm_info.beat, 4);
        assert_eq!(bpm_info.start, 0.25);
        assert_eq!(bpm_info.offset, -0.5);
    }

    #[test]
    fn bpm_info_aligns_time_signature_start() {
        let bpm_info = bpm_info_at(480 * 5, 480 * 5, 2.5, 500_000, 3, 480.0);

        assert_eq!(bpm_info.tempo, 120.0);
        assert_eq!(bpm_info.beat, 3);
        assert_eq!(bpm_info.start, 2.5);
        assert_eq!(bpm_info.offset, 0.0);
    }

    #[test]
    fn bpm_info_offsets_from_current_measure_start() {
        let bpm_info = bpm_info_at(480 * 5, 480 * 3, 2.5, 500_000, 3, 480.0);

        assert_eq!(bpm_info.tempo, 120.0);
        assert_eq!(bpm_info.beat, 3);
        assert_eq!(bpm_info.start, 2.5);
        assert_eq!(bpm_info.offset, -1.0);
    }

    #[test]
    fn advances_to_latest_measure_start() {
        assert_eq!(advance_measure_start_tick(480 * 5, 0, 4, 480), 480 * 4);
        assert_eq!(
            advance_measure_start_tick(480 * 10 + 240, 480, 3, 480),
            480 * 10
        );
    }
}

aviutl2::register_generic_plugin!(ImportMidiTemposAux2);
