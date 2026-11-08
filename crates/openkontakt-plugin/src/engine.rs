use crate::fx;
use crate::script_worker::{IrLoader, ScriptCommand, ScriptWorker};
use crate::state::LoadedInstrument;
use openkontakt::AudioSample;
use openkontakt::Instrument;
use openkontakt::dsp::Convolver;
use openkontakt::effects::{self, ModuleParams};
use openkontakt::envelope::{self, Ahdsr};
use openkontakt::ksp::EventContext;
use openkontakt::ksp::ScriptAction;
use std::collections::HashMap;
use std::sync::Arc;

const MAX_VOICES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvelopeStage {
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Off,
}

/// Map the stored `attack_curve` (−1…1, 0 = linear) onto a power curve.
///
/// The exact law is not in the file; this one is the standard "curve knob" shape — 0 gives a
/// straight line, positive values bend the ramp upward (fast start), negative values down (slow
/// start) — and is tuned so a curve of 0 leaves the previous linear behaviour untouched.
fn attack_exponent(curve: f32) -> f32 {
    let curve = curve.clamp(-1.0, 1.0);
    (2f32.powf(-2.0 * curve)).clamp(0.25, 4.0)
}

/// Per-voice amplitude envelope built from the group's AHDSR at note start.
#[derive(Clone, Copy, Debug)]
struct Envelope {
    stage: EnvelopeStage,
    level: f32,
    /// Samples elapsed in the attack stage, and its length.
    attack_elapsed: f32,
    attack_samples: f32,
    /// Shape exponent derived from the group's stored `attack_curve`.
    attack_exponent: f32,
    hold_left: f32,
    decay_dec: f32,
    sustain: f32,
    /// Fade-out slope: `1 / release_samples` scaled by the level the note was released at.
    release_step: f32,
    release_left: f32,
}

impl Envelope {
    fn new(envelope: Ahdsr, sample_rate: f32) -> Self {
        let rate = sample_rate.max(1.0) / 1000.0; // samples per millisecond
        let attack_samples = (envelope.attack_ms.max(0.0) * rate).max(1.0);
        let decay_samples = (envelope.decay_ms.max(0.0) * rate).max(1.0);
        let release_samples = (envelope.release_ms.max(1.0) * rate).max(1.0);
        let sustain = envelope.sustain.clamp(0.0, 1.0);
        Self {
            stage: EnvelopeStage::Attack,
            level: 0.0,
            attack_elapsed: 0.0,
            attack_samples,
            attack_exponent: attack_exponent(envelope.attack_curve),
            hold_left: envelope.hold_ms.max(0.0) * rate,
            decay_dec: (1.0 - sustain) / decay_samples,
            sustain,
            release_step: 1.0 / release_samples,
            release_left: release_samples,
        }
    }

    /// Advance one sample and return the gain to apply.
    fn next(&mut self) -> f32 {
        match self.stage {
            EnvelopeStage::Attack => {
                // Shaped, not incremental: the file stores an `attack_curve` in −1…1 (0 = linear)
                // and the reference implementation uses it. A linear ramp reaches 1 % of full level
                // almost immediately, which is audible as a click on instruments whose stored curve
                // is positive.
                self.attack_elapsed += 1.0;
                let progress = (self.attack_elapsed / self.attack_samples).min(1.0);
                self.level = progress.powf(self.attack_exponent);
                if progress >= 1.0 {
                    self.level = 1.0;
                    self.stage = if self.hold_left > 0.0 {
                        EnvelopeStage::Hold
                    } else {
                        EnvelopeStage::Decay
                    };
                }
            }
            EnvelopeStage::Hold => {
                self.hold_left -= 1.0;
                if self.hold_left <= 0.0 {
                    self.stage = EnvelopeStage::Decay;
                }
            }
            EnvelopeStage::Decay => {
                self.level -= self.decay_dec;
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.stage = EnvelopeStage::Sustain;
                }
            }
            EnvelopeStage::Sustain => self.level = self.sustain,
            EnvelopeStage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 || self.release_left <= 0.0 {
                    self.level = 0.0;
                    self.stage = EnvelopeStage::Off;
                }
                self.release_left -= 1.0;
            }
            EnvelopeStage::Off => self.level = 0.0,
        }
        self.level
    }

    /// Begin the fade-out. The envelope always takes the stored release time from wherever the
    /// level currently is, so a note released during its attack still gets the full fade.
    fn release(&mut self) {
        if self.stage != EnvelopeStage::Off {
            self.stage = EnvelopeStage::Release;
            self.release_step = self.level / self.release_left.max(1.0);
        }
    }
}

