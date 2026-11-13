use atomic_float::AtomicF32;
use nice_plug_egui::RepaintNotifier;
use openkontakt::{Diagnostic, Instrument, KspRuntime, load_samples};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct LoadedInstrument {
    pub control_notices: HashMap<(usize, String), String>,
    pub instrument: Instrument,
    /// Compiled KSP scripts plus the engine parameters they drive.
    pub ksp: Arc<KspRuntime>,
    pub samples: HashMap<i32, Arc<openkontakt::AudioSample>>,
    /// Groups with authored velocity fades must not also receive the inferred boundary fade.
    pub explicit_velocity_fades: Vec<bool>,
    pub diagnostics: Vec<Diagnostic>,
    pub bytes_loaded: u64,
    /// Bytes mapped from disk but not decoded: this is what a streaming sample costs.
    pub bytes_mapped: u64,
    pub loaded_at: Instant,
    pub elapsed_ms: u128,
}

impl LoadedInstrument {
    pub fn name(&self) -> &str {
        &self.instrument.name
    }
}

#[derive(Clone, Debug)]
pub enum LoadStatus {
    Empty,
    Loading {
        path: PathBuf,
        started: Instant,
    },
    Ready {
        path: PathBuf,
        name: String,
        groups: usize,
        zones: usize,
        sample_files: usize,
        bytes_loaded: u64,
        elapsed_ms: u128,
        warnings: usize,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
}

impl LoadStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GuiNote {
    pub key: u8,
    pub velocity: f32,
    pub on: bool,
}

pub struct SharedState {
    pub loaded: arc_swap::ArcSwapOption<LoadedInstrument>,
    pub status: Mutex<LoadStatus>,
    /** Held notes, 128 bits split across two words. */
    pub active_notes: [AtomicU64; 2],
    pub active_voices: AtomicU32,
    pub panic_requested: std::sync::atomic::AtomicBool,
    /// Instrument-bar mute, applied to the master output in the audio thread.
    pub instrument_muted: std::sync::atomic::AtomicBool,
    pub peak_left: AtomicF32,
    pub peak_right: AtomicF32,
    /// Rolling DSP load of the audio callback, in percent.
    pub cpu_load: AtomicF32,
    load_generation: AtomicU64,
    gui_notes: Mutex<rtrb::Producer<GuiNote>>,
    pub repaint: RepaintNotifier,
    repaint_tx: std::sync::mpsc::Sender<()>,
}

/// Load an instrument the way the plugin does: parse it, resolve and map the samples, compile the
/// scripts, then restore their persisted control values.
///
/// The GUI loader and the offline renderer must agree exactly, otherwise a diff against a reference
/// render measures the loader rather than the engine.
pub fn load_instrument(path: &std::path::Path) -> Result<LoadedInstrument, openkontakt::Error> {
    let started = Instant::now();
    let instrument = Instrument::load(path)?;
    let loaded = load_samples(&instrument);
    let mut diagnostics = instrument.diagnostics.clone();
    diagnostics.extend(loaded.diagnostics.clone());

    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    // Diagnostic switch: some instruments route every note through their own script, which makes
    // it impossible to tell "the sample path is broken" from "the script is". Skipping the scripts
    // plays the plain zone mapping instead.
    let without_scripts = std::env::var("OPENKONTAKT_NO_SCRIPTS").is_ok();
    let texts = if without_scripts {
        vec![None; texts.len()]
    } else {
        texts
    };
    let ksp = Arc::new(KspRuntime::compile_instrument(
        &texts,
        &root,
        &instrument.groups,
    ));
    for group in &instrument.groups {
        ksp.engine
            .set_group_gain(group.index, group.volume.max(0.0));
    }
    ksp.initialize();
    let persistent = instrument
        .scripts
        .iter()
        .map(|slot| {
            let entries = slot
                .persistent
                .iter()
                .map(|entry| (entry.name.clone(), entry.values.clone()))
                .collect();
            (slot.index, entries)
        })
        .collect::<Vec<_>>();
    ksp.apply_persistent_batch(&persistent);
    let mut control_notices = HashMap::new();
    for slot in 0..ksp.slot_count() {
        let controls = ksp.controls(slot);
        if controls.iter().any(|c| c.name == "Noise") {
            if let Some(groups) = ksp.array_variable(slot, "%Room_Tone_Groups") {
                if !groups.is_empty()
                    && groups
                        .iter()
                        .all(|g| !instrument.zones.iter().any(|z| z.group_index as i64 == *g))
                {
                    control_notices.insert(
                        (slot, "Noise".into()),
                        "Unavailable in this preset: the room-tone groups contain no sample zones."
                            .into(),
                    );
                }
            }
        }
        if controls.iter().any(|c| c.name == "Rel_Trig") {
            let groups = (1..=24)
                .filter_map(|i| ksp.array_variable(slot, &format!("%Art_Release_{i:02}_Groups")))
                .flatten()
                .collect::<Vec<_>>();
            if !groups.is_empty()
                && groups.iter().all(|g| {
                    let parsed = usize::try_from(*g)
                        .ok()
                        .and_then(|g| ksp.engine.modulation.get(g))
                        .and_then(|g| g.find("REL_VOLUME"))
                        .is_some();
                    let preserved = usize::try_from(*g)
                        .ok()
                        .and_then(|g| instrument.groups.get(g))
                        .is_some_and(|g| {
                            g.children.iter().any(|c| {
                                c.body
                                    .windows(b"REL_VOLUME".len())
                                    .any(|w| w == b"REL_VOLUME")
                            })
                        });
                    !parsed && !preserved
                })
            {
                control_notices.insert((slot,"Rel_Trig".into()),"Unavailable in this preset: the release-volume modulator requested by the script is absent. Release samples are separate from this control.".into());
            }
        }
    }
    for message in ksp.diagnostics() {
        // Script errors belong on the console, not in a status bar: they are long, they are the
        // kind of thing you paste into a bug report, and a truncated tail in the corner says
        // nothing useful.
        eprintln!("OpenKontakt: KSP: {message}");
        diagnostics.push(Diagnostic {
            severity: openkontakt::DiagnosticSeverity::Warning,
            area: "ksp",
            message,
        });
    }

    let mut explicit_velocity_fades = vec![false; instrument.groups.len()];
    for zone in &instrument.zones {
        if zone.fade_low_velocity > 0 || zone.fade_high_velocity > 0 {
            if let Some(explicit) = explicit_velocity_fades.get_mut(zone.group_index) {
                *explicit = true;
            }
        }
    }
    Ok(LoadedInstrument {
        control_notices,
        instrument,
        ksp,
        samples: loaded.samples,
        explicit_velocity_fades,
        diagnostics,
        bytes_loaded: loaded.bytes_loaded,
        bytes_mapped: loaded.bytes_mapped,
        loaded_at: Instant::now(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

impl SharedState {
    pub fn new() -> (Arc<Self>, rtrb::Consumer<GuiNote>) {
        let (gui_notes_tx, gui_notes_rx) = rtrb::RingBuffer::new(1024);
        let repaint = RepaintNotifier::new();
        let (repaint_tx, repaint_rx) = std::sync::mpsc::channel::<()>();
        let repaint_thread = repaint.clone();
        std::thread::Builder::new()
            .name("openkontakt-repaint".into())
            .spawn(move || {
                while repaint_rx.recv().is_ok() {
                    // Give the current frame time to finish, then request a new one. This is
                    // needed for mouse-up events that do not otherwise wake baseview: without
                    // it the last painted frame can keep showing a released key.
                    std::thread::sleep(std::time::Duration::from_millis(16));
                    repaint_thread.request_repaint();
                }
            })
            .ok();
        let state = Arc::new(Self {
            loaded: arc_swap::ArcSwapOption::empty(),
            status: Mutex::new(LoadStatus::Empty),
            active_notes: [AtomicU64::new(0), AtomicU64::new(0)],
            active_voices: AtomicU32::new(0),
            panic_requested: std::sync::atomic::AtomicBool::new(false),
            instrument_muted: std::sync::atomic::AtomicBool::new(false),
            peak_left: AtomicF32::new(0.0),
            peak_right: AtomicF32::new(0.0),
            cpu_load: AtomicF32::new(0.0),
            load_generation: AtomicU64::new(0),
            gui_notes: Mutex::new(gui_notes_tx),
            repaint,
            repaint_tx,
        });

        // Meters (CPU, memory, disk) change without any input, so keep the editor repainting on a
        // timer. egui's own `request_repaint_after` is not always honoured by the standalone event
        // loop, while the repaint notifier is polled by baseview.
        let weak = Arc::downgrade(&state);
        std::thread::Builder::new()
            .name("openkontakt-idle-repaint".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let Some(state) = weak.upgrade() else {
                        break;
                    };
                    state.repaint.request_repaint();
                }
            })
            .ok();

        (state, gui_notes_rx)
    }

    pub fn send_note(&self, key: u8, velocity: f32, on: bool) {
        // Update the keyboard display immediately even before the audio thread drains the queue.
        if on {
            if self.is_note_active(key) {
                return;
            }
            self.note_on(key);
        } else {
            self.note_off(key);
        }
        let _ = self
            .gui_notes
            .lock()
            .unwrap()
            .push(GuiNote { key, velocity, on });
        self.repaint.request_repaint();
        let _ = self.repaint_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a preset whose Noise control references empty room-tone groups"]
    fn labels_missing_room_tone_without_remapping_it() {
        let path = std::env::var("OPENKONTAKT_EMPTY_ROOM_TONE_NKI").expect("set the preset path");
        let loaded = super::load_instrument(std::path::Path::new(&path)).unwrap();
        assert!(
            loaded
                .control_notices
                .iter()
                .any(|((_, name), reason)| name == "Noise" && reason.contains("no sample zones"))
        );
        assert!(!loaded.ksp.engine.external_envelopes_enabled);
        for (index, slot) in loaded.instrument.scripts.iter().enumerate() {
            if let Some(groups) = loaded.ksp.array_variable(slot.index, "%Room_Tone_Groups") {
                if loaded
                    .control_notices
                    .contains_key(&(index, "Noise".into()))
                {
                    assert!(groups.iter().all(|g| {
                        !loaded
                            .instrument
                            .zones
                            .iter()
                            .any(|z| z.group_index as i64 == *g)
                    }));
                }
            }
        }
    }
}

impl SharedState {
    pub fn current(&self) -> Option<Arc<LoadedInstrument>> {
        self.loaded.load_full()
    }

    pub fn spawn_load(self: &Arc<Self>, path: PathBuf) {
        {
            let status = self.status.lock().unwrap();
            if let LoadStatus::Loading { path: loading, .. } = &*status {
                if loading == &path {
                    return;
                }
            }
        }
        let generation = self.load_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.status.lock().unwrap() = LoadStatus::Loading {
            path: path.clone(),
            started: Instant::now(),
        };
        self.repaint.request_repaint();

        let shared = Arc::clone(self);
        std::thread::Builder::new()
            .name("openkontakt-loader".into())
            .spawn(move || {
                let result = load_instrument(&path);

                if shared.load_generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                match result {
                    Ok(loaded) => {
                        let name = loaded.name().to_string();
                        let stats = loaded.instrument.stats();
                        // Kontakt's browser does not list groups; expose that breakdown in the
                        // console instead of the UI.
                        eprintln!("{name}: {} groups", loaded.instrument.groups.len());
                        for group in &loaded.instrument.groups {
                            let zones = loaded
                                .instrument
                                .zones
                                .iter()
                                .filter(|zone| zone.group_index == group.index)
                                .count();
                            eprintln!(
                                "  group {:>2}  {}{}  ·  {zones} zones",
                                group.index,
                                group.name,
                                if group.muted { " (muted)" } else { "" }
                            );
                        }
                        let summary = LoadStatus::Ready {
                            path: path.clone(),
                            name,
                            groups: stats.groups,
                            zones: stats.zones,
                            sample_files: loaded.samples.len(),
                            bytes_loaded: loaded.bytes_loaded,
                            elapsed_ms: loaded.elapsed_ms,
                            warnings: loaded.diagnostics.len(),
                        };
                        shared.loaded.store(Some(Arc::new(loaded)));
                        *shared.status.lock().unwrap() = summary;
                        shared.repaint.request_repaint();
                    }
                    Err(error) => {
                        *shared.status.lock().unwrap() = LoadStatus::Failed {
                            path,
                            message: error.to_string(),
                        };
                        shared.repaint.request_repaint();
                    }
                }
            })
            .ok();
    }

    pub fn note_on(&self, key: u8) {
        let key = key as usize & 127;
        self.active_notes[key / 64].fetch_or(1u64 << (key % 64), Ordering::Relaxed);
    }

    pub fn note_off(&self, key: u8) {
        let key_u = key as usize & 127;
        self.active_notes[key_u / 64].fetch_and(!(1u64 << (key_u % 64)), Ordering::Relaxed);
    }

    pub fn all_notes_off(&self) {
        self.active_notes[0].store(0, Ordering::Relaxed);
        self.active_notes[1].store(0, Ordering::Relaxed);
    }

    pub fn request_panic(&self) {
        self.panic_requested.store(true, Ordering::SeqCst);
        self.all_notes_off();
    }

    pub fn is_note_active(&self, key: u8) -> bool {
        let key = key as usize & 127;
        self.active_notes[key / 64].load(Ordering::Relaxed) & (1u64 << (key % 64)) != 0
    }
}
