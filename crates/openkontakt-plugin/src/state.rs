use atomic_float::AtomicF32;
use nice_plug_egui::RepaintNotifier;
use openkontakt::ksp::GroupInfo;
use openkontakt::{Diagnostic, Instrument, KspRuntime, load_samples};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct LoadedInstrument {
    pub instrument: Instrument,
    /// Compiled KSP scripts plus the engine parameters they drive.
    pub ksp: Arc<KspRuntime>,
    pub samples: HashMap<i32, Arc<openkontakt::AudioSample>>,
    pub diagnostics: Vec<Diagnostic>,
    pub bytes_loaded: u64,
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
    let groups: Vec<GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let ksp = Arc::new(KspRuntime::compile_with_groups(&texts, &root, groups));
    ksp.initialize();
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect();
        ksp.apply_persistent(slot.index, &entries);
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

    Ok(LoadedInstrument {
        instrument,
        ksp,
        samples: loaded.samples,
        diagnostics,
        bytes_loaded: loaded.bytes_loaded,
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
