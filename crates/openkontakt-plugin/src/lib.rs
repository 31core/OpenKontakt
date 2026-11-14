#![allow(clippy::collapsible_if, clippy::too_many_arguments)]

//! OpenKontakt: a Kontakt `.nki` + loose WAV sampler for VST3, CLAP and standalone.

mod editor;
mod engine;
mod fx;
mod measured_envelope;
pub mod offline;
mod params;
mod script_worker;
pub mod smf;
mod state;

use editor::OpenKontaktEditor;
use engine::Engine;
use nice_plug::editor::dpi::LogicalSize;
use nice_plug::prelude::*;
use params::OpenKontaktParams;
use state::{GuiNote, SharedState};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const MIN_EDITOR_SIZE: LogicalSize<f32> = LogicalSize::new(940.0, 620.0);
const RESIZE_HINT: ResizeHint = ResizeHint::resizable().with_min_logical_size(MIN_EDITOR_SIZE);

fn midi_cc_value(value: f32) -> i64 {
    (value.clamp(0.0, 1.0) * 127.0).round() as i64
}

pub struct OpenKontakt {
    params: Arc<OpenKontaktParams>,
    shared: Arc<SharedState>,
    engine: Engine,
    gui_notes: rtrb::Consumer<GuiNote>,
    editor_state: Arc<nice_plug_egui::EguiEditorState>,
    initial_editor: Option<OpenKontaktEditor>,
    sample_rate: f32,
}

impl Default for OpenKontakt {
    fn default() -> Self {
        let params = Arc::new(OpenKontaktParams::default());
        let (shared, gui_notes) = SharedState::new();
        let editor_state =
            nice_plug_egui::EguiEditorState::from_size(LogicalSize::new(1180.0, 780.0), 1.0);

        // Standalone use: `openkontakt path/to/instrument.nki` sets this before the plugin is
        // instantiated. In a DAW the value is restored from the plugin state on activation.
        if let Ok(path) = std::env::var("OPENKONTAKT_INSTRUMENT") {
            if !path.trim().is_empty() {
                let path = PathBuf::from(path);
                params.set_instrument(&path.display().to_string(), "");
                shared.spawn_load(path);
            }
        }

        let initial_editor = OpenKontaktEditor::new(params.clone(), shared.clone());
        Self {
            params,
            shared,
            engine: Engine::default(),
            gui_notes,
            editor_state,
            initial_editor: Some(initial_editor),
            sample_rate: 48_000.0,
        }
    }
}