struct Voice {
    active: bool,
    note: u8,
    sample: Arc<AudioSample>,
    position: f64,
    start_frame: f64,
    end_frame: f64,
    loop_region: Option<(f64, f64)>,
    alternating: bool,
    direction: f64,
    increment: f64,
    gain_left: f32,
    gain_right: f32,
    base_gain: f32,
    env: Envelope,
    age: u64,
    /// Which bus this voice feeds. Scripts drive bus volumes through `$NI_BUS_OFFSET`.
    bus: usize,
    /// `$EVENT_ID` of the note that started this voice, for `note_off()` from a script.
    event_id: i64,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            note: 0,
            sample: Arc::new(AudioSample::silent()),
            position: 0.0,
            start_frame: 0.0,
            end_frame: 0.0,
            loop_region: None,
            alternating: false,
            direction: 1.0,
            increment: 1.0,
            gain_left: 0.0,
            gain_right: 0.0,
            base_gain: 0.0,
            env: Envelope {
                stage: EnvelopeStage::Off,
                level: 0.0,
                attack_elapsed: 0.0,
                attack_samples: 1.0,
                attack_exponent: 1.0,
                hold_left: 0.0,
                decay_dec: 0.0,
                sustain: 1.0,
                release_step: 0.0,
                release_left: 0.0,
            },
            age: 0,
            bus: 0,
            event_id: 0,
        }
    }
}

pub struct Engine {
    sample_rate: f32,
    voices: Vec<Voice>,
    age_counter: u64,
    /// `$EVENT_ID` and velocity of the note-on that started each key, for `on release`.
    event_ids: [i64; 128],
    event_velocity: [u8; 128],
    next_event_id: i64,
    current: Option<Arc<LoadedInstrument>>,
    master_tune_semitones: f64,
    /// Program insert rack, in slot order. Built once when the instrument loads.
    insert_chain: Vec<fx::Fx>,
    /// One AHDSR per group, read from the internal-modulator array.
    group_envelopes: Vec<Ahdsr>,
    /// Values the script currently asks for, so a running filter is retuned only when they move.
    filter_cutoff: f32,
    filter_resonance: f32,
    /// Send path: the IRC effect convolves a copy of the mix with the loaded impulse response.
    conv: Option<Convolver>,
    ir_loaded: Option<String>,
    /// Impulse response the program's convolution slot names, and its wet gain.
    rack_ir: Option<String>,
    rack_ir_gain: f32,
    /// How many notes each round-robin cycle has played.
    round_robin_counters: HashMap<Vec<u8>, u64>,
    send_block: usize,
    send_index: usize,
    send_in_l: Vec<f32>,
    send_in_r: Vec<f32>,
    send_out_l: Vec<f32>,
    send_out_r: Vec<f32>,
    send_level: f32,
    /// Per-bus accumulation for the current sample, so sends can be taken per microphone bus.
    bus_left: [f32; 16],
    bus_right: [f32; 16],
    /// `$ENGINE_PAR_SENDLEVEL_0..7`, one per send bus.
    send_levels: [f32; 8],
    /// KSP interpreter thread; the audio callback only posts commands and applies results.
    script: Option<ScriptWorker>,
    /// Impulse-response loader thread.
    ir: Option<IrLoader>,
    pub peak_left: f32,
    pub peak_right: f32,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            voices: (0..MAX_VOICES).map(|_| Voice::default()).collect(),
            age_counter: 0,
            event_ids: [0; 128],
            event_velocity: [0; 128],
            next_event_id: 1,
            current: None,
            master_tune_semitones: 0.0,
            insert_chain: Vec::new(),
            group_envelopes: Vec::new(),
            filter_cutoff: 1.0,
            filter_resonance: 0.0,
            conv: None,
            ir_loaded: None,
            round_robin_counters: HashMap::new(),
            rack_ir: None,
            rack_ir_gain: 1.0,
            send_block: 512,
            send_index: 0,
            send_in_l: vec![0.0; 512],
            send_in_r: vec![0.0; 512],
            send_out_l: vec![0.0; 512],
            send_out_r: vec![0.0; 512],
            send_level: 0.0,
            bus_left: [0.0; 16],
            bus_right: [0.0; 16],
            send_levels: [0.0; 8],
            script: None,
            ir: None,
            peak_left: 0.0,
            peak_right: 0.0,
        }
    }
}

impl Engine {
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
    }

    pub fn reset(&mut self) {
        for voice in &mut self.voices {
            voice.active = false;
            voice.env.stage = EnvelopeStage::Off;
        }
        self.current = None;
        self.peak_left = 0.0;
        self.peak_right = 0.0;
    }

    pub fn set_instrument(&mut self, instrument: Option<Arc<LoadedInstrument>>) {
        let changed = match (&self.current, &instrument) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.current = instrument.clone();
            for voice in &mut self.voices {
                voice.active = false;
                voice.env.stage = EnvelopeStage::Off;
            }
            self.rebuild_insert_chain(instrument.as_deref());
            self.script = instrument
                .as_ref()
                .map(|loaded| ScriptWorker::spawn(Arc::clone(&loaded.ksp)));
        } else {
            self.current = instrument;
        }
    }

    /// Read the program's insert rack once per instrument.
    fn rebuild_insert_chain(&mut self, loaded: Option<&LoadedInstrument>) {
        self.insert_chain.clear();
        self.group_envelopes.clear();
        self.filter_cutoff = 1.0;
        self.filter_resonance = 0.0;
        self.rack_ir = None;
        self.rack_ir_gain = 1.0;
        let Some(loaded) = loaded else {
            return;
        };

        // A convolution module (`0x16`) names its impulse response as a filename reference. The
        // engine's reverb return is driven by the send level, so the IR only has to be found; the
        // slot's output gain is the wet trim.
        for rack in effects::program_racks(&loaded.instrument.program.children) {
            for slot in rack.slots.iter().flatten() {
                if let ModuleParams::Convolution { ir_index } = effects::decode(slot) {
                    if let Some(path) =
                        ir_index.and_then(|index| loaded.instrument.other_path(index))
                    {
                        self.rack_ir = Some(path.display().to_string());
                        self.rack_ir_gain = slot.output_gain.clamp(0.0, 8.0);
                    }
                }
            }
        }
        let racks = effects::program_racks(&loaded.instrument.program.children);
        let Some(rack) = racks.first() else {
            return;
        };
        self.insert_chain = fx::build_chain(&rack.slots, self.sample_rate);
        self.group_envelopes = envelope::instrument_envelopes(&loaded.instrument.groups);
        if let Some(slot) = rack.slot(0) {
            if let ModuleParams::Filter { cutoff, resonance } = effects::decode(slot) {
                self.filter_cutoff = cutoff;
                self.filter_resonance = resonance;
            }
        }
    }

    /// Retune the master filter from the script's current values (0..1 knobs).
    fn sync_filter(&mut self) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let cutoff = loaded
            .ksp
            .engine
            .parameter_any("ENGINE_PAR_CUTOFF")
            .map(|value| value as f32 / 1_000_000.0)
            .unwrap_or(self.filter_cutoff);
        let resonance = loaded
            .ksp
            .engine
            .parameter_any("ENGINE_PAR_RESONANCE")
            .map(|value| value as f32 / 1_000_000.0)
            .unwrap_or(self.filter_resonance);
        if (cutoff - self.filter_cutoff).abs() < 1e-4
            && (resonance - self.filter_resonance).abs() < 1e-4
        {
            return;
        }
        self.filter_cutoff = cutoff;
        self.filter_resonance = resonance;
        for fx in &mut self.insert_chain {
            if fx.is_filter() {
                fx.retune_filter(cutoff, resonance);
            }
        }
    }

    pub fn set_master_tune(&mut self, semitones: f32) {
        self.master_tune_semitones = semitones as f64;
    }

    /// Called once per audio block, before rendering: pulls the script's live filter and send
    /// settings, and loads an impulse response the first time a script asks for it.
    pub fn prepare_block(&mut self) {
        self.sync_filter();
        let Some(loaded) = self.current.clone() else {
            return;
        };
        // The 8-way send matrix is addressed by `$ENGINE_PAR_SENDLEVEL_n`. Which index a panel
        // uses for "the reverb send" varies, so the effective send is the maximum of the eight:
        // a knob on any send slot opens the same (single) convolution return we implement today.
        for (index, level) in self.send_levels.iter_mut().enumerate() {
            let name = format!("ENGINE_PAR_SENDLEVEL_{index}");
            *level = loaded
                .ksp
                .engine
                .parameter_any(&name)
                .map(|value| (value as f32 / 1_000_000.0).clamp(0.0, 1.0))
                .unwrap_or(0.0);
        }
        self.send_level = self.send_levels.iter().copied().fold(0.0f32, f32::max);
        // Impulse responses are loaded on their own thread: reading a WAV and building the FFT
        // partitions takes milliseconds, which must never happen in the audio callback.
        if self.ir.is_none() {
            self.ir = Some(IrLoader::spawn(self.send_block));
        }
        if let Some(loader) = self.ir.as_ref() {
            if let Some(loaded_ir) = loader.poll() {
                self.conv = loaded_ir;
                if let Some(convolver) = self.conv.as_ref() {
                    let _ = convolver;
                    if let Ok(mut messages) = loaded.ksp.engine.messages.lock() {
                        messages.push(format!(
                            "IR loaded: {}",
                            self.ir_loaded.as_deref().unwrap_or("?")
                        ));
                    }
                }
            }
        }
        let request = {
            let Ok(guard) = loaded.ksp.engine.ir_request.lock() else {
                return;
            };
            guard.clone().or_else(|| self.rack_ir.clone())
        };
        let request = request.filter(|path| Some(path) != self.ir_loaded.as_ref());
        if let Some(path) = request {
            self.ir_loaded = Some(path.clone());
            if let Some(loader) = self.ir.as_ref() {
                loader.request(&path, self.sample_rate);
            }
        }
    }

    /// Add the convolver's output for this sample and feed its input ring.
    fn run_send(&mut self, left: f32, right: f32) -> (f32, f32) {
        if self.send_index >= self.send_block {
            self.send_index = 0;
            if let Some(conv) = self.conv.as_mut() {
                // `process` accumulates into the output buffers, so they have to be cleared or
                // every block is added on top of the previous ones.
                self.send_out_l.iter_mut().for_each(|value| *value = 0.0);
                self.send_out_r.iter_mut().for_each(|value| *value = 0.0);
                conv.process(
                    &self.send_in_l,
                    &self.send_in_r,
                    &mut self.send_out_l,
                    &mut self.send_out_r,
                );
            }
        }
        let index = self.send_index;
        self.send_in_l[index] = left;
        self.send_in_r[index] = right;
        self.send_index += 1;
        (self.send_out_l[index], self.send_out_r[index])
    }

    pub fn clear_peak(&mut self) {
        self.peak_left = 0.0;
        self.peak_right = 0.0;
    }

    pub fn voice_count(&self) -> usize {
        self.voices.iter().filter(|voice| voice.active).count()
    }

    pub fn note_on(&mut self, key: u8, velocity: f32) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let velocity_127 = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.event_ids[key as usize & 127] = id;
        self.event_velocity[key as usize & 127] = velocity_127;

        // The interpreter runs on its own thread; the note is started when the result arrives
        // (a few samples later) or fails back to the default routing.
        let _ = loaded;
        if let Some(worker) = self.script.as_mut() {
            worker.submit(ScriptCommand::NoteOn {
                key,
                velocity: velocity_127,
                id,
            });
        } else {
            self.trigger(key, velocity_127, None);
        }
    }

    /// Apply any script results that are ready. Called once per sample from the audio callback.
    pub fn poll_script(&mut self) {
        let mut results = Vec::new();
        if let Some(worker) = self.script.as_mut() {
            while let Some(result) = worker.poll() {
                results.push(result);
            }
        }
        for result in results {
            match result.command {
                ScriptCommand::NoteOn { key, velocity, .. } => {
                    let allowed = result.dispatch.allowed_groups.clone();
                    if !result.dispatch.ignore_event {
                        self.trigger(key, velocity, allowed.as_deref());
                    }
                    self.execute_actions(&result.dispatch.actions, allowed.as_deref());
                }
                ScriptCommand::NoteOff { .. } => {
                    self.execute_actions(
                        &result.dispatch.actions,
                        result.dispatch.allowed_groups.as_deref(),
                    );
                }
                ScriptCommand::Controller { .. } => {
                    self.execute_actions(
                        &result.dispatch.actions,
                        result.dispatch.allowed_groups.as_deref(),
                    );
                }
            }
        }
    }

    /// Build the program's impulse response on this thread. Offline renders must not wait for a
    /// loader thread: the result has to be identical on every run and available from sample zero.
    pub(crate) fn load_rack_ir_blocking(&mut self) {
        let Some(path) = self.rack_ir.clone() else {
            return;
        };
        if Some(&path) == self.ir_loaded.as_ref() {
            return;
        }
        self.conv = crate::script_worker::build(&path, self.send_block, self.sample_rate);
        self.ir_loaded = Some(path);
    }

    /// Turn the convolution return off. Used by the differential harness to separate "our reverb is
    /// wrong" from "everything else is wrong".
    pub(crate) fn disable_reverb(&mut self) {
        self.conv = None;
        self.rack_ir = None;
        self.rack_ir_gain = 0.0;
        self.send_level = 0.0;
    }

    /// Active voices with their envelope state, for diagnostics.
    pub(crate) fn debug_voices(&self) -> Vec<(String, String)> {
        self.voices
            .iter()
            .filter(|voice| voice.active)
            .map(|voice| {
                (
                    voice
                        .sample
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    format!(
                        "{:?} lvl {:.2} pos {:.0}/{:.0} inc {:.3} gain {:.4}",
                        voice.env.stage,
                        voice.env.level,
                        voice.position,
                        voice.end_frame,
                        voice.increment,
                        voice.base_gain
                    ),
                )
            })
            .collect()
    }

    /// Reverb wiring, for diagnostics.
    pub(crate) fn reverb_debug(&self) -> (Option<String>, bool, f32) {
        (self.rack_ir.clone(), self.conv.is_some(), self.send_level)
    }

    /// Offline variant of [`Engine::set_instrument`]: no script thread, so the interpreter runs on
    /// the caller's thread and every event lands on an exact sample. A render must not depend on
    /// how quickly a worker thread wakes up.
    pub(crate) fn set_instrument_blocking(&mut self, loaded: Arc<LoadedInstrument>) {
        self.set_instrument(None);
        self.current = Some(Arc::clone(&loaded));
        self.rebuild_insert_chain(Some(&loaded));
    }

    /// Note on with the script applied synchronously. Same effect as `note_on` + `poll_script`,
    /// minus the queue.
    pub(crate) fn note_on_blocking(&mut self, key: u8, velocity: f32) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let velocity_127 = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.event_ids[key as usize & 127] = id;
        self.event_velocity[key as usize & 127] = velocity_127;

        let dispatch = loaded.ksp.dispatch(
            "note",
            EventContext {
                note: key as i64,
                velocity: velocity_127 as i64,
                id,
                status: 1,
            },
        );
        let allowed = dispatch.allowed_groups.clone();
        if std::env::var("OPENKONTAKT_RENDER_DEBUG").is_ok() {
            eprintln!("debug: mask for key {key} vel {velocity_127} = {allowed:?}");
        }
        if !dispatch.ignore_event {
            self.trigger(key, velocity_127, allowed.as_deref());
        }
        self.execute_actions(&dispatch.actions, allowed.as_deref());
    }

    /// Note off with the script applied synchronously.
    pub(crate) fn note_off_blocking(&mut self, key: u8) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let id = self.event_ids[key as usize & 127];
        let dispatch = loaded.ksp.dispatch(
            "release",
            EventContext {
                note: key as i64,
                velocity: self.event_velocity[key as usize & 127] as i64,
                id,
                status: 0,
            },
        );
        for voice in &mut self.voices {
            if voice.active && voice.note == key && voice.env.stage != EnvelopeStage::Release {
                voice.env.release();
            }
        }
        self.execute_actions(&dispatch.actions, dispatch.allowed_groups.as_deref());
    }

    /// Controller event with the script applied synchronously.
    pub(crate) fn controller_blocking(&mut self, number: i64, value: i64) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let dispatch = loaded.ksp.dispatch(
            "controller",
            EventContext {
                note: number,
                velocity: value,
                id: 0,
                status: 1,
            },
        );
        self.execute_actions(&dispatch.actions, dispatch.allowed_groups.as_deref());
    }

    /// Hand a MIDI controller to the scripts. The interpreter runs on the worker thread; any
    /// `play_note` it decides on is applied when the result is polled.
    pub fn controller(&mut self, _channel: i64, number: i64, value: i64) {
        if let Some(worker) = self.script.as_mut() {
            worker.submit(ScriptCommand::Controller { number, value });
        }
    }

    fn execute_actions(&mut self, actions: &[ScriptAction], allowed: Option<&[bool]>) {
        for action in actions {
            match action {
                ScriptAction::PlayNote {
                    note,
                    velocity,
                    offset: _,
                    release: _,
                } => {
                    let note = (*note).clamp(0, 127) as u8;
                    let velocity = (*velocity).clamp(1, 127) as u8;
                    self.trigger(note, velocity, allowed);
                }
                ScriptAction::NoteOff { id } => {
                    for voice in &mut self.voices {
                        if voice.active && voice.event_id == *id {
                            voice.env.release();
                        }
                    }
                }
            }
        }
    }

    /// Start every zone that matches, honouring the script's group routing when it set one.
    /// Pick the round-robin group that plays this note, once per note-on.
    ///
    /// A cycle is every group whose start-criteria record is identical apart from the position
    /// word. One group from each cycle plays; the rest stay silent until their turn.
    ///
    /// **The `0x38` reading behind this is inferred** — see `Group::round_robin`. It holds for the
    /// two libraries that use it; the multi-microphone case is what fixes the shape of the rule.
    /// Two consequences worth knowing: a library is treated as round-robin only when at least two
    /// of its groups share everything but that word, and `ok-inspect` lists the cycles it finds,
    /// so a suspected misdetection can be confirmed without listening.
    fn round_robin_choice(
        &mut self,
        instrument: &Instrument,
        allowed: Option<&[bool]>,
    ) -> HashMap<Vec<u8>, u32> {
        let mut cycles: HashMap<Vec<u8>, Vec<(u32, usize)>> = HashMap::new();
        for group in &instrument.groups {
            // Only groups this event may play take part, and only they advance the cycle: a
            // release-trigger `play_note` must not consume a round-robin slot.
            if let Some(mask) = allowed {
                if !mask.get(group.index).copied().unwrap_or(true) {
                    continue;
                }
            }
            if let Some((position, key)) = group.round_robin() {
                cycles.entry(key).or_default().push((position, group.index));
            }
        }
        let mut active = HashMap::new();
        for (key, mut members) in cycles {
            if members.len() < 2 {
                continue;
            }
            members.sort_by_key(|(position, _)| *position);
            // The cycle length is one past the highest position. Every group sharing the record
            // plays when its own position comes round — *all* of them, not one: a multi-mic
            // library puts each microphone's RR1 and RR2 in separate groups, and a note has to
            // sound one variant of every microphone at once.
            let length = members
                .iter()
                .map(|(position, _)| *position)
                .max()
                .unwrap_or(0)
                + 1;
            let counter = self.round_robin_counters.entry(key.clone()).or_insert(0);
            let wanted = (*counter % length as u64) as u32;
            *counter = counter.wrapping_add(1);
            active.insert(key, wanted);
        }
        active
    }

    fn trigger(&mut self, key: u8, velocity_127: u8, allowed: Option<&[bool]>) {
        let Some(loaded) = self.current.clone() else {
            return;
        };
        let instrument = &loaded.instrument;
        let program = &instrument.program.params;
        if key < program.low_key
            || key > program.high_key
            || velocity_127 < program.low_velocity
            || velocity_127 > program.high_velocity
        {
            return;
        }
        let any_solo = instrument.groups.iter().any(|group| group.soloed);

        // Round-robin: groups that share a start-criteria record (apart from its position word)
        // form a cycle, and a group plays only when its position comes round. A multi-microphone
        // library has several such cycles sharing one record, so this selects *every* matching
        // group, not one: a note must sound one variant of each microphone at once.
        let active = self.round_robin_choice(instrument, allowed);
        for zone in &instrument.zones {
            if !zone.matches(key, velocity_127) {
                continue;
            }
            if let Some(mask) = allowed {
                if !mask.get(zone.group_index).copied().unwrap_or(true) {
                    continue;
                }
            }
            let Some(group) = instrument.groups.get(zone.group_index) else {
                continue;
            };
            if group.muted || (any_solo && !group.soloed) {
                continue;
            }
            if let Some((position, key)) = group.round_robin() {
                if active.get(&key).is_some_and(|wanted| *wanted != position) {
                    continue;
                }
            }
            let Some(sample_info) = zone.sample.as_ref() else {
                continue;
            };
            let Some(sample) = loaded.samples.get(&sample_info.filename_id).cloned() else {
                continue;
            };
            if sample.frames == 0 {
                continue;
            }

            let start_frame = (zone.sample_start.max(0) as f64).min(sample.frames as f64);
            let mut end_frame = if zone.sample_end > 0 {
                zone.sample_end as f64
            } else {
                sample.frames as f64
            }
            .min(sample.frames as f64);
            if end_frame <= start_frame + 1.0 {
                end_frame = sample.frames as f64;
            }

            let loop_region = zone.main_loop().and_then(|looop| {
                if looop.mode == 0 || looop.loop_length <= 0 || looop.loop_start < 0 {
                    return None;
                }
                let loop_start = looop.loop_start as f64;
                let loop_end = loop_start + looop.loop_length as f64;
                if loop_start < start_frame || loop_end > end_frame {
                    return None;
                }
                Some((loop_start, loop_end))
            });

            let pitch_ratio = if group.key_tracking {
                2f64.powf((key as f64 + program.transpose as f64 - zone.root_key as f64) / 12.0)
            } else {
                1.0
            } * 2f64.powf(self.master_tune_semitones / 12.0);
            let tune_ratio = (instrument.program.params.tune as f64
                * zone.tune.max(0.0001) as f64
                * group.tune.max(0.0001) as f64)
                .clamp(0.0001, 16.0);
            // Source samples consumed per output sample. A file recorded at a lower rate than the
            // device must be read *slower* (one source sample per `src/dev` output samples) or the
            // resampling itself transposes it: 44.1 kHz content in a 48 kHz stream is +1.47
            // semitones, and this factor used to be inverted.
            let increment =
                (sample.sample_rate as f64 / self.sample_rate as f64) * pitch_ratio * tune_ratio;
            if !increment.is_finite() || increment <= 0.0 {
                continue;
            }

            let velocity_gain = velocity_127 as f32 / 127.0;
            let base_gain = (instrument.program.params.volume.max(0.0)
                * zone.volume.max(0.0)
                * zone.gain
                * group.volume.max(0.0)
                * velocity_gain)
                .clamp(0.0, 8.0);
            let pan = (zone.pan + group.pan + program.pan).clamp(-1.0, 1.0);
            let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            let (gain_left, gain_right) = (angle.cos(), angle.sin());
            let reverse = group.reverse;
            let event_id = self.event_ids[key as usize & 127];
            let envelope = self.voice_envelope(zone.group_index);

            self.start_voice(Voice {
                active: true,
                note: key,
                sample,
                position: if reverse {
                    (end_frame - 1.0).max(start_frame)
                } else {
                    start_frame
                },
                start_frame,
                end_frame,
                loop_region,
                alternating: zone.main_loop().is_some_and(|looop| looop.alternating),
                direction: if reverse { -1.0 } else { 1.0 },
                increment,
                gain_left,
                gain_right,
                base_gain,
                env: Envelope::new(envelope, self.sample_rate),
                age: 0,
                // 255 is the "no explicit output bus" sentinel, not bus 255.
                bus: match group.private_bus {
                    Some(bus) if bus < 16 => bus as usize,
                    _ => 0,
                },
                event_id,
            });
        }
    }

    /// The group's stored AHDSR with whatever the script currently asks for applied on top.
    ///
    /// KSP exposes the group amplitude envelope as `$ENGINE_PAR_ATTACK`/`DECAY`/`SUSTAIN`/`RELEASE`
    /// at 0..1. Times map exponentially so 0 is instant and 1 is long; those limits are a player
    /// choice, not file data.
    fn voice_envelope(&self, group_index: usize) -> Ahdsr {
        let mut envelope = self
            .group_envelopes
            .get(group_index)
            .copied()
            .unwrap_or_default();
        let Some(loaded) = self.current.as_ref() else {
            return envelope;
        };
        let parameter = |name: &str| {
            loaded
                .ksp
                .engine
                .parameter(name, group_index as i64, 0, -1)
                .or_else(|| loaded.ksp.engine.parameter(name, 0, 0, -1))
                .map(|value| value as f32 / 1_000_000.0)
                .filter(|value| value.is_finite())
        };
        if let Some(value) = parameter("ENGINE_PAR_ATTACK") {
            envelope.attack_ms = openkontakt::envelope::normalized_to_ms(value);
        }
        if let Some(value) = parameter("ENGINE_PAR_DECAY") {
            envelope.decay_ms = openkontakt::envelope::normalized_to_ms(value);
        }
        if let Some(value) = parameter("ENGINE_PAR_RELEASE") {
            envelope.release_ms = openkontakt::envelope::normalized_to_ms(value);
        }
        if let Some(value) = parameter("ENGINE_PAR_SUSTAIN") {
            envelope.sustain = value.clamp(0.0, 1.0);
        }
        envelope
    }

    fn start_voice(&mut self, mut voice: Voice) {
        self.age_counter = self.age_counter.wrapping_add(1);
        voice.age = self.age_counter;
        if let Some(slot) = self.voices.iter_mut().find(|slot| !slot.active) {
            *slot = voice;
            return;
        }
        if let Some(slot) = self.voices.iter_mut().min_by_key(|slot| slot.age) {
            *slot = voice;
        }
    }

    pub fn note_off(&mut self, key: u8) {
        let id = self.event_ids[key as usize & 127];
        if let Some(worker) = self.script.as_mut() {
            worker.submit(ScriptCommand::NoteOff {
                key,
                velocity: self.event_velocity[key as usize & 127],
                id,
            });
        }
        // The voices release immediately; the script's `on release` runs on the worker and adds
        // release samples when it asks for them.
        for voice in &mut self.voices {
            if voice.active && voice.note == key && voice.env.stage != EnvelopeStage::Release {
                voice.env.release();
            }
        }
    }

    pub fn choke_all(&mut self) {
        for voice in &mut self.voices {
            voice.active = false;
            voice.env.stage = EnvelopeStage::Off;
        }
    }

    /// Render one stereo sample. Returns `(left, right)` before the master section.
    pub fn render_sample(&mut self) -> (f32, f32) {
        let mut left = 0.0;
        let mut right = 0.0;
        self.bus_left = [0.0; 16];
        self.bus_right = [0.0; 16];
        // Bus gains come from the KSP runtime, so a panel slider changes the mix while notes are
        // still sounding. `Arc` clone keeps the borrow checker out of the voice loop.
        let ksp = self.current.clone().map(|loaded| Arc::clone(&loaded.ksp));
        let bus_gain = |bus: usize| -> f32 {
            ksp.as_ref()
                .map(|ksp| ksp.engine.bus_gain(bus))
                .unwrap_or(1.0)
        };
        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }

            if voice.env.stage == EnvelopeStage::Off {
                continue;
            }
            let gain = voice.env.next();
            // The envelope reaches `Off` by itself when a linear release lands on zero, so the
            // voice has to be retired here; otherwise it stays "active" (silent) forever and the
            // pool slowly fills with corpses.
            if voice.env.stage == EnvelopeStage::Off || gain <= 0.0 {
                if voice.env.stage == EnvelopeStage::Release {
                    voice.env.stage = EnvelopeStage::Off;
                }
                if voice.env.stage == EnvelopeStage::Off {
                    voice.active = false;
                }
                continue;
            }

            let (sample_left, sample_right) = voice.sample.sample_at(voice.position);
            let mix = voice.base_gain * gain;
            let bus = voice.bus.min(15);
            self.bus_left[bus] += sample_left * voice.gain_left * mix;
            self.bus_right[bus] += sample_right * voice.gain_right * mix;

            voice.position += voice.increment * voice.direction;
            if let Some((loop_start, loop_end)) = voice.loop_region {
                let length = loop_end - loop_start;
                if length > 0.0 {
                    if voice.direction > 0.0 && voice.position >= loop_end {
                        let overshoot = voice.position - loop_end;
                        if voice.alternating {
                            voice.position = (loop_end - overshoot).max(loop_start);
                            voice.direction = -1.0;
                        } else {
                            voice.position = loop_start + overshoot % length;
                        }
                    } else if voice.direction < 0.0 && voice.position < loop_start {
                        let overshoot = loop_start - voice.position;
                        if voice.alternating {
                            voice.position = (loop_start + overshoot).min(loop_end);
                            voice.direction = 1.0;
                        } else {
                            voice.position = loop_end - overshoot % length;
                        }
                    }
                }
            } else if voice.direction > 0.0 {
                if voice.position >= voice.end_frame {
                    voice.active = false;
                    voice.env.stage = EnvelopeStage::Off;
                }
            } else if voice.position < voice.start_frame {
                voice.active = false;
                voice.env.stage = EnvelopeStage::Off;
            }
        }

        let mut send_input_left = 0.0;
        let mut send_input_right = 0.0;
        for bus in 0..16 {
            let gain = bus_gain(bus);
            let bus_left = self.bus_left[bus] * gain;
            let bus_right = self.bus_right[bus] * gain;
            left += bus_left;
            right += bus_right;
            if self.send_level > 0.0 {
                // One return, so every bus feeds it at the same level. `$ENGINE_PAR_SENDLEVEL_n`
                // is addressed with `group = -1` in the panels that use it ("all groups"), and the
                // bus index in the call is the send slot, not the channel.
                send_input_left += bus_left * self.send_level;
                send_input_right += bus_right * self.send_level;
            }
        }
        for fx in &mut self.insert_chain {
            let (l, r) = fx.process(left, right);
            left = l;
            right = r;
        }
        if self.send_level > 0.0 && self.conv.is_some() {
            let (send_l, send_r) = self.run_send(send_input_left, send_input_right);
            left += send_l * self.rack_ir_gain;
            right += send_r * self.rack_ir_gain;
        }
        self.peak_left = self.peak_left.max(left.abs());
        self.peak_right = self.peak_right.max(right.abs());
        (left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ahdsr_reaches_sustain_then_releases() {
        let envelope = Ahdsr {
            attack_curve: 0.0,
            attack_ms: 10.0,
            decay_ms: 10.0,
            hold_ms: 5.0,
            release_ms: 20.0,
            sustain: 0.5,
        };
        let mut env = Envelope::new(envelope, 48_000.0);
        let rate = 48.0; // samples per ms
        let mut peak: f32 = 0.0;
        for _ in 0..((10.0 + 5.0 + 10.0) * rate) as usize {
            peak = peak.max(env.next());
        }
        assert!(peak > 0.99, "attack should reach full scale, got {peak}");
        assert!(
            (env.level - 0.5).abs() < 0.02,
            "decay should settle at sustain, got {}",
            env.level
        );
        env.release();
        for _ in 0..(25.0 * rate) as usize {
            env.next();
        }
        assert!(
            env.level < 0.001,
            "release should reach silence, got {}",
            env.level
        );
    }

    /// The release must spend its stored time fading out. An exponential-to-silence shape (the
    /// earlier implementation) is already 1 % of the way there at the half-way point, which makes a
    /// 300 ms release sound like roughly 150 ms — the complaint that started this test.
    #[test]
    fn release_takes_the_stored_time_to_fade() {
        let envelope = Ahdsr {
            attack_curve: 0.0,
            attack_ms: 0.0,
            decay_ms: 0.0,
            hold_ms: 0.0,
            release_ms: 300.0,
            sustain: 1.0,
        };
        let mut env = Envelope::new(envelope, 48_000.0);
        let rate = 48; // samples per ms
        env.next();
        env.release();

        let mut half = 0.0f32;
        for _ in 0..(150 * rate) {
            half = env.next();
        }
        assert!(
            (half - 0.5).abs() < 0.02,
            "half-way through a linear release the level should be ~0.5, got {half}"
        );

        let mut remaining = 0usize;
        while env.level > 0.0 && remaining < 200 * rate {
            env.next();
            remaining += 1;
        }
        let tail_ms = remaining as f32 / rate as f32;
        assert!(
            (tail_ms - 150.0).abs() < 5.0,
            "the second half should take another 150 ms, took {tail_ms}"
        );
    }

    /// Render a few keys and measure the pitch that comes out. A library whose zones say
    /// `key = 67..73, root = 73` must render key 72 one semitone below the sample, i.e. an equal
    /// tempered C4 for a C#4 sample — not a minor third off.
    #[test]
    #[ignore = "requires a retail library; set OPENKONTAKT_PITCH_NKI"]
    fn renders_each_key_at_the_equal_tempered_pitch() {
        use openkontakt::envelope::group_amp_envelope;
        use openkontakt::ksp::GroupInfo;
        use openkontakt::{Instrument, KspRuntime, load_samples};

        let path = std::env::var("OPENKONTAKT_PITCH_NKI")
            .expect("set OPENKONTAKT_PITCH_NKI to a library with resolved samples");
        let instrument = Instrument::load(&path).expect("instrument should parse");
        let samples = load_samples(&instrument);
        assert!(samples.samples.len() > 4, "no samples were resolved");
        let root = instrument.library_root.display().to_string();
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
                envelope: group_amp_envelope(group),
            })
            .collect();
        let ksp = Arc::new(KspRuntime::compile_with_groups(&texts, &root, groups));
        ksp.initialize();
        let loaded = Arc::new(LoadedInstrument {
            instrument,
            ksp,
            samples: samples.samples,
            diagnostics: Vec::new(),
            bytes_loaded: 0,
            loaded_at: std::time::Instant::now(),
            elapsed_ms: 0,
        });

        for key in [48u8, 60, 69, 72, 76] {
            let mut engine = Engine::default();
            engine.set_sample_rate(48_000.0);
            engine.set_instrument(Some(Arc::clone(&loaded)));
            engine.note_on(key, 0.8);
            // The interpreter runs on its own thread and decides the group routing.
            std::thread::sleep(std::time::Duration::from_millis(60));
            let mut block = Vec::with_capacity(24_000);
            for _ in 0..24_000 {
                engine.poll_script();
                block.push(engine.render_sample().0);
            }
            let (_, measured) = dominant_note(&block, 48_000.0);
            let expected = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
            let cents = 1_200.0 * (measured / expected).log2();
            assert!(
                cents.abs() < 15.0,
                "key {key} rendered {measured:.2} Hz instead of {expected:.2} Hz ({cents:+.1} cents)"
            );
        }
    }

    /// Which chromatic note is strongest? Scores every equal tempered pitch from C1 to C7 with a
    /// DFT at the fundamental and its first four harmonics, so a piano tone cannot be mistaken for
    /// its own octave.
    fn dominant_note(samples: &[f32], sample_rate: f64) -> (f64, f64) {
        let start = samples.len() / 8;
        let window = &samples[start..];
        let mut best = (0.0f64, 0.0f64); // (score, frequency)
        for note in 24..=96 {
            let frequency = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
            let mut score = 0.0f64;
            for harmonic in 1..=4 {
                let target = frequency * harmonic as f64;
                if target >= sample_rate / 2.0 {
                    break;
                }
                let mut real = 0.0f64;
                let mut imaginary = 0.0f64;
                let step = std::f64::consts::TAU * target / sample_rate;
                for (index, value) in window.iter().enumerate() {
                    real += *value as f64 * (step * index as f64).cos();
                    imaginary += *value as f64 * (step * index as f64).sin();
                }
                score += (real * real + imaginary * imaginary).sqrt() / harmonic as f64;
            }
            if score > best.0 {
                best = (score, frequency);
            }
        }
        best
    }

    #[test]
    fn zero_attack_starts_at_full_scale_without_a_click_gap() {
        let envelope = Ahdsr {
            attack_curve: 0.0,
            attack_ms: 0.0,
            decay_ms: 0.0,
            hold_ms: 0.0,
            release_ms: 100.0,
            sustain: 1.0,
        };
        let mut env = Envelope::new(envelope, 48_000.0);
        assert!(
            env.next() > 0.9,
            "an instant attack must be audible immediately"
        );
    }
}