impl Plugin for OpenKontakt {
    const NAME: &'static str = "OpenKontakt";
    const VENDOR: &'static str = "OpenKontakt";
    const URL: &'static str = "https://openkontakt.example";
    const EMAIL: &'static str = "hello@openkontakt.example";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: None,
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames {
                layout: Some("Stereo"),
                main_input: None,
                main_output: Some("Out"),
                aux_inputs: &[],
                aux_outputs: &[],
            },
        },
        AudioIOLayout {
            main_input_channels: None,
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
    ];
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;
    const HARD_REALTIME_ONLY: bool = false;

    type Editor = nice_plug_egui::EguiEditor<OpenKontaktEditor>;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        let editor = self.initial_editor.take()?;
        nice_plug_egui::create_egui_editor(
            self.editor_state.clone(),
            self.shared.repaint.clone(),
            nice_plug_egui::EguiNiceSettings::new()
                .with_tile("OpenKontakt")
                .with_resize_hint(RESIZE_HINT),
            editor,
        )
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        self.engine.set_sample_rate(buffer_config.sample_rate);
        self.sample_rate = buffer_config.sample_rate;
        self.engine.reset();
        self.engine
            .set_offline(buffer_config.process_mode == ProcessMode::Offline);
        self.shared.all_notes_off();
        // Pitch depends on two numbers that are invisible in the UI once a session is restored:
        // the engine rate and the global transposition. Log them, because "it sounds two semitones
        // sharp" is otherwise impossible to attribute.
        eprintln!(
            "OpenKontakt: engine {:.0} Hz, global tune {:.2} Hz ({:+.1} cents)",
            buffer_config.sample_rate,
            440.0 * 2f32.powf(self.params.tune.value() / 1200.0),
            self.params.tune.value()
        );

        // A host session restores persisted fields before activation. Load the instrument again if
        // this plugin instance has not already done so.
        let path = self.params.path();
        let already_loading = self.shared.status.lock().unwrap().is_loading();
        if !path.is_empty() && self.shared.current().is_none() && !already_loading {
            self.shared.spawn_load(PathBuf::from(path));
        }
        true
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.shared.all_notes_off();
        self.shared.active_voices.store(0, Ordering::Relaxed);
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        if self.shared.panic_requested.swap(false, Ordering::SeqCst) {
            self.engine.choke_all();
            self.shared.all_notes_off();
        }

        while let Ok(note) = self.gui_notes.pop() {
            if note.on {
                self.shared.note_on(note.key);
                self.engine.note_on(note.key, note.velocity);
            } else {
                self.shared.note_off(note.key);
                self.engine.note_off(note.key);
            }
        }

        let block_started = std::time::Instant::now();
        let loaded = self.shared.current();
        // Listener timers and `ms_to_ticks` follow the host tempo, as KONTAKT's do; without it a
        // tempo-synced LFO would run at a fixed 120 BPM whatever the project is set to.
        if let Some(tempo) = context.transport().tempo {
            if let Some(loaded) = &loaded {
                loaded.ksp.engine.set_tempo_bpm(tempo);
            }
        }
        self.engine.set_instrument(loaded);
        // Global Tune is stored in cents relative to A440; the engine wants semitones.
        self.engine
            .set_master_tune(self.params.tune.smoothed.next() / 100.0);
        self.engine.prepare_block();

        let mut next_event = context.next_event();
        for (sample_idx, mut channel_samples) in buffer.iter_samples().enumerate() {
            while let Some(event) = next_event {
                if event.timing() as usize > sample_idx {
                    break;
                }
                match event {
                    NoteEvent::NoteOn { key, velocity, .. } => {
                        self.shared.note_on(key.number_or_middle_c());
                        self.engine.note_on(key.number_or_middle_c(), velocity);
                    }
                    NoteEvent::NoteOff { key, .. } => {
                        self.shared.note_off(key.number_or_middle_c());
                        self.engine.note_off(key.number_or_middle_c());
                    }
                    NoteEvent::Choke { key, .. } => {
                        self.shared.note_off(key.number_or_middle_c());
                        self.engine.choke_note(key.number_or_middle_c());
                    }
                    NoteEvent::MidiCC {
                        channel, cc, value, ..
                    } => {
                        self.engine
                            .controller(channel as i64, cc as i64, midi_cc_value(value));
                    }
                    _ => {}
                }
                next_event = context.next_event();
            }

            // Apply whatever the KSP worker finished since the previous sample, so a script's
            // note decisions land within a few samples instead of a whole block.
            self.engine.poll_script();
            let (mut left, mut right) = self.engine.render_sample();
            let master_gain =
                nice_plug::util::db_to_gain(self.params.master_volume.smoothed.next())
                    * nice_plug::util::db_to_gain(self.params.volume.smoothed.next());
            // Balance law, not a constant-power pan: the engine already produces a stereo signal,
            // and a -3 dB centre (cos/sin at 45°) made every centred instrument 3 dB quieter than
            // the reference export.
            let pan = self.params.pan.smoothed.next().clamp(-1.0, 1.0);
            let (pan_l, pan_r) = if pan > 0.0 {
                (1.0 - pan, 1.0)
            } else {
                (1.0, 1.0 + pan)
            };
            let muted = self
                .shared
                .instrument_muted
                .load(std::sync::atomic::Ordering::Relaxed);
            let gain = if muted { 0.0 } else { master_gain };
            left *= gain * pan_l;
            right *= gain * pan_r;

            let channels = channel_samples.len();
            if channels >= 2 {
                if let Some(out) = channel_samples.get_mut(0) {
                    *out = left;
                }
                if let Some(out) = channel_samples.get_mut(1) {
                    *out = right;
                }
                for index in 2..channels {
                    if let Some(out) = channel_samples.get_mut(index) {
                        *out = 0.0;
                    }
                }
            } else if let Some(out) = channel_samples.get_mut(0) {
                *out = (left + right) * 0.5;
            }
        }

        let budget = (buffer.samples().max(1) as f32 / self.sample_rate).max(1e-6);
        let load = (block_started.elapsed().as_secs_f32() / budget * 100.0).clamp(0.0, 999.0);
        let previous = self.shared.cpu_load.load(Ordering::Relaxed);
        self.shared
            .cpu_load
            .store(previous * 0.85 + load * 0.15, Ordering::Relaxed);

        self.shared
            .active_voices
            .store(self.engine.voice_count() as u32, Ordering::Relaxed);
        self.shared
            .peak_left
            .store(self.engine.peak_left, Ordering::Relaxed);
        self.shared
            .peak_right
            .store(self.engine.peak_right, Ordering::Relaxed);
        self.engine.clear_peak();

        ProcessStatus::Normal
    }

    fn deactivate(&mut self) {
        self.engine.choke_all();
        self.shared.all_notes_off();
        self.shared.active_voices.store(0, Ordering::Relaxed);
    }
}

impl ClapPlugin for OpenKontakt {
    const CLAP_ID: &'static str = "com.openkontakt.sampler";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("OpenKontakt — plays Kontakt .nki instruments with loose WAV samples (no NKX yet)");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Sampler,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for OpenKontakt {
    const VST3_CLASS_ID: [u8; 16] = *b"OpenKontaktSampl";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Instrument, Vst3SubCategory::Sampler];
}

nice_export_clap!(OpenKontakt);
nice_export_vst3!(OpenKontakt);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cc_input_preserves_all_midi_steps() {
        assert_eq!(OpenKontakt::MIDI_INPUT, MidiConfig::MidiCCs);
        for value in 0..=127 {
            assert_eq!(midi_cc_value(value as f32 / 127.0), value);
        }
        assert_eq!(midi_cc_value(0.5), 64);
        assert_eq!(midi_cc_value(-0.1), 0);
        assert_eq!(midi_cc_value(1.1), 127);
    }
}
