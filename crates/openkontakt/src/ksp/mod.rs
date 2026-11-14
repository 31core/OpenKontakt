//! KSP execution: enough of the language to run a library's panel script.
//!
//! This is deliberately a real interpreter rather than a declaration scraper. Library panels
//! (`on init`, `on ui_control`, `on note`, …) drive the engine through `set_engine_par`, so a
//! reader that only extracts widget positions produces a dead GUI.
//!
//! The supported dialect is the subset used by real panels, not the whole language. Anything not
//! implemented is recorded as a diagnostic and skipped, so a script cannot take the host down.

pub mod parser;

use parser::{BinOp, Callback, Expr, Program, Stmt, UiDecl, UiKind, UnOp, VarKind};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------------------------
// Values and variables

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Real(f64),
    Str(String),
}

impl Value {
    fn as_int(&self) -> i64 {
        match self {
            Value::Int(v) => *v,
            Value::Real(v) => *v as i64,
            Value::Str(s) => s.trim().parse().unwrap_or(0),
        }
    }

    fn as_real(&self) -> f64 {
        match self {
            Value::Int(v) => *v as f64,
            Value::Real(v) => *v,
            Value::Str(s) => s.trim().parse().unwrap_or(0.0),
        }
    }

    fn as_str(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Real(v) => v.to_string(),
            Value::Str(s) => s.clone(),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Int(v) => *v != 0,
            Value::Real(v) => *v != 0.0,
            Value::Str(s) => !s.is_empty(),
        }
    }
}

#[derive(Clone, Debug)]
enum Variable {
    Scalar(Value),
    Array(Vec<Value>),
}

#[derive(Default)]
struct Vars {
    vars: HashMap<String, Variable>,
    polyphonic: HashSet<String>,
    note_vars: HashMap<i64, HashMap<String, Value>>,
    note_context: Option<i64>,
}

impl Vars {
    fn get(&self, name: &str) -> Value {
        if self.polyphonic.contains(name) {
            // There is no note instance in UI/controller/listener callbacks. Do not leak the
            // last played note's value into such a callback (notably note_off($eventid)).
            return self
                .note_context
                .and_then(|id| self.note_vars.get(&id))
                .and_then(|values| values.get(name))
                .cloned()
                .unwrap_or(Value::Int(0));
        }
        match self.vars.get(name) {
            Some(Variable::Scalar(value)) => value.clone(),
            Some(Variable::Array(values)) => values.first().cloned().unwrap_or(Value::Int(0)),
            None => Value::Int(0),
        }
    }

    fn get_index(&self, name: &str, index: i64) -> Value {
        if self.polyphonic.contains(name) {
            return self.get(name);
        }
        match self.vars.get(name) {
            Some(Variable::Array(values)) => values
                .get(index.max(0) as usize)
                .cloned()
                .unwrap_or(Value::Int(0)),
            Some(Variable::Scalar(value)) => value.clone(),
            None => Value::Int(0),
        }
    }

    fn set(&mut self, name: &str, value: Value) {
        if self.polyphonic.contains(name) {
            if let Some(id) = self.note_context {
                self.note_vars
                    .entry(id)
                    .or_default()
                    .insert(name.to_string(), value);
            }
            return;
        }
        match self.vars.get_mut(name) {
            Some(Variable::Scalar(slot)) => *slot = value,
            Some(Variable::Array(values)) => {
                if values.is_empty() {
                    values.push(value);
                } else {
                    values[0] = value;
                }
            }
            None => {
                self.vars.insert(name.to_string(), Variable::Scalar(value));
            }
        }
    }

    fn set_index(&mut self, name: &str, index: i64, value: Value) {
        if self.polyphonic.contains(name) {
            self.set(name, value);
            return;
        }
        let index = index.max(0) as usize;
        match self.vars.get_mut(name) {
            Some(Variable::Array(values)) => {
                if index < values.len() {
                    values[index] = value;
                }
            }
            Some(Variable::Scalar(slot)) => *slot = value,
            None => {
                let mut values = vec![Value::Int(0); index + 1];
                values[index] = value;
                self.vars.insert(name.to_string(), Variable::Array(values));
            }
        }
    }

    fn set_array(&mut self, name: &str, values: Vec<Value>) {
        self.vars.insert(name.to_string(), Variable::Array(values));
    }

    fn declare(&mut self, kind: VarKind, name: &str, len: Option<usize>) {
        let initial = match kind {
            VarKind::Int => Value::Int(0),
            VarKind::Real => Value::Real(0.0),
            VarKind::Str => Value::Str(String::new()),
        };
        match len {
            Some(len) => {
                self.vars.insert(
                    name.to_string(),
                    Variable::Array(vec![initial; len.min(1 << 20)]),
                );
            }
            None => {
                self.vars
                    .insert(name.to_string(), Variable::Scalar(initial));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// UI controls

#[derive(Clone, Debug)]
pub struct UiControl {
    pub kind: UiKind,
    pub name: String,
    pub min: i64,
    pub max: i64,
    pub value: i64,
    /// Control name shown above it (`set_text`).
    pub label: String,
    /// Value readout shown under it (`set_knob_label`), e.g. `500.0 ms`.
    pub value_text: Option<String>,
    pub unit: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub menu: Vec<String>,
    pub persistent: bool,
    /// Positioned with `move_control_px` (native pixels) instead of the grid.
    pub px: bool,
    /// `$CONTROL_PAR_PICTURE` name, e.g. `SimpleSlider`, `BlueDot`, `DamperLine`.
    pub picture: Option<String>,
    /// `$CONTROL_PAR_MOUSE_BEHAVIOUR`: 0 = absolute, negative = relative with |value| px per range.
    pub mouse_behaviour: i32,
    /// `$CONTROL_PAR_WIDTH` / `$CONTROL_PAR_HEIGHT` in native pixels. Picture buttons set these
    /// explicitly; without them a 27×27 button is drawn as one 87×18 grid cell.
    pub pixel_width: Option<i32>,
    pub pixel_height: Option<i32>,
    /// A `ui_table`'s steps, snapshotted from the array it is bound to.
    pub table: Vec<i64>,
    /// `$CONTROL_PAR_BAR_COLOR`, the colour a table draws its bars in.
    pub bar_color: Option<i64>,
    /// Parts hidden with `hide_part`: 1 = background, 2 = cursor, 4 = value, 8 = title,
    /// 16 = whole control.
    /// `$CONTROL_PAR_HIDE` / `hide_part` bitmask. Bit 16 the whole control, bit 1 just its
    /// background. Panels page themselves with it: every page's controls are hidden except the
    /// current page's, so a reader that ignores it stacks all of them at once.
    pub hidden_parts: u32,
}

impl UiControl {
    fn from_decl(decl: &UiDecl, args: &[i64], len: Option<usize>) -> Self {
        let _ = len;
        let (min, max) = match decl.kind {
            UiKind::Knob | UiKind::Slider | UiKind::ValueEdit => (
                args.first().copied().unwrap_or(0),
                args.get(1).copied().unwrap_or(1_000_000),
            ),
            UiKind::Table => (0, args.get(2).copied().unwrap_or(630_000)),
            // `declare ui_button $x` takes no arguments; the control is a 0/1 switch. Without this
            // every write to it clamps to 0 and the button can never latch.
            UiKind::Button | UiKind::Switch => (0, 1),
            _ => (0, 0),
        };
        Self {
            kind: decl.kind,
            name: bare(&decl.name).to_string(),
            min,
            max,
            value: min,
            label: clean_label(bare(&decl.name)),
            value_text: None,
            unit: None,
            x: 0,
            y: 0,
            width: match decl.kind {
                UiKind::Label | UiKind::Table => args.first().copied().unwrap_or(1) as i32,
                _ => 1,
            },
            height: match decl.kind {
                UiKind::Label | UiKind::Table => args.get(1).copied().unwrap_or(1) as i32,
                _ => 1,
            },
            menu: Vec::new(),
            persistent: false,
            px: false,
            picture: None,
            pixel_width: None,
            pixel_height: None,
            table: Vec::new(),
            hidden_parts: 0,
            bar_color: None,
            mouse_behaviour: 0,
        }
    }

    pub fn normalized(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) as f64 / (self.max - self.min) as f64) as f32
    }

    pub fn set_normalized(&mut self, normalized: f32) {
        self.value =
            self.min + (normalized.clamp(0.0, 1.0) as f64 * (self.max - self.min) as f64) as i64;
    }
}

/// KSP names carry a sigil (`$`, `%`, `@`, `!`); comparisons between a declaration, a callback
/// argument and a persistent entry must ignore it.
pub fn bare(name: &str) -> &str {
    name.trim_start_matches(['$', '%', '@', '!'])
}

fn clean_label(name: &str) -> String {
    let trimmed = name.trim_start_matches('$');
    trimmed.replace('_', " ")
}

// ---------------------------------------------------------------------------------------------
// Host: what a script can reach outside itself

pub trait Host {
    fn find_mod(&mut self, _group: i64, _name: &str) -> i64 {
        -1
    }
    fn find_target(&mut self, _group: i64, _modulator: i64, _name: &str) -> i64 {
        -1
    }
    fn random(&mut self, low: i64, _high: i64) -> i64 {
        low
    }
    fn change_event(&mut self, _kind: &str, _id: i64, _value: i64, _relative: bool) {}
    /// Read an engine parameter. `None` means "this instrument has no such target", which KSP
    /// answers with 0.
    fn engine_par(&mut self, param: &str, group: i64, slot: i64, generic: i64) -> Option<i64>;
    fn set_engine_par(&mut self, param: &str, value: i64, group: i64, slot: i64, generic: i64);
    fn engine_par_disp(&mut self, param: &str, group: i64, slot: i64, generic: i64) -> String;
    /// Persisted array storage (`pgs_*`).
    fn pgs_set(&mut self, _key: &str, _index: i64, _value: i64) {}
    fn pgs_get(&mut self, _key: &str, _index: i64) -> i64 {
        0
    }
    fn pgs_exists(&mut self, _key: &str) -> bool {
        false
    }
    fn pgs_create(&mut self, _key: &str, _size: i64) {}
    fn message(&mut self, _text: &str) {}

    /// `set_control_par_str` aimed at one of the instrument's own handles (`$INST_WALLPAPER_ID`
    /// and friends) rather than at a `ui_*` control.
    fn instance_par_str(&mut self, _id: i64, _par: &str, _text: &str) {}

    /// `set_control_par` aimed at one of the instrument's own handles.
    fn instance_par(&mut self, _id: i64, _par: &str, _value: i64) {
        let _ = (_id, _par, _value);
    }

    /// `set_ui_height_px`.
    fn panel_height(&mut self, _height: f32) {}
    /// `set_skin_offset`: which vertical band of the skin image this page shows.
    fn skin_offset(&mut self, _offset: f32) {}
    /// `set_key_color`: recolour one key of the instrument's keyboard.
    fn set_key_color(&mut self, _note: i64, _colour: i64) {}
    /// `set_controller`: write a MIDI controller value into the shared `%CC` table.
    fn set_controller(&mut self, _number: i64, _value: i64) {}
    /// Host tempo, for `ms_to_ticks` and the listener timer.
    fn tempo_bpm(&self) -> f64 {
        DEFAULT_TEMPO_BPM
    }
    /// `change_listener_par($NI_SIGNAL_TIMER_MS, ms)`: how often this slot's `on listener` runs.
    fn set_listener_period(&mut self, _milliseconds: f64) {}
    fn load_ir_sample(&mut self, _path: &str, _slot: i64, _generic: i64) -> bool {
        false
    }
    fn folder(&mut self, _kind: &str) -> String {
        String::new()
    }
    fn diagnostic(&mut self, _message: String) {}

    // Event callbacks. A script that never calls these leaves the engine's defaults alone.
    fn allow_group(&mut self, _group: i64) {}
    fn disallow_group(&mut self, _group: i64) {}
    fn play_note(
        &mut self,
        _note: i64,
        _velocity: i64,
        _offset: i64,
        _release: i64,
        _parent: i64,
    ) -> i64 {
        0
    }
    fn note_off_event(&mut self, _id: i64) {}
    /// `change_note`: retarget one event to another key.
    fn change_note(&mut self, _id: i64, _note: i64) {}
    fn ignore_event(&mut self) {}
    fn group_release_trigger(&mut self, _group: i64) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Event callbacks

/// What a script knows about a group when it runs `_get_engine_par`.
#[derive(Clone, Debug, Default)]
pub struct GroupInfo {
    pub release_trigger: bool,
    /// The group's amplitude envelope, so `$ENGINE_PAR_ATTACK`…`$ENGINE_PAR_RELEASE` can answer
    /// with the values a panel expects to display.
    pub envelope: Option<crate::envelope::Ahdsr>,
}

/// A sound-producing request from a script.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptAction {
    ChangeEvent {
        kind: String,
        id: i64,
        value: i64,
        relative: bool,
    },
    PlayNote {
        id: i64,
        parent: i64,
        note: i64,
        velocity: i64,
        offset: i64,
        release: i64,
    },
    NoteOff {
        id: i64,
    },
    /// `change_note`: the event plays, and keeps playing, at another key. KONTAKT re-points the
    /// event itself, so the *release* callback sees the new note too — which is how a script
    /// recognises "the key it played as 16" when the key comes up.
    ChangeNote {
        id: i64,
        note: i64,
    },
}

/// The result of running an event callback.
#[derive(Clone, Debug, Default)]
pub struct Dispatch {
    /// Per-group routing. `None` means the script never called `allow_group`/`disallow_group`, so
    /// the engine plays every group as usual.
    pub allowed_groups: Option<Vec<bool>>,
    pub actions: Vec<ScriptAction>,
    pub ignore_event: bool,
    /// pgs keys written while the callback ran; the host re-dispatches `on pgs_changed`.
    pub pgs_changed: Vec<String>,
}

/// The script-visible state of one MIDI event.
#[derive(Clone, Copy, Debug, Default)]
pub struct EventContext {
    pub note: i64,
    pub velocity: i64,
    pub id: i64,
    pub status: i64,
}

/// Engine-visible parameters shared with the audio thread.
///
/// Volumes are stored as atomics so the realtime thread can read them without a lock; everything
/// else is behind a mutex because only the GUI and background threads touch it.
pub struct EngineState {
    pub external_envelopes_enabled: bool,
    pub modulation: Vec<crate::modulation::GroupModulation>,
    diagnostics: Mutex<Vec<String>>,
    next_script_event: AtomicU64,
    random_state: AtomicU64,
    pending_ui: Mutex<Vec<Dispatch>>,
    bus_gain: [AtomicU32; 16],
    group_gain: Vec<AtomicU32>,
    group_pan: Vec<AtomicU32>,
    group_tune: Vec<AtomicU32>,
    params: Mutex<HashMap<String, i64>>,
    pub messages: Mutex<Vec<String>>,
    /// Last `load_ir_sample` request: the impulse response the script wants convolved.
    pub ir_request: Mutex<Option<String>>,
    /// Picture a script assigned to the instrument's own panel through `$INST_WALLPAPER_ID`.
    wallpaper_picture: Mutex<Option<String>>,
    /// Picture a script assigned to the instrument header icon through `$INST_ICON_ID`.
    icon_picture: Mutex<Option<String>>,
    /// Panel height a script asked for with `set_ui_height_px`.
    panel_height: Mutex<Option<f32>>,
    /// Per-slot panel height, so each page keeps its own.
    panel_heights: Mutex<HashMap<usize, f32>>,
    /// Per-slot `set_skin_offset` values.
    skin_offsets: Mutex<HashMap<usize, f32>>,
    /// Keyboard swatches, shared by every slot the way `set_key_color` is shared in KONTAKT:
    /// whichever script runs last decides what the key looks like.
    key_colors: Mutex<KeyColours>,
    /// Built-in `%CC[128]`. The host's MIDI input writes it, and so does a script's
    /// `set_controller`: panels use controller numbers as a bus between their slots.
    cc: Mutex<[i64; 128]>,
    /// Defaults belong to individual routes until a controller has actually been received.
    cc_seen: Mutex<[bool; 128]>,
    /// Which controllers a *script* has written. A panel that drives a parameter from its own
    /// timer is the only case where the player knows what a controller means, so the engine keys
    /// its voice filter on this rather than on the host's MIDI input.
    script_controllers: Mutex<[bool; 128]>,
    /// Host tempo, which `ms_to_ticks` and every listener timer are expressed against.
    tempo_bpm: Mutex<f64>,
    /// `change_listener_par($NI_SIGNAL_TIMER_MS, …)` per slot, in milliseconds.
    listener_period_ms: Mutex<HashMap<usize, f64>>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            external_envelopes_enabled: false,
            modulation: Vec::new(),
            diagnostics: Mutex::new(Vec::new()),
            next_script_event: AtomicU64::new(1 << 40),
            random_state: AtomicU64::new(0x9e3779b97f4a7c15),
            pending_ui: Mutex::new(Vec::new()),
            bus_gain: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
            group_gain: (0..16).map(|_| AtomicU32::new(1.0f32.to_bits())).collect(),
            group_pan: (0..16)
                .map(|_| AtomicU32::new(f32::NAN.to_bits()))
                .collect(),
            group_tune: (0..16)
                .map(|_| AtomicU32::new(f32::NAN.to_bits()))
                .collect(),
            params: Mutex::new(HashMap::new()),
            messages: Mutex::new(Vec::new()),
            ir_request: Mutex::new(None),
            wallpaper_picture: Mutex::new(None),
            icon_picture: Mutex::new(None),
            panel_height: Mutex::new(None),
            panel_heights: Mutex::new(HashMap::new()),
            skin_offsets: Mutex::new(HashMap::new()),
            key_colors: Mutex::new([None; 128]),
            cc: Mutex::new([0; 128]),
            cc_seen: Mutex::new([false; 128]),
            script_controllers: Mutex::new([false; 128]),
            tempo_bpm: Mutex::new(DEFAULT_TEMPO_BPM),
            listener_period_ms: Mutex::new(HashMap::new()),
        }
    }
}

impl EngineState {
    pub fn modulate_amp_envelope(&self, group: usize, mut envelope: crate::Ahdsr) -> crate::Ahdsr {
        if !self.external_envelopes_enabled {
            return envelope;
        }
        let Some(routes) = self.modulation.get(group) else {
            return envelope;
        };
        let Some(amp) = routes.amp_slot() else {
            return envelope;
        };
        let mut values = [
            crate::envelope::ms_to_normalized(envelope.attack_ms),
            crate::envelope::ms_to_normalized(envelope.decay_ms),
            envelope.sustain,
            crate::envelope::ms_to_normalized(envelope.release_ms),
        ];
        let mut changed = [false; 4];
        for source in &routes.modulators {
            let Some(input) = self.controller_source_value(&source.source) else {
                continue;
            };
            for (index, target) in source.targets.iter().enumerate() {
                if target.destination != Some(amp) {
                    continue;
                }
                let field = match target.parameter.as_str() {
                    "ahdsr_attack" => 0,
                    "ahdsr_decay" => 1,
                    "ahdsr_sustain" => 2,
                    "ahdsr_release" => 3,
                    _ => continue,
                };
                let amount = self
                    .get_cached(
                        "ENGINE_PAR_MOD_TARGET_INTENSITY",
                        group as i64,
                        source.handle,
                        index as i64,
                    )
                    .or_else(|| {
                        self.get_cached(
                            "ENGINE_PAR_MOD_TARGET_INTENSITY",
                            group as i64,
                            source.handle,
                            -1,
                        )
                    })
                    .map(|v| v as f32 / 1_000_000.0)
                    .unwrap_or(target.amount);
                values[field] += amount * target.value(input);
                changed[field] = true;
            }
        }
        if changed[0] {
            envelope.attack_ms = crate::envelope::normalized_to_ms(values[0]);
        }
        if changed[1] {
            envelope.decay_ms = crate::envelope::normalized_to_ms(values[1]);
        }
        if changed[2] {
            envelope.sustain = values[2].clamp(0.0, 1.0);
        }
        if changed[3] {
            envelope.release_ms = crate::envelope::normalized_to_ms(values[3]);
        }
        envelope
    }
    /// UI callbacks run outside the audio thread. Drain their sound-producing actions at a block
    /// boundary, retaining their group routing rather than dropping `play_note`/`note_off` calls.
    pub fn take_ui_dispatches(&self) -> Vec<Dispatch> {
        self.pending_ui
            .try_lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }
    pub fn bus_gain(&self, bus: usize) -> f32 {
        self.bus_gain
            .get(bus)
            .map(|value| f32::from_bits(value.load(Ordering::Relaxed)))
            .unwrap_or(1.0)
    }

    pub fn group_gain(&self, group: usize) -> f32 {
        self.group_gain
            .get(group)
            .map(|value| f32::from_bits(value.load(Ordering::Relaxed)))
            .unwrap_or(1.0)
    }

    pub fn group_pan(&self, group: usize) -> Option<f32> {
        self.group_pan
            .get(group)
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)))
            .filter(|v| v.is_finite())
    }

    pub fn group_tune(&self, group: usize) -> Option<f32> {
        self.group_tune
            .get(group)
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)))
            .filter(|v| v.is_finite())
    }

    fn set_bus_gain(&self, bus: usize, gain: f32) {
        if let Some(slot) = self.bus_gain.get(bus) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_group_gain(&self, group: usize, gain: f32) {
        if let Some(slot) = self.group_gain.get(group) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    /// The picture a script drew behind the whole instrument, if any.
    pub fn wallpaper_picture(&self) -> Option<String> {
        self.wallpaper_picture
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    /// The picture a script assigned to the instrument header icon, if any.
    pub fn icon_picture(&self) -> Option<String> {
        self.icon_picture
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    /// One MIDI controller value, as a script or the host last set it.
    pub fn controller(&self, number: usize) -> i64 {
        self.cc
            .lock()
            .ok()
            .and_then(|values| values.get(number).copied())
            .unwrap_or(0)
    }

    /// A route's saved default is independent of `%CC`, and -1 disables it until input arrives.
    pub fn controller_source_value(&self, source: &crate::modulation::Source) -> Option<f32> {
        let crate::modulation::Source::Controller {
            number,
            raw_assignment,
        } = source
        else {
            return None;
        };
        let index = *number as usize;
        if index >= 128 {
            return None;
        }
        let values = self.cc.lock().ok()?;
        let seen = self.cc_seen.lock().ok()?;
        if seen[index] {
            Some(values[index].clamp(0, 127) as f32 / 127.0)
        } else {
            let saved = i32::from_le_bytes(*raw_assignment);
            (0..=127).contains(&saved).then_some(saved as f32 / 127.0)
        }
    }

    fn set_controller(&self, number: i64, value: i64) {
        if (0..128).contains(&number) {
            if let Ok(mut values) = self.cc.lock() {
                values[number as usize] = value;
                if let Ok(mut seen) = self.cc_seen.lock() {
                    seen[number as usize] = true;
                }
            }
        }
    }

    /// `set_controller` called by a script, which also records that the script owns this
    /// controller.
    fn set_controller_from_script(&self, number: i64, value: i64) {
        self.set_controller(number, value);
        if (0..128).contains(&number) {
            if let Ok(mut written) = self.script_controllers.lock() {
                written[number as usize] = true;
            }
        }
    }

    /// Has a script written this controller? Only then does the player know what it controls.
    pub fn script_wrote_controller(&self, number: usize) -> bool {
        self.script_controllers
            .lock()
            .ok()
            .and_then(|written| written.get(number).copied())
            .unwrap_or(false)
    }

    fn controllers(&self) -> Vec<Value> {
        self.cc
            .lock()
            .map(|values| values.iter().map(|value| Value::Int(*value)).collect())
            .unwrap_or_else(|_| vec![Value::Int(0); 128])
    }

    /// Host tempo in BPM. 120 until the host reports one.
    pub fn tempo_bpm(&self) -> f64 {
        self.tempo_bpm
            .lock()
            .map(|tempo| *tempo)
            .unwrap_or(DEFAULT_TEMPO_BPM)
    }

    pub fn set_tempo_bpm(&self, tempo: f64) {
        if let Ok(mut current) = self.tempo_bpm.lock() {
            *current = tempo.clamp(1.0, 1000.0);
        }
    }

    /// The interval a slot asked for with `change_listener_par`, if it asked at all.
    pub fn listener_period(&self, slot: usize) -> Option<f64> {
        self.listener_period_ms
            .lock()
            .ok()
            .and_then(|map| map.get(&slot).copied())
    }

    fn set_listener_period(&self, slot: usize, milliseconds: f64) {
        if let Ok(mut map) = self.listener_period_ms.lock() {
            map.insert(slot, milliseconds.clamp(1.0, 10_000.0));
        }
    }

    /// Panel height a script asked for with `set_ui_height_px`.
    /// `set_skin_offset` of one script slot, in image pixels.
    pub fn skin_offset(&self, slot: usize) -> Option<f32> {
        self.skin_offsets
            .lock()
            .ok()
            .and_then(|map| map.get(&slot).copied())
    }

    /// `set_ui_height_px` of one script slot.
    pub fn panel_height_for(&self, slot: usize) -> Option<f32> {
        self.panel_heights
            .lock()
            .ok()
            .and_then(|map| map.get(&slot).copied())
    }

    pub fn panel_height(&self) -> Option<f32> {
        self.panel_height.lock().ok().and_then(|value| *value)
    }

    /// `set_key_color` for one key. Notes outside 0..127 are dropped: KONTAKT has no key there,
    /// and `$KEY_COLOR_NONE` clears the swatch instead of storing a colour.
    pub fn set_key_color(&self, note: i64, colour: i64) {
        if !(0..128).contains(&note) {
            return;
        }
        if let Ok(mut colours) = self.key_colors.lock() {
            colours[note as usize] = if colour == KEY_COLOR_NONE {
                None
            } else {
                Some(colour)
            };
        }
    }

    /// Every swatch a script has set so far.
    pub fn key_colors(&self) -> KeyColours {
        self.key_colors
            .lock()
            .map(|colours| *colours)
            .unwrap_or([None; 128])
    }

    fn key(param: &str, group: i64, slot: i64, generic: i64) -> String {
        format!("{param}|{group}|{slot}|{generic}")
    }

    /// Live value of an engine parameter, as last set by a script.
    pub fn parameter(&self, param: &str, group: i64, slot: i64, generic: i64) -> Option<i64> {
        self.get_cached(param, group, slot, generic)
    }

    /// Highest value cached for a parameter, whatever `slot`/`generic` the script addressed it by.
    ///
    /// Panels are inconsistent about those arguments: the same "reverb send" knob appears as
    /// `(-1, 7, 0)` in one library and `(-1, 0, 1)` in the next. Since this player has exactly one
    /// convolution return, the widest value is the right one to use.
    /// Every cached engine parameter, for diagnostics.
    pub fn cached_parameters(&self) -> Vec<(String, i64)> {
        self.params
            .lock()
            .map(|map| {
                map.iter()
                    .map(|(key, value)| (key.clone(), *value))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn parameter_any(&self, param: &str) -> Option<i64> {
        let map = self.params.lock().ok()?;
        let prefix = format!("{param}|");
        map.iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, value)| *value)
            .max()
    }

    pub fn get_cached(&self, param: &str, group: i64, slot: i64, generic: i64) -> Option<i64> {
        self.params
            .lock()
            .ok()
            .and_then(|map| map.get(&Self::key(param, group, slot, generic)).copied())
    }

    fn put_cached(&self, param: &str, group: i64, slot: i64, generic: i64, value: i64) {
        if let Ok(mut map) = self.params.lock() {
            map.insert(Self::key(param, group, slot, generic), value);
        }
    }
}

/// Default host: no instrument-specific targets except the 16 bus volumes, which is exactly what
/// KSP exposes through `$NI_BUS_OFFSET`.
pub struct EngineHost {
    pub state: Arc<EngineState>,
    /// Slot this host is running; per-page panel settings (height, skin offset) are stored
    /// against it.
    pub slot: usize,
    pub root: String,
    pub groups: Arc<Vec<GroupInfo>>,
    /// `None` until the script routes groups; otherwise one flag per group.
    pub allowed: Option<Vec<bool>>,
    pub actions: Vec<ScriptAction>,
    pub ignored: bool,
    pub pgs_changed: Vec<String>,
}

impl Host for EngineHost {
    fn find_mod(&mut self, group: i64, name: &str) -> i64 {
        let result = usize::try_from(group)
            .ok()
            .and_then(|g| self.state.modulation.get(g))
            .and_then(|g| g.find(name))
            .map(|m| m.handle);
        if let Some(handle) = result {
            return handle;
        }
        if let Ok(mut diagnostics) = self.state.diagnostics.lock() {
            let key = format!("KSP: modulator `{name}` is absent");
            if !diagnostics.iter().any(|d| d.starts_with(&key)) {
                diagnostics.push(format!("{key} in group {group}"));
            }
        }
        -1
    }
    fn find_target(&mut self, group: i64, modulator: i64, name: &str) -> i64 {
        let Some(module) = usize::try_from(group)
            .ok()
            .and_then(|g| self.state.modulation.get(g))
            .and_then(|g| g.by_handle(modulator))
        else {
            return -1;
        };
        module
            .targets
            .iter()
            .position(|t| t.name == name)
            .map(|i| i as i64)
            .unwrap_or(-1)
    }
    fn diagnostic(&mut self, message: String) {
        if let Ok(mut messages) = self.state.diagnostics.lock() {
            if !messages.contains(&message) {
                messages.push(message);
            }
        }
    }
    fn random(&mut self, low: i64, high: i64) -> i64 {
        let (low, high) = (low.min(high), low.max(high));
        let mut value = self
            .state
            .random_state
            .fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^= value >> 31;
        let width = high as i128 - low as i128 + 1;
        (low as i128 + (value as u128 % width as u128) as i128) as i64
    }

    fn change_event(&mut self, kind: &str, id: i64, value: i64, relative: bool) {
        self.actions.push(ScriptAction::ChangeEvent {
            kind: kind.to_string(),
            id,
            value,
            relative,
        });
    }
    fn set_controller(&mut self, number: i64, value: i64) {
        self.state.set_controller_from_script(number, value);
    }

    fn tempo_bpm(&self) -> f64 {
        self.state.tempo_bpm()
    }

    fn set_listener_period(&mut self, milliseconds: f64) {
        self.state.set_listener_period(self.slot, milliseconds);
    }

    fn engine_par(&mut self, param: &str, group: i64, slot: i64, generic: i64) -> Option<i64> {
        if let Some(value) = self.state.get_cached(param, group, slot, generic) {
            return Some(value);
        }
        if param == "ENGINE_PAR_MOD_TARGET_INTENSITY" {
            let module = self
                .state
                .modulation
                .get(usize::try_from(group).ok()?)?
                .by_handle(slot)?;
            return module
                .targets
                .get(if generic < 0 { 0 } else { generic as usize })
                .map(|t| (t.amount * 1_000_000.0).round() as i64);
        }
        if param == "ENGINE_PAR_RELEASE_TRIGGER" && group >= 0 {
            return Some(
                self.groups
                    .get(group as usize)
                    .map(|info| info.release_trigger as i64)
                    .unwrap_or(0),
            );
        }
        if group >= 0 {
            let envelope = self
                .groups
                .get(group as usize)
                .and_then(|info| info.envelope)
                .or_else(|| self.groups.first().and_then(|info| info.envelope));
            if let Some(envelope) = envelope {
                let value = match param {
                    "ENGINE_PAR_ATTACK" => crate::envelope::ms_to_normalized(envelope.attack_ms),
                    "ENGINE_PAR_DECAY" => crate::envelope::ms_to_normalized(envelope.decay_ms),
                    "ENGINE_PAR_RELEASE" => crate::envelope::ms_to_normalized(envelope.release_ms),
                    "ENGINE_PAR_SUSTAIN" => envelope.sustain.clamp(0.0, 1.0),
                    _ => return None,
                };
                return Some((value as f64 * 1_000_000.0).round() as i64);
            }
        }
        // Engine defaults for the handful of targets the panels read at init.
        let default = match param {
            "ENGINE_PAR_VOLUME" if generic >= 1024 => 1_000_000,
            "ENGINE_PAR_VOLUME" => 1_000_000,
            "ENGINE_PAR_PAN" => 500_000,
            "ENGINE_PAR_CUTOFF" | "ENGINE_PAR_RESONANCE" => 500_000,
            "ENGINE_PAR_ATTACK" => 0,
            "ENGINE_PAR_SENDLEVEL_0"
            | "ENGINE_PAR_SENDLEVEL_1"
            | "ENGINE_PAR_SENDLEVEL_2"
            | "ENGINE_PAR_SENDLEVEL_3"
            | "ENGINE_PAR_SENDLEVEL_4"
            | "ENGINE_PAR_SENDLEVEL_5"
            | "ENGINE_PAR_SENDLEVEL_6"
            | "ENGINE_PAR_SENDLEVEL_7" => 0,
            "ENGINE_PAR_IRC_PREDELAY" => 0,
            "ENGINE_PAR_IRC_LENGTH_RATIO_LR" | "ENGINE_PAR_IRC_LENGTH_RATIO_ER" => 1_000_000,
            _ => return None,
        };
        Some(default)
    }

    fn set_engine_par(&mut self, param: &str, value: i64, group: i64, slot: i64, generic: i64) {
        if param == "ENGINE_PAR_MOD_TARGET_INTENSITY" && slot < 0 {
            return;
        }
        self.state.put_cached(param, group, slot, generic, value);
        match param {
            "ENGINE_PAR_PAN" if group >= 0 && generic < 1024 && slot <= 0 => {
                if let Some(target) = self.state.group_pan.get(group as usize) {
                    let pan = (value as f32 / 500_000.0 - 1.0).clamp(-1.0, 1.0);
                    target.store(pan.to_bits(), Ordering::Relaxed);
                }
            }
            "ENGINE_PAR_TUNE" if group >= 0 && generic < 1024 && slot <= 0 => {
                if let Some(target) = self.state.group_tune.get(group as usize) {
                    let tune =
                        2f32.powf((value.clamp(0, 1_000_000) as f32 / 1_000_000.0 - 0.5) * 6.0);
                    target.store(tune.to_bits(), Ordering::Relaxed);
                }
            }
            "ENGINE_PAR_VOLUME" => {
                let gain = volume_to_gain(value);
                if generic >= 1024 {
                    self.state.set_bus_gain((generic - 1024) as usize, gain);
                } else if group >= 0 {
                    self.state.set_group_gain(group as usize, gain);
                }
            }
            _ => {}
        }
    }

    fn engine_par_disp(&mut self, param: &str, group: i64, slot: i64, generic: i64) -> String {
        let value = self.engine_par(param, group, slot, generic).unwrap_or(0);
        match param {
            "ENGINE_PAR_VOLUME" => {
                let gain = value as f32 / 1_000_000.0;
                if gain <= 0.000_01 {
                    "-inf dB".to_string()
                } else {
                    format!("{:.1} dB", 20.0 * gain.log10())
                }
            }
            name if name.starts_with("ENGINE_PAR_SENDLEVEL") => {
                let gain = value as f32 / 1_000_000.0;
                if gain <= 0.000_01 {
                    "-inf dB".to_string()
                } else {
                    format!("{:.1} dB", 20.0 * gain.log10())
                }
            }
            "ENGINE_PAR_PAN" => {
                let pan = (value as f32 / 500_000.0) - 1.0;
                if pan.abs() < 0.005 {
                    "C".to_string()
                } else if pan < 0.0 {
                    format!("{:.0} L", -pan * 100.0)
                } else {
                    format!("{:.0} R", pan * 100.0)
                }
            }
            "ENGINE_PAR_CUTOFF" => format!(
                "{:.0} Hz",
                20.0 * (1000.0f32).powf(value as f32 / 1_000_000.0)
            ),
            "ENGINE_PAR_ATTACK" | "ENGINE_PAR_DECAY" | "ENGINE_PAR_HOLD" | "ENGINE_PAR_RELEASE" => {
                let ms = crate::envelope::normalized_to_ms(value as f32 / 1_000_000.0);
                if ms < 1.0 {
                    format!("{ms:.2} ms")
                } else {
                    format!("{ms:.1} ms")
                }
            }
            "ENGINE_PAR_SUSTAIN" => {
                let sustain = (value as f32 / 1_000_000.0).max(1e-5);
                format!("{:.1} dB", 20.0 * sustain.log10())
            }
            "ENGINE_PAR_IRC_PREDELAY" => {
                format!("{:.2} ms", value as f32 / 1_000_000.0 * 200.0)
            }
            "ENGINE_PAR_IRC_LENGTH_RATIO_LR" | "ENGINE_PAR_IRC_LENGTH_RATIO_ER" => {
                format!("{:.1} %", value as f32 / 10_000.0)
            }
            "ENGINE_PAR_RESONANCE" => format!("{:.1} %", value as f32 / 10_000.0),
            _ => value.to_string(),
        }
    }

    fn pgs_set(&mut self, key: &str, index: i64, value: i64) {
        let slot = format!("PGS|{key}");
        let previous = self.state.get_cached(&slot, 0, 0, index);
        self.state.put_cached(&slot, 0, 0, index, value);
        if previous != Some(value) && !self.pgs_changed.iter().any(|changed| changed == key) {
            self.pgs_changed.push(key.to_string());
        }
    }

    fn pgs_get(&mut self, key: &str, index: i64) -> i64 {
        self.state
            .get_cached(&format!("PGS|{key}"), 0, 0, index)
            .unwrap_or(0)
    }

    fn pgs_exists(&mut self, key: &str) -> bool {
        self.state
            .get_cached(&format!("PGS|{key}"), 0, 0, -1)
            .is_some()
    }

    fn pgs_create(&mut self, key: &str, size: i64) {
        self.state.put_cached(&format!("PGS|{key}"), 0, 0, -1, size);
    }

    fn message(&mut self, text: &str) {
        if let Ok(mut messages) = self.state.messages.lock() {
            messages.push(text.to_string());
        }
    }

    fn instance_par_str(&mut self, id: i64, par: &str, text: &str) {
        if par != "CONTROL_PAR_PICTURE" {
            return;
        }
        let slot = match id {
            INST_WALLPAPER_ID => &self.state.wallpaper_picture,
            INST_ICON_ID => &self.state.icon_picture,
            _ => return,
        };
        if let Ok(mut value) = slot.lock() {
            *value = Some(text.to_string());
        }
    }

    fn panel_height(&mut self, height: f32) {
        // Each script states its own performance-view height; the panel is as tall as the tallest
        // one. Keeping the last value let a one-row utility script shrink a 536 px panel to 23.
        if let Ok(mut value) = self.state.panel_height.lock() {
            *value = Some(value.map_or(height, |current| current.max(height)));
        }
        if let Ok(mut heights) = self.state.panel_heights.lock() {
            heights.insert(self.slot, height);
        }
    }

    fn skin_offset(&mut self, offset: f32) {
        if let Ok(mut offsets) = self.state.skin_offsets.lock() {
            offsets.insert(self.slot, offset);
        }
    }

    fn set_key_color(&mut self, note: i64, colour: i64) {
        self.state.set_key_color(note, colour);
    }

    fn load_ir_sample(&mut self, path: &str, _slot: i64, _generic: i64) -> bool {
        self.state
            .put_cached("IR_SAMPLE", 0, 0, 0, if path.is_empty() { 0 } else { 1 });
        if let Ok(mut request) = self.state.ir_request.lock() {
            *request = Some(path.to_string());
        }
        if let Ok(mut messages) = self.state.messages.lock() {
            messages.push(format!("load_ir_sample({path})"));
        }
        true
    }

    fn folder(&mut self, kind: &str) -> String {
        // The factory directory is the installed Kontakt's shared content, not the library's own
        // folder; scripts concatenate it with `presets/Effects/Convolution/...` to reach an IR.
        if kind == "GET_FOLDER_FACTORY_DIR" {
            if let Some(root) = crate::resolve::factory_content_root() {
                // No trailing slash: the scripts append "/presets/…" themselves.
                return root.display().to_string();
            }
        }
        if self.root.is_empty() || self.root.ends_with('/') {
            self.root.clone()
        } else {
            format!("{}/", self.root)
        }
    }

    fn allow_group(&mut self, group: i64) {
        let count = self.groups.len();
        let mask = self.allowed.get_or_insert_with(|| vec![true; count]);
        if group < 0 {
            mask.fill(true);
        } else if let Some(slot) = mask.get_mut(group as usize) {
            *slot = true;
        }
    }

    fn disallow_group(&mut self, group: i64) {
        let count = self.groups.len();
        let mask = self.allowed.get_or_insert_with(|| vec![true; count]);
        if group < 0 {
            mask.fill(false);
        } else if let Some(slot) = mask.get_mut(group as usize) {
            *slot = false;
        }
    }

    fn play_note(
        &mut self,
        note: i64,
        velocity: i64,
        offset: i64,
        release: i64,
        parent: i64,
    ) -> i64 {
        let id = self.state.next_script_event.fetch_add(1, Ordering::Relaxed) as i64;
        self.actions.push(ScriptAction::PlayNote {
            id,
            parent,
            note,
            velocity,
            offset,
            release,
        });
        id
    }

    fn note_off_event(&mut self, id: i64) {
        self.actions.push(ScriptAction::NoteOff { id });
    }

    fn change_note(&mut self, id: i64, note: i64) {
        self.actions.push(ScriptAction::ChangeNote { id, note });
    }

    fn ignore_event(&mut self) {
        self.ignored = true;
    }

    fn group_release_trigger(&mut self, group: i64) -> bool {
        self.groups
            .get(group as usize)
            .map(|info| info.release_trigger)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------------------------
// Interpreter

const STEP_LIMIT: u64 = 2_000_000;
const DEPTH_LIMIT: usize = 64;

struct Interp<'a> {
    program: &'a Program,
    vars: Vars,
    ui: Vec<UiControl>,
    host: &'a mut dyn Host,
    steps: u64,
    depth: usize,
    exit: bool,
    /// Virtual script clock in milliseconds; `wait` advances it.
    timer_ms: u64,
}

impl<'a> Interp<'a> {
    fn new(program: &'a Program, host: &'a mut dyn Host) -> Self {
        Self {
            program,
            vars: Vars::default(),
            ui: Vec::new(),
            host,
            steps: 0,
            depth: 0,
            exit: false,
            timer_ms: 0,
        }
    }

    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err("script exceeded its instruction budget".into());
        }
        Ok(())
    }

    fn run(&mut self, body: &[Stmt]) -> Result<(), String> {
        for stmt in body {
            self.tick()?;
            if self.exit {
                return Ok(());
            }
            self.stmt(stmt)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Declare {
                kind,
                name,
                polyphonic,
                init,
                array_init,
                len,
            } => {
                let len = len
                    .as_ref()
                    .map(|expr| self.eval(expr).map(|v| v.as_int().max(0) as usize))
                    .transpose()?;
                self.vars.declare(*kind, name, len);
                if *polyphonic {
                    self.vars.polyphonic.insert(name.clone());
                }
                if let Some(init) = init {
                    let value = self.eval(init)?;
                    self.vars.set(name, value);
                }
                // `declare %x[N] := (…)` fills the elements; the declaration comes first so the
                // array exists before the values are written into it.
                if let Some(values) = array_init {
                    for (index, expr) in values.iter().enumerate() {
                        let value = self.eval(expr)?;
                        self.vars.set_index(name, index as i64, value);
                    }
                }
            }
            Stmt::DeclareUi(decl) => {
                let args: Vec<i64> = decl
                    .args
                    .iter()
                    .map(|expr| self.eval(expr).map(|value| value.as_int()).unwrap_or(0))
                    .collect();
                let len = decl.len.as_ref().map(|expr| {
                    self.eval(expr)
                        .map(|value| value.as_int().max(0) as usize)
                        .unwrap_or(0)
                });
                let control = UiControl::from_decl(decl, &args, len);
                let name = control.name.clone();
                // The control's own name is bare, but a `ui_table` is bound to an array whose
                // name keeps its `%`, and that is the key the script writes to. Declaring it
                // under the stripped name left every reference pointing at a different slot.
                let variable = decl.name.clone();
                if self.vars.get(&variable) == Value::Int(0)
                    && !self.vars.vars.contains_key(&variable)
                {
                    self.vars.declare(VarKind::Int, &variable, len);
                }
                if let Some(existing) = self.ui.iter_mut().find(|c| c.name == name) {
                    *existing = control;
                } else {
                    self.ui.push(control);
                }
            }
            Stmt::Assign(target, value) => {
                let value = self.eval(value)?;
                match target {
                    Expr::Var(_, name) => {
                        // A `ui_*` control's value *is* its variable: assigning it moves the
                        // widget without firing `on ui_control`. Panels rely on this to link
                        // two controls (a slider and its handle both write each other).
                        if let Some(control) = self.ui.iter_mut().find(|c| c.name == bare(name)) {
                            control.value = value.as_int().clamp(control.min, control.max);
                        }
                        self.vars.set(name, value);
                    }
                    Expr::Index(base, index) => {
                        let index = self.eval(index)?.as_int();
                        if let Expr::Var(_, name) = base.as_ref() {
                            self.vars.set_index(name, index, value);
                        }
                    }
                    _ => {}
                }
            }
            Stmt::Expr(expr) => {
                self.eval(expr)?;
            }
            Stmt::If { arms, otherwise } => {
                for (cond, body) in arms {
                    if self.eval(cond)?.truthy() {
                        return self.run(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.run(body);
                }
            }
            Stmt::While { cond, body } => {
                while self.eval(cond)?.truthy() {
                    self.run(body)?;
                    if self.exit {
                        break;
                    }
                }
            }
            Stmt::Select {
                subject,
                cases,
                otherwise,
            } => {
                let value = self.eval(subject)?;
                for (case, body) in cases {
                    if self.eval(case)? == value {
                        return self.run(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.run(body);
                }
            }
            Stmt::Exit => self.exit = true,
        }
        Ok(())
    }

    fn eval(&mut self, expr: &Expr) -> Result<Value, String> {
        self.tick()?;
        Ok(match expr {
            Expr::Int(value) => Value::Int(*value),
            Expr::Real(value) => Value::Real(*value),
            Expr::Str(value) => Value::Str(value.clone()),
            Expr::Var(kind, name) => match symbol(name) {
                Some(Value::Int(value)) => Value::Int(value),
                // The script clock. Panels read it to hold a readout for a moment and then clear
                // it: `while ($KSP_TIMER < $deadline) wait(2000) end while`.
                _ if name == "KSP_TIMER" => Value::Int(self.timer_ms as i64),
                _ => match kind {
                    VarKind::Str => Value::Str(self.vars.get(name).as_str()),
                    _ => self.vars.get(name),
                },
            },
            Expr::Index(base, index) => {
                let index = self.eval(index)?.as_int();
                match base.as_ref() {
                    Expr::Var(_, name) => self.vars.get_index(name, index),
                    _ => Value::Int(0),
                }
            }
            Expr::Unary(op, inner) => {
                let value = self.eval(inner)?;
                match op {
                    UnOp::Neg => match value {
                        Value::Real(v) => Value::Real(-v),
                        other => Value::Int(-other.as_int()),
                    },
                    UnOp::Not => Value::Int(!value.truthy() as i64),
                }
            }
            Expr::Binary(op, left, right) => {
                // `and` / `or` short-circuit; the rest evaluate both sides.
                match op {
                    BinOp::And => {
                        let left = self.eval(left)?;
                        if !left.truthy() {
                            return Ok(Value::Int(0));
                        }
                        Value::Int(self.eval(right)?.truthy() as i64)
                    }
                    BinOp::Or => {
                        let left = self.eval(left)?;
                        if left.truthy() {
                            return Ok(Value::Int(1));
                        }
                        Value::Int(self.eval(right)?.truthy() as i64)
                    }
                    _ => {
                        let left = self.eval(left)?;
                        let right = self.eval(right)?;
                        binary(*op, left, right)
                    }
                }
            }
            Expr::Call(name, args) => self.call(name, args)?,
        })
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        let lower = name.to_ascii_lowercase();

        // User-defined functions take precedence.
        if let Some(function) = self.program.functions.get(name).cloned() {
            if self.depth >= DEPTH_LIMIT {
                return Err("call depth exceeded".into());
            }
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(self.eval(arg)?);
            }
            // Only the parameters are local. Restoring the whole store (as this once did) threw
            // away every global the function had just assigned — a retail panel's `all_off()`
            // helper zeroes three globals, and all three were being silently reverted.
            let names: Vec<String> = function
                .params
                .iter()
                .map(|(_, name)| name.clone())
                .collect();
            let saved: Vec<Option<Variable>> = names
                .iter()
                .map(|name| self.vars.vars.get(name).cloned())
                .collect();
            for ((kind, param), value) in function.params.iter().zip(values) {
                self.vars.declare(*kind, param, None);
                self.vars.set(param, value);
            }
            self.depth += 1;
            let result = self.run(&function.body);
            self.depth -= 1;
            for (name, previous) in names.iter().zip(saved) {
                match previous {
                    Some(value) => {
                        self.vars.vars.insert(name.clone(), value);
                    }
                    None => {
                        self.vars.vars.remove(name);
                    }
                }
            }
            return result
                .map(|_| Value::Int(0))
                .map_err(|e| format!("{name}: {e}"));
        }

        let symbol_arg = |index: usize| -> Option<String> {
            args.get(index).and_then(|expr| match expr {
                Expr::Var(_, name) => Some(name.clone()),
                _ => None,
            })
        };

        match lower.as_str() {
            "random" => {
                let low = self.eval_arg(args, 0)?;
                let high = self.eval_arg(args, 1)?;
                Ok(Value::Int(self.host.random(low, high)))
            }
            "change_vol" | "change_tune" | "change_pan" => {
                let id = self.eval_arg(args, 0)?;
                let value = self.eval_arg(args, 1)?;
                let relative = self.eval_arg(args, 2)? != 0;
                self.host.change_event(&lower, id, value, relative);
                Ok(Value::Int(0))
            }
            "inc" | "dec" => {
                let step = if lower == "inc" { 1 } else { -1 };
                if let Some(Expr::Var(_, name)) = args.first() {
                    let value = self.vars.get(name).as_int() + step;
                    if let Some(control) = self.ui.iter_mut().find(|c| c.name == bare(name)) {
                        control.value = value.clamp(control.min, control.max);
                    }
                    self.vars.set(name, Value::Int(value));
                }
                Ok(Value::Int(0))
            }
            "set_engine_par" => {
                let param = symbol_arg(0).unwrap_or_default();
                let value = self.eval_arg(args, 1)?;
                let group = self.eval_arg(args, 2)?;
                let slot = self.eval_arg(args, 3)?;
                let generic = self.eval_arg(args, 4)?;
                self.host
                    .set_engine_par(&param, value, group, slot, generic);
                Ok(Value::Int(0))
            }
            "find_mod" => {
                let group = self.eval_arg(args, 0)?;
                let name = self.eval_arg_str(args, 1)?;
                Ok(Value::Int(self.host.find_mod(group, &name)))
            }
            "find_target" => {
                let group = self.eval_arg(args, 0)?;
                let module = self.eval_arg(args, 1)?;
                let name = self.eval_arg_str(args, 2)?;
                Ok(Value::Int(self.host.find_target(group, module, &name)))
            }
            "get_engine_par" => {
                let param = symbol_arg(0).unwrap_or_default();
                let group = self.eval_arg(args, 1)?;
                let slot = self.eval_arg(args, 2)?;
                let generic = self.eval_arg(args, 3)?;
                Ok(Value::Int(
                    self.host
                        .engine_par(&param, group, slot, generic)
                        .unwrap_or(0),
                ))
            }
            "get_engine_par_disp" => {
                let param = symbol_arg(0).unwrap_or_default();
                let group = self.eval_arg(args, 1)?;
                let slot = self.eval_arg(args, 2)?;
                let generic = self.eval_arg(args, 3)?;
                Ok(Value::Str(
                    self.host.engine_par_disp(&param, group, slot, generic),
                ))
            }
            "set_knob_label" => {
                if let Some(name) = symbol_arg(0) {
                    let text = self.eval_arg_str(args, 1)?;
                    if let Some(control) = self.control_mut(&name) {
                        control.value_text = Some(text);
                    }
                }
                Ok(Value::Int(0))
            }
            "set_text" => {
                if let Some(name) = symbol_arg(0) {
                    let text = self.eval_arg_str(args, 1)?;
                    if let Some(control) = self.control_mut(&name) {
                        control.label = text;
                    }
                }
                Ok(Value::Int(0))
            }
            "set_knob_unit" => {
                if let Some(name) = symbol_arg(0) {
                    let unit = match symbol_arg(1).as_deref() {
                        Some("KNOB_UNIT_HZ") => "Hz",
                        Some("KNOB_UNIT_DB") => "dB",
                        Some("KNOB_UNIT_MS") => "ms",
                        Some("KNOB_UNIT_PERCENT") => "%",
                        Some("KNOB_UNIT_ST") => "st",
                        Some("KNOB_UNIT_OCT") => "oct",
                        _ => "",
                    };
                    if let Some(control) = self.control_mut(&name) {
                        control.unit = (!unit.is_empty()).then(|| unit.to_string());
                    }
                }
                Ok(Value::Int(0))
            }
            "set_knob_defval" => {
                if let Some(name) = symbol_arg(0) {
                    let value = self.eval_arg(args, 1)?;
                    if let Some(control) = self.control_mut(&name) {
                        if control.max > control.min || control.value == 0 {
                            control.value = value;
                        }
                    }
                }
                Ok(Value::Int(0))
            }
            "move_control" | "move_control_px" => {
                let pixels = lower == "move_control_px";
                if let Some(name) = symbol_arg(0) {
                    let x = self.eval_arg(args, 1)? as i32;
                    let y = self.eval_arg(args, 2)? as i32;
                    if let Some(control) = self.control_mut(&name) {
                        control.x = x;
                        control.y = y;
                        control.px = pixels;
                    }
                }
                Ok(Value::Int(0))
            }
            "make_persistent" => {
                if let Some(name) = symbol_arg(0) {
                    if let Some(control) = self.control_mut(&name) {
                        control.persistent = true;
                    }
                }
                Ok(Value::Int(0))
            }
            "get_ui_id" => {
                let name = symbol_arg(0).unwrap_or_default();
                let id = self
                    .ui
                    .iter()
                    .position(|control| control.name == bare(&name))
                    .map(|index| index as i64)
                    .unwrap_or(-1);
                Ok(Value::Int(id))
            }
            "get_ui_idx" => {
                let name = symbol_arg(0).unwrap_or_default();
                Ok(Value::Int(
                    self.ui
                        .iter()
                        .position(|control| control.name == bare(&name))
                        .map(|index| index as i64)
                        .unwrap_or(-1),
                ))
            }
            "get_control_par" => {
                let id = self.eval_arg(args, 0)? as usize;
                let par = symbol_arg(1).unwrap_or_default();
                let value = match (self.ui.get(id), par.as_str()) {
                    // A menu's value *is* the selected row. Subtracting one here made every
                    // selection one row too low, and the first row fall out of a `select` that
                    // starts at `case 0`.
                    (Some(control), "CONTROL_PAR_SELECTED_ITEM_IDX") => {
                        control.value.clamp(control.min, control.max)
                    }
                    (Some(control), "CONTROL_PAR_VALUE") => control.value,
                    (Some(control), "CONTROL_PAR_MIN") => control.min,
                    (Some(control), "CONTROL_PAR_MAX") => control.max,
                    (Some(control), "CONTROL_PAR_POS_X") => control.x as i64,
                    (Some(control), "CONTROL_PAR_POS_Y") => control.y as i64,
                    _ => 0,
                };
                Ok(Value::Int(value))
            }
            "set_control_par" => {
                let id = self.control_arg(args)?;
                let par = symbol_arg(1).unwrap_or_default();
                let value = self.eval_arg(args, 2)?;
                if let Some(id) = id.filter(|id| *id >= INST_ID_BASE as usize) {
                    self.host.instance_par(id as i64, &par, value);
                    return Ok(Value::Int(0));
                }
                if let Some(control) = id.and_then(|id| self.ui.get_mut(id)) {
                    match par.as_str() {
                        "CONTROL_PAR_DEFAULT_VALUE" => {
                            if control.value == control.min {
                                control.value = value;
                            }
                        }
                        "CONTROL_PAR_VALUE" => control.value = value,
                        // `set_control_par(..., $CONTROL_PAR_POS_X, n)` takes pixels, exactly
                        // like `move_control_px`, so it also switches the control off the grid.
                        "CONTROL_PAR_POS_X" => {
                            control.x = value as i32;
                            control.px = true;
                        }
                        "CONTROL_PAR_POS_Y" => {
                            control.y = value as i32;
                            control.px = true;
                        }
                        "CONTROL_PAR_MOUSE_BEHAVIOUR" => control.mouse_behaviour = value as i32,
                        // Panels page themselves by hiding whole controls, so this has to feed
                        // the same state `hide_part` writes; ignoring it draws every page at once.
                        "CONTROL_PAR_HIDE" => control.hidden_parts = value as u32,
                        "CONTROL_PAR_WIDTH" => control.pixel_width = Some(value as i32),
                        "CONTROL_PAR_HEIGHT" => control.pixel_height = Some(value as i32),
                        "CONTROL_PAR_BAR_COLOR" => control.bar_color = Some(value),
                        _ => {}
                    }
                }
                Ok(Value::Int(0))
            }
            "set_control_par_str" => {
                let id = self.control_arg(args)?;
                let par = symbol_arg(1).unwrap_or_default();
                let text = self.eval_arg_str(args, 2)?;
                if let Some(id) = id.filter(|id| *id >= INST_ID_BASE as usize) {
                    self.host.instance_par_str(id as i64, &par, &text);
                    return Ok(Value::Int(0));
                }
                if let Some(control) = id.and_then(|id| self.ui.get_mut(id)) {
                    match par.as_str() {
                        "CONTROL_PAR_TEXT" => control.label = text,
                        "CONTROL_PAR_PICTURE" => control.picture = Some(text),
                        _ => {}
                    }
                }
                Ok(Value::Int(0))
            }
            "set_menu_item_str" => {
                let id = self.control_arg(args)?;
                let row = self.eval_arg(args, 1)?;
                let text = self.eval_arg_str(args, 2)?;
                if let Some(item) = id.and_then(|id| self.ui.get_mut(id)).and_then(|control| {
                    usize::try_from(row)
                        .ok()
                        .and_then(|row| control.menu.get_mut(row))
                }) {
                    *item = text;
                }
                Ok(Value::Int(0))
            }
            "add_menu_item" => {
                let name = symbol_arg(0).unwrap_or_default();
                let text = self.eval_arg_str(args, 1)?;
                let value = self.eval_arg(args, 2)?;
                if let Some(control) = self.control_mut(&name) {
                    // The third argument is the value a menu stores when the row is picked; retail
                    // scripts pass the row index. Growing the range here is what makes a
                    // `declare ui_menu` control selectable at all — it is declared with no
                    // arguments, so its range starts at 0..0 and every selection clamps to 0.
                    let _ = value;
                    control.menu.push(text);
                    control.min = 0;
                    control.max = control.menu.len() as i64 - 1;
                }
                Ok(Value::Int(0))
            }
            "allow_group" => {
                let group = self.eval_arg(args, 0)?;
                self.host.allow_group(group);
                Ok(Value::Int(0))
            }
            "disallow_group" => {
                let group = self.eval_arg(args, 0)?;
                self.host.disallow_group(group);
                Ok(Value::Int(0))
            }
            "play_note" => {
                let note = self.eval_arg(args, 0)?;
                let velocity = self.eval_arg(args, 1)?;
                let offset = self.eval_arg(args, 2)?;
                let release = self.eval_arg(args, 3)?;
                let parent = self.vars.get("EVENT_ID").as_int();
                Ok(Value::Int(
                    self.host.play_note(note, velocity, offset, release, parent),
                ))
            }
            "note_off" => {
                let id = self.eval_arg(args, 0)?;
                self.host.note_off_event(id);
                Ok(Value::Int(0))
            }
            // A script writing a controller value is indistinguishable from an incoming CC to
            // everything else: it lands in the shared table, so another slot — and the engine —
            // sees what an external controller would have set.
            "set_controller" => {
                let number = self.eval_arg(args, 0)?;
                let value = self.eval_arg(args, 1)?;
                self.host.set_controller(number, value);
                if (0..128).contains(&number) {
                    self.vars
                        .set_index("%CC", number, Value::Int(value.clamp(0, 127)));
                }
                Ok(Value::Int(0))
            }
            "change_listener_par" => {
                // Only `$NI_SIGNAL_TIMER_MS` is honoured; a beat clock would need the host
                // transport, which a player driven by a note stream does not have.
                let _signal = self.eval_arg(args, 0)?;
                let value = self.eval_arg(args, 1)?;
                self.host.set_listener_period(value as f64);
                Ok(Value::Int(0))
            }
            "ms_to_ticks" => {
                let milliseconds = self.eval_arg(args, 0)? as f64;
                let ms_per_quarter = 60_000.0 / self.host.tempo_bpm().max(1.0);
                Ok(Value::Int(
                    (milliseconds / ms_per_quarter * TICKS_PER_QUARTER).round() as i64,
                ))
            }
            "change_note" => {
                let id = self.eval_arg(args, 0)?;
                let note = self.eval_arg(args, 1)?;
                self.host.change_note(id, note);
                Ok(Value::Int(0))
            }
            "ignore_event" => {
                self.host.ignore_event();
                Ok(Value::Int(0))
            }
            "hide_part" => {
                let id = self.control_arg(args)?;
                let part = symbol_arg(1).unwrap_or_default().to_ascii_uppercase();
                let bit = if part.contains("WHOLE_CONTROL") {
                    16
                } else if part.contains("PART_BG") {
                    1
                } else if part.contains("CURSOR") {
                    2
                } else if part.contains("PART_VALUE") {
                    4
                } else if part.contains("PART_TITLE") {
                    8
                } else {
                    0
                };
                if let Some(control) = id.and_then(|id| self.ui.get_mut(id)) {
                    control.hidden_parts |= bit;
                }
                Ok(Value::Int(0))
            }
            "set_skin_offset" => {
                let offset = self.eval_arg(args, 0)? as f32;
                self.host.skin_offset(offset);
                Ok(Value::Int(0))
            }
            "set_key_color" => {
                let note = self.eval_arg(args, 0)?;
                let colour = self.eval_arg(args, 1)?;
                self.host.set_key_color(note, colour);
                Ok(Value::Int(0))
            }
            "set_ui_height_px" => {
                let height = self.eval_arg(args, 0)? as f32;
                self.host.panel_height(height);
                Ok(Value::Int(0))
            }
            // The grid form: one row is 21 px tall, and the panel starts two pixels down.
            "set_ui_height" => {
                let rows = self.eval_arg(args, 0)? as f32;
                self.host.panel_height(rows * 21.0 + 2.0);
                Ok(Value::Int(0))
            }
            "set_ui_width_px"
            | "make_perfview"
            | "watch_var"
            | "read_persistent_var"
            | "_read_persistent_var"
            | "set_listener"
            | "fade_out"
            | "set_event_par_arr"
            | "message"
            | "exit" => {
                if lower == "message" {
                    let text = self.eval_arg_str(args, 0)?;
                    self.host.message(&text);
                }
                Ok(Value::Int(0))
            }
            // Suspending the script is how panels hold a readout on screen for a moment. The
            // interpreter has no thread of its own here, so the clock advances instead of the
            // world: the loop terminates, the handler finishes, and the readout clears at once.
            "wait" => {
                let ms = self.eval_arg(args, 0)?.max(0) as u64;
                self.timer_ms = self.timer_ms.saturating_add(ms.max(1));
                Ok(Value::Int(0))
            }
            "load_ir_sample" | "_load_ir_sample" => {
                let path = self.eval_arg_str(args, 0)?;
                let slot = self.eval_arg(args, 1)?;
                let generic = self.eval_arg(args, 2)?;
                self.host.load_ir_sample(&path, slot, generic);
                Ok(Value::Int(0))
            }
            // `_get_folder` is the builtin spelling; both forms appear in retail scripts.
            "get_folder" | "_get_folder" => {
                let kind = symbol_arg(0).unwrap_or_default();
                Ok(Value::Str(self.host.folder(&kind)))
            }
            "pgs_create_key" => {
                let key = self.eval_pgs_key(args)?;
                let size = self.eval_arg(args, 1)?;
                self.host.pgs_create(&key, size);
                Ok(Value::Int(0))
            }
            "pgs_set_key_val" => {
                let key = self.eval_pgs_key(args)?;
                let index = self.eval_arg(args, 1)?;
                let value = self.eval_arg(args, 2)?;
                self.host.pgs_set(&key, index, value);
                Ok(Value::Int(0))
            }
            "pgs_get_key_val" => {
                let key = self.eval_pgs_key(args)?;
                let index = self.eval_arg(args, 1)?;
                Ok(Value::Int(self.host.pgs_get(&key, index)))
            }
            "pgs_key_exists" => {
                let key = self.eval_pgs_key(args)?;
                Ok(Value::Int(self.host.pgs_exists(&key) as i64))
            }
            "event_status" => Ok(Value::Int(0)),
            "_get_engine_par" => {
                let param = symbol_arg(0).unwrap_or_default();
                let group = self.eval_arg(args, 1)?;
                let slot = self.eval_arg(args, 2)?;
                let generic = self.eval_arg(args, 3)?;
                Ok(Value::Int(
                    self.host
                        .engine_par(&param, group, slot, generic)
                        .unwrap_or(0),
                ))
            }
            "_reset_rls_trig_counter" => Ok(Value::Int(0)),
            // `search(array, value)` is the only way a script asks "is this in the list, and
            // where". Returning 0 for it (as the unknown-function fallback did) makes a routing
            // test like `search(%groups, $i) = -1` false for every group, which silences the
            // whole instrument.
            "search" => {
                let name = match args.first() {
                    // An array's name keeps its `%` (that is how it is stored), so this must not
                    // go through `bare()`.
                    Some(Expr::Var(_, name)) => name.clone(),
                    _ => return Ok(Value::Int(-1)),
                };
                let value = self.eval_arg(args, 1)?;
                let found = match self.vars.vars.get(&name) {
                    Some(Variable::Array(values)) => values
                        .iter()
                        .position(|entry| entry.as_int() == value)
                        .map(|index| index as i64)
                        .unwrap_or(-1),
                    _ => -1,
                };
                Ok(Value::Int(found))
            }
            // `num_elements(array)` is how a script walks a group list. Without it every
            // `while ($a < num_elements(%groups))` loop runs zero times, so a script that pairs
            // `disallow_group($ALL_GROUPS)` with a loop of `allow_group(...)` allows nothing and
            // the instrument plays silently. one piano library is exactly that shape.
            "num_elements" => {
                let name = match args.first() {
                    // An array's name keeps its `%`, as with `search`.
                    Some(Expr::Var(_, name)) => name.clone(),
                    _ => return Ok(Value::Int(0)),
                };
                let count = match self.vars.vars.get(&name) {
                    Some(Variable::Array(values)) => values.len() as i64,
                    _ => 0,
                };
                Ok(Value::Int(count))
            }
            other => {
                self.host
                    .diagnostic(format!("KSP: unsupported function `{other}` ignored"));
                Ok(Value::Int(0))
            }
        }
    }

    /// Resolve a control argument that is either a literal `get_ui_id` result or a control name.
    fn control_arg(&mut self, args: &[Expr]) -> Result<Option<usize>, String> {
        if let Some(name) = args.first().and_then(|expr| match expr {
            Expr::Var(_, name) => Some(name.clone()),
            _ => None,
        }) {
            if let Some(index) = self.ui.iter().position(|c| c.name == bare(&name)) {
                return Ok(Some(index));
            }
        }
        let id = self.eval_arg(args, 0)?;
        if id < 0 {
            return Ok(None);
        }
        Ok(Some(id as usize))
    }

    fn eval_arg(&mut self, args: &[Expr], index: usize) -> Result<i64, String> {
        match args.get(index) {
            Some(expr) => Ok(self.eval(expr)?.as_int()),
            None => Ok(0),
        }
    }

    /// PGS keys are symbolic names. The parser represents a bare name as a zero-argument call;
    /// evaluating that unknown call as a number aliases every key to the string "0".
    fn eval_pgs_key(&mut self, args: &[Expr]) -> Result<String, String> {
        match args.first() {
            Some(Expr::Call(name, arguments)) if arguments.is_empty() => Ok(name.clone()),
            _ => self.eval_arg_str(args, 0),
        }
    }

    fn eval_arg_str(&mut self, args: &[Expr], index: usize) -> Result<String, String> {
        match args.get(index) {
            Some(expr) => Ok(self.eval(expr)?.as_str()),
            None => Ok(String::new()),
        }
    }

    fn control_mut(&mut self, name: &str) -> Option<&mut UiControl> {
        let name = bare(name);
        self.ui.iter_mut().find(|control| control.name == name)
    }
}

fn binary(op: BinOp, left: Value, right: Value) -> Value {
    if matches!(op, BinOp::Concat) {
        return Value::Str(format!("{}{}", left.as_str(), right.as_str()));
    }
    let promote_real = matches!(left, Value::Real(_)) || matches!(right, Value::Real(_));
    match op {
        BinOp::Add if promote_real => Value::Real(left.as_real() + right.as_real()),
        BinOp::Sub if promote_real => Value::Real(left.as_real() - right.as_real()),
        BinOp::Mul if promote_real => Value::Real(left.as_real() * right.as_real()),
        BinOp::Div if promote_real => Value::Real(left.as_real() / right.as_real()),
        BinOp::Add => Value::Int(left.as_int() + right.as_int()),
        BinOp::Sub => Value::Int(left.as_int() - right.as_int()),
        BinOp::Mul => Value::Int(left.as_int() * right.as_int()),
        BinOp::Div => {
            let divisor = right.as_int();
            Value::Int(if divisor == 0 {
                0
            } else {
                left.as_int() / divisor
            })
        }
        BinOp::Mod => {
            let divisor = right.as_int();
            Value::Int(if divisor == 0 {
                0
            } else {
                left.as_int() % divisor
            })
        }
        BinOp::Eq => Value::Int((left == right) as i64),
        BinOp::Ne => Value::Int((left != right) as i64),
        BinOp::Lt => Value::Int((left.as_real() < right.as_real()) as i64),
        BinOp::Le => Value::Int((left.as_real() <= right.as_real()) as i64),
        BinOp::Gt => Value::Int((left.as_real() > right.as_real()) as i64),
        BinOp::Ge => Value::Int((left.as_real() >= right.as_real()) as i64),
        BinOp::And => Value::Int((left.truthy() && right.truthy()) as i64),
        BinOp::Or => Value::Int((left.truthy() || right.truthy()) as i64),
        BinOp::Concat => unreachable!(),
    }
}

/// KSP's volume controller uses `630000` as unity gain, `0` as silence and `1_000_000` as the top
/// of the range. Between those points it is linear in decibels: -60 dB at 0, 0 dB at 630 000 and
/// +12 dB at 1 000 000. Library panels rely on this — one library's saved "stereo pair" is 630 000 and its
/// other microphones default to 0.
fn volume_to_gain(value: i64) -> f32 {
    let value = value.clamp(0, 1_000_000) as f32 / 1_000_000.0;
    // Alternative law for controlled differential renders; retain the established default until
    // host level, saved mixer state and parameter mapping can be measured independently.
    if std::env::var("OPENKONTAKT_VOLUME_LAW").as_deref() == Ok("linear") {
        return value;
    }
    if value <= 0.0 {
        return 0.0;
    }
    let db = if value <= 0.63 {
        -60.0 * (1.0 - value / 0.63)
    } else {
        12.0 * (value - 0.63) / 0.37
    };
    10f32.powf(db / 20.0)
}

/// Handles a script can hand to `set_control_par` to address the instrument's own chrome.
/// They are deliberately outside the `ui_*` index range so nothing can alias a real control.
pub const INST_ID_BASE: i64 = 1_000_000;
pub const INST_WALLPAPER_ID: i64 = INST_ID_BASE;
pub const INST_ICON_ID: i64 = INST_ID_BASE + 1;

/// Bit in `$CONTROL_PAR_HIDE` that removes a control from the panel entirely.
pub const HIDE_WHOLE_CONTROL: i64 = 16;

/// KONTAKT's internal resolution, in ticks per quarter note, as `ms_to_ticks` counts them.
///
/// A timer script divides `ms_to_ticks(…)` by 960 and then inverts the result to get a period in
/// milliseconds, so the constant only becomes visible through the rate those panels actually run
/// at. It falls out exactly: the script's `60000000 / $Tempo` arithmetic lands on
/// `640 * ms_per_quarter / ticks`, and one library's panel takes eight seconds for its 32 steps at
/// 120 BPM — 250 ms per step, which is an eighth note, and 250 ms is what 1280 ticks gives. With
/// 960 the same table takes 10.7 s and with 2880 it takes 3.6 s.
pub const TICKS_PER_QUARTER: f64 = 1280.0;

/// Tempo assumed before the host reports one.
pub const DEFAULT_TEMPO_BPM: f64 = 120.0;

/// How often `on listener` runs before the script has asked for its own interval. Timer scripts
/// set one on their first tick; one that never asks keeps this. [open]
pub const DEFAULT_LISTENER_MS: f64 = 10.0;

/// `set_key_color` swatches, in KONTAKT's own order: `$KEY_COLOR_DEFAULT` is 0 and every later
/// name is one higher. The order is the order the constants are listed in NI's reference; the
/// names are what scripts pass, so this table is also how `$KEY_COLOR_*` is resolved.
pub const KEY_COLOR_NAMES: [&str; 18] = [
    "DEFAULT",
    "RED",
    "ORANGE",
    "LIGHT_ORANGE",
    "WARM_YELLOW",
    "YELLOW",
    "LIME",
    "GREEN",
    "MINT",
    "CYAN",
    "TURQUOISE",
    "BLUE",
    "PLUM",
    "VIOLET",
    "PURPLE",
    "MAGENTA",
    "FUCHSIA",
    "INACTIVE",
];

/// `$KEY_COLOR_DEFAULT`: the blue KONTAKT paints every mapped key with.
pub const KEY_COLOR_DEFAULT: i64 = 0;
/// `$KEY_COLOR_INACTIVE`: paint the key as if nothing were mapped to it. Drum kits use this to
/// hide everything outside their playing range, which is the "key range" the keyboard shows.
pub const KEY_COLOR_INACTIVE: i64 = KEY_COLOR_NAMES.len() as i64 - 1;

/// The three neutral colours: `set_key_color($n, $KEY_COLOR_NONE)` hands the key back to the
/// zone mapping, and `$KEY_COLOR_BLACK`/`$KEY_COLOR_WHITE` force the unplayed look of a black or
/// a white key, which is how libraries draw a keyboard bed over ranges they do not map.
///
/// They are numbered above the swatch list because they name the *absence* of a hue. KONTAKT's
/// own numbers for them are not published; every retail script passes the name, never the value.
pub const KEY_COLOR_NEUTRAL_BASE: i64 = KEY_COLOR_NAMES.len() as i64;
pub const KEY_COLOR_NONE: i64 = KEY_COLOR_NEUTRAL_BASE;
pub const KEY_COLOR_BLACK: i64 = KEY_COLOR_NEUTRAL_BASE + 1;
pub const KEY_COLOR_WHITE: i64 = KEY_COLOR_NEUTRAL_BASE + 2;

/// The `$KEY_COLOR_*` name behind a stored swatch, for diagnostics and tooling.
pub fn key_color_name(index: i64) -> &'static str {
    match index {
        KEY_COLOR_NONE => "NONE",
        KEY_COLOR_BLACK => "BLACK",
        KEY_COLOR_WHITE => "WHITE",
        other => KEY_COLOR_NAMES
            .get(other as usize)
            .copied()
            .unwrap_or("UNKNOWN"),
    }
}

/// What a script said about each key's keyboard colour. `None` is "the script never touched this
/// key", which is not the same as `$KEY_COLOR_DEFAULT`: an untouched key keeps the default only
/// if something is mapped to it.
pub type KeyColours = [Option<i64>; 128];

/// KSP constants that behave like integers. `$ENGINE_PAR_*` and `$CONTROL_PAR_*` are handled
/// symbolically at their call sites, so only the arithmetic constants need values here.
fn symbol(name: &str) -> Option<Value> {
    // `$KEY_COLOR_*` is a numbered swatch list rather than a hand-written table, so the number
    // and the name stay in one place.
    if let Some(swatch) = name.strip_prefix("KEY_COLOR_") {
        if let Some(index) = KEY_COLOR_NAMES.iter().position(|known| *known == swatch) {
            return Some(Value::Int(index as i64));
        }
        match swatch {
            "NONE" => return Some(Value::Int(KEY_COLOR_NONE)),
            "BLACK" => return Some(Value::Int(KEY_COLOR_BLACK)),
            "WHITE" => return Some(Value::Int(KEY_COLOR_WHITE)),
            _ => {}
        }
    }
    Some(Value::Int(match name {
        "NI_BUS_OFFSET" => 1024,
        "KNOB_UNIT_DB" => 0,
        "KNOB_UNIT_HZ" => 1,
        "KNOB_UNIT_MS" => 2,
        "KNOB_UNIT_PERCENT" => 3,
        "KNOB_UNIT_ST" => 4,
        "KNOB_UNIT_OCT" => 5,
        "EVENT_PAR_VOLUME" => 0,
        "EVENT_PAR_PAN" => 1,
        "EVENT_PAR_TUNE" => 2,
        "EVENT_PAR_POSITION" => 3,
        "ALL_GROUPS" => -1,
        "NI_SIGNAL_TIMER_MS" => 0,
        "NI_SIGNAL_TIMER_BEAT" => 1,
        "EVENT_STATUS_INACTIVE" => 0,
        "EVENT_STATUS_ACTIVE" => 1,
        "EVENT_STATUS_STOLEN" => 2,
        "GET_FOLDER_PATCH_DIR" => 0,
        "GET_FOLDER_LIBRARY_DIR" => 1,
        "GET_FOLDER_FACTORY_DIR" => 2,
        "INST_WALLPAPER_ID" => INST_WALLPAPER_ID,
        "INST_ICON_ID" => INST_ICON_ID,
        "INST_HEADER_ID" => INST_ID_BASE + 2,
        "INST_PERF_VIEW_ID" => INST_ID_BASE + 3,
        "INST_KEYBOARD_ID" => INST_ID_BASE + 4,
        "INST_OUTPUT_ID" => INST_ID_BASE + 5,
        "INST_MAIN_VIEW_ID" => INST_ID_BASE + 6,
        // `$CONTROL_PAR_HIDE` bits. Only the whole-control bit changes layout; the individual
        // parts (background, title, value, modulation light) are cosmetic.
        "HIDE_PART_NOTHING" => 0,
        "HIDE_PART_BG" => 1,
        "HIDE_PART_TITLE" => 2,
        "HIDE_PART_VALUE" => 4,
        "HIDE_PART_MOD_LIGHT" => 8,
        "HIDE_WHOLE_CONTROL" => HIDE_WHOLE_CONTROL,
        _ => return None,
    }))
}

// ---------------------------------------------------------------------------------------------
// Public runtime

#[derive(Default)]
struct SlotState {
    program: Option<Arc<Program>>,
    vars: Vars,
    ui: Vec<UiControl>,
    diagnostics: Vec<String>,
    initialized: bool,
}

/// Per-slot script state plus the engine parameters the scripts drive.
pub struct KspRuntime {
    restoring: AtomicBool,
    deferred_ui: Mutex<Vec<Dispatch>>,
    slots: Vec<Mutex<SlotState>>,
    pub engine: Arc<EngineState>,
    root: String,
    groups: Arc<Vec<GroupInfo>>,
}

impl KspRuntime {
    pub fn compile_instrument(
        texts: &[Option<String>],
        root: &str,
        groups: &[crate::group::Group],
    ) -> Self {
        let info = groups
            .iter()
            .map(|g| GroupInfo {
                release_trigger: g.release_trigger,
                envelope: crate::envelope::group_amp_envelope(g),
            })
            .collect();
        let mut runtime = Self::compile_with_groups(texts, root, info);
        let state = Arc::get_mut(&mut runtime.engine).expect("new runtime owns engine state");
        state.external_envelopes_enabled =
            std::env::var("OPENKONTAKT_EXTERNAL_ENVELOPES").as_deref() == Ok("1");
        state.modulation = groups
            .iter()
            .map(crate::modulation::group_modulation)
            .collect();
        for group in &state.modulation {
            if let Ok(mut diagnostics) = state.diagnostics.lock() {
                diagnostics.extend(group.diagnostics.iter().cloned());
            }
        }
        runtime
    }
    /// Compile every slot. Slots that fail to parse are recorded and skipped.
    pub fn compile(texts: &[Option<String>], root: &str) -> Self {
        Self::compile_with_groups(texts, root, Vec::new())
    }

    /// Compile with the instrument's group table, so `_get_engine_par` can answer group
    /// parameters such as `$ENGINE_PAR_RELEASE_TRIGGER`.
    pub fn compile_with_groups(
        texts: &[Option<String>],
        root: &str,
        groups: Vec<GroupInfo>,
    ) -> Self {
        let mut slots = Vec::with_capacity(texts.len());
        for text in texts {
            let mut state = SlotState::default();
            match text {
                Some(text) if !text.trim().is_empty() => match parser::parse(text) {
                    Ok(program) => state.program = Some(Arc::new(program)),
                    Err(error) => state.diagnostics.push(format!("KSP parse: {error}")),
                },
                _ => {}
            }
            slots.push(Mutex::new(state));
        }
        Self {
            slots,
            restoring: AtomicBool::new(false),
            deferred_ui: Mutex::new(Vec::new()),
            engine: Arc::new(EngineState {
                group_gain: (0..groups.len().max(16))
                    .map(|_| AtomicU32::new(1.0f32.to_bits()))
                    .collect(),
                group_pan: (0..groups.len().max(16))
                    .map(|_| AtomicU32::new(f32::NAN.to_bits()))
                    .collect(),
                group_tune: (0..groups.len().max(16))
                    .map(|_| AtomicU32::new(f32::NAN.to_bits()))
                    .collect(),
                ..EngineState::default()
            }),
            root: root.to_string(),
            groups: Arc::new(groups),
        }
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Run `on init` for every slot.
    pub fn initialize(&self) {
        for index in 0..self.slots.len() {
            self.run_init(index);
        }
    }

    /// Restore all slots before delivering cross-slot notifications. A receiver must not react to
    /// the saved UI with its unrestored init defaults (which may enable a long-running callback).
    pub fn apply_persistent_batch(&self, slots: &[(usize, Vec<(String, Vec<i64>)>)]) {
        self.restoring.store(true, Ordering::Relaxed);
        for (slot, entries) in slots {
            self.apply_persistent(*slot, entries);
        }
        self.restoring.store(false, Ordering::Relaxed);
        let pending = self
            .deferred_ui
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default();
        for dispatch in pending {
            self.finish_ui_dispatch(dispatch);
        }
    }

    fn run_init(&self, index: usize) {
        let Ok(mut state) = self.slots[index].lock() else {
            return;
        };
        let Some(program) = state.program.clone() else {
            return;
        };
        let mut host = EngineHost {
            slot: index,
            state: Arc::clone(&self.engine),
            root: self.root.clone(),
            groups: Arc::clone(&self.groups),
            allowed: None,
            actions: Vec::new(),
            ignored: false,
            pgs_changed: Vec::new(),
        };
        let mut interp = Interp::new(&program, &mut host);
        self.install_builtins(&mut interp);
        for callback in program.callbacks.iter().filter(|c| c.kind == "init") {
            if let Err(error) = interp.run(&callback.body) {
                state.diagnostics.push(format!("KSP on init: {error}"));
                break;
            }
        }
        state.vars = interp.vars;
        state.ui = interp.ui;
        state.initialized = true;
    }

    /// Restore saved values, then re-run each affected `on ui_control` so the engine mirrors them.
    pub fn apply_persistent(&self, index: usize, entries: &[(String, Vec<i64>)]) {
        let Ok(mut state) = self.slots[index].lock() else {
            return;
        };
        let Some(program) = state.program.clone() else {
            return;
        };
        let mut changed = Vec::new();
        for (name, values) in entries {
            let Some(first) = values.first().copied() else {
                continue;
            };
            let bare = name.trim_start_matches('$');
            if let Some(control) = state.ui.iter_mut().find(|c| c.name == bare) {
                control.value = first;
                // A knob or a switch owns engine state, so its handler is replayed. A button is
                // normally a click rather than a restore — but a button stored as *pressed* is
                // the saved selection of a radio group, and its handler is what lays out the page
                // that selection belongs to: one panel only moves its step table into view from
                // the tab button's handler, so skipping every button left the table parked at
                // (0,0) and the panel empty until the user clicked the tab.
                // Replaying only the pressed buttons also keeps the other panel's three direction
                // buttons from all firing at once, which used to leave the last one selected.
                if control.kind != UiKind::Button || first != 0 {
                    changed.push(bare.to_string());
                }
            }
            let is_array = values.len() > 1;
            if is_array {
                for (n, value) in values.iter().enumerate() {
                    state.vars.set_index(bare, n as i64, Value::Int(*value));
                }
            } else {
                state.vars.set(bare, Value::Int(first));
            }
        }
        if changed.is_empty() {
            return;
        }
        let mut host = EngineHost {
            slot: index,
            state: Arc::clone(&self.engine),
            root: self.root.clone(),
            groups: Arc::clone(&self.groups),
            allowed: None,
            actions: Vec::new(),
            ignored: false,
            pgs_changed: Vec::new(),
        };
        let mut interp = Interp::new(&program, &mut host);
        interp.vars = std::mem::take(&mut state.vars);
        interp.ui = std::mem::take(&mut state.ui);
        self.install_builtins(&mut interp);
        for name in &changed {
            let callbacks: Vec<Callback> = program
                .callbacks
                .iter()
                .filter(|c| {
                    c.kind == "ui_control" && c.arg.as_deref().map(bare) == Some(bare(name))
                })
                .cloned()
                .collect();
            for callback in callbacks {
                // The budget guards a single handler against a runaway loop, so it has to start
                // from zero for each one. Sharing the counter across every persisted control made
                // a panel with a few hundred of them fail a third of the way through, with the
                // error attributed to whichever handler happened to cross the line.
                interp.steps = 0;
                if let Err(error) = interp.run(&callback.body) {
                    state
                        .diagnostics
                        .push(format!("KSP on ui_control({name}): {error}"));
                }
            }
        }
        // Re-running the handlers is how the engine mirrors saved knob positions, but a handler
        // can also assign other saved variables as a side effect: one library's articulation selectors
        // set `$Current_Art`, and the last one to run wins, which left the instrument on
        // articulation 24 instead of the saved 1 and playing nothing. Put the saved variables back.
        for (name, values) in entries {
            let Some(first) = values.first().copied() else {
                continue;
            };
            let bare = name.trim_start_matches('$');
            if values.len() > 1 {
                for (n, value) in values.iter().enumerate() {
                    interp.vars.set_index(bare, n as i64, Value::Int(*value));
                }
            } else {
                interp.vars.set(bare, Value::Int(first));
            }
        }
        state.vars = interp.vars;
        state.ui = interp.ui;
        drop(state);
        self.finish_ui_callback(host);
    }

    /// A control moved in the GUI: store the value and run its `on ui_control` callback.
    pub fn set_control_normalized(&self, index: usize, control: &str, normalized: f32) -> bool {
        let value = {
            let Some(slot) = self.slots.get(index) else {
                return false;
            };
            let Ok(mut state) = slot.lock() else {
                return false;
            };
            let Some(ui) = state.ui.iter_mut().find(|c| c.name == bare(control)) else {
                return false;
            };
            ui.set_normalized(normalized);
            ui.value
        };
        self.set_control_value(index, control, value)
    }

    fn finish_ui_callback(&self, host: EngineHost) {
        let result = Dispatch {
            allowed_groups: host.allowed,
            actions: host.actions,
            ignore_event: host.ignored,
            pgs_changed: host.pgs_changed,
        };
        if self.restoring.load(Ordering::Relaxed) {
            if let Ok(mut pending) = self.deferred_ui.lock() {
                pending.push(result);
            }
            return;
        }
        self.finish_ui_dispatch(result);
    }

    fn finish_ui_dispatch(&self, mut result: Dispatch) {
        for key in std::mem::take(&mut result.pgs_changed) {
            self.dispatch_pgs_changed(&key, 0, &mut result);
        }
        if !result.actions.is_empty() {
            if let Ok(mut pending) = self.engine.pending_ui.lock() {
                pending.push(result);
            }
        }
    }

    fn cc_snapshot(&self) -> Vec<Value> {
        self.engine.controllers()
    }

    fn install_builtins(&self, interp: &mut Interp<'_>) {
        interp.vars.set_array("%CC", self.cc_snapshot());
        interp
            .vars
            .set("NUM_GROUPS", Value::Int(self.groups.len() as i64));
    }

    /// Feed a MIDI controller event to the scripts.
    pub fn controller(&self, number: i64, value: i64) -> Dispatch {
        self.engine.set_controller(number, value);
        self.dispatch_with("controller", EventContext::default(), Some((number, value)))
    }

    /// Run one slot's `on listener` callbacks.
    ///
    /// Each script has its own listener timer, so unlike `dispatch` this reaches exactly one slot.
    /// `None` means the slot has no listener at all, which is what lets the engine skip it.
    pub fn listener(&self, index: usize) -> Option<Dispatch> {
        let mut result = Dispatch::default();
        {
            let mut state = self.slots.get(index)?.lock().ok()?;
            let program = state.program.clone()?;
            if !state.initialized {
                return None;
            }
            let callbacks: Vec<Callback> = program
                .callbacks
                .iter()
                .filter(|callback| callback.kind == "listener")
                .cloned()
                .collect();
            if callbacks.is_empty() {
                return None;
            }
            let mut host = EngineHost {
                slot: index,
                state: Arc::clone(&self.engine),
                root: self.root.clone(),
                groups: Arc::clone(&self.groups),
                allowed: None,
                actions: Vec::new(),
                ignored: false,
                pgs_changed: Vec::new(),
            };
            let mut interp = Interp::new(&program, &mut host);
            interp.vars = std::mem::take(&mut state.vars);
            interp.ui = std::mem::take(&mut state.ui);
            self.install_builtins(&mut interp);
            for callback in &callbacks {
                interp.steps = 0;
                if let Err(error) = interp.run(&callback.body) {
                    state.diagnostics.push(format!("KSP on listener: {error}"));
                    break;
                }
            }
            state.vars = interp.vars;
            state.ui = interp.ui;
            result.allowed_groups = host.allowed.take();
            result.actions = std::mem::take(&mut host.actions);
            result.ignore_event = host.ignored;
            result.pgs_changed = std::mem::take(&mut host.pgs_changed);
        }
        // Outside the lock: a pgs write wakes the other scripts, this one included.
        if !result.pgs_changed.is_empty() {
            let changed = std::mem::take(&mut result.pgs_changed);
            for key in changed {
                self.dispatch_pgs_changed(&key, 0, &mut result);
            }
        }
        Some(result)
    }

    /// One script variable, for diagnostics. `$` or `%` is optional; an array reads element zero.
    pub fn variable(&self, index: usize, name: &str) -> Option<i64> {
        let state = self.slots.get(index)?.lock().ok()?;
        Some(state.vars.get(bare(name)).as_int())
    }

    pub fn array_variable(&self, index: usize, name: &str) -> Option<Vec<i64>> {
        let state = self.slots.get(index)?.lock().ok()?;
        match state.vars.vars.get(name)? {
            Variable::Array(values) => Some(values.iter().map(Value::as_int).collect()),
            _ => None,
        }
    }

    /// How often a slot's `on listener` should run: the interval the script asked for, or the
    /// default while it has not asked yet — the timer sets its own interval from inside the
    /// callback, so the first tick cannot wait for it.
    pub fn listener_period_ms(&self, index: usize) -> Option<f64> {
        if let Some(period) = self.engine.listener_period(index) {
            return Some(period);
        }
        let has_listener = self
            .slots
            .get(index)
            .and_then(|slot| slot.lock().ok())
            .map(|state| {
                state
                    .program
                    .as_ref()
                    .map(|program| {
                        program
                            .callbacks
                            .iter()
                            .any(|callback| callback.kind == "listener")
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        has_listener.then_some(DEFAULT_LISTENER_MS)
    }

    /// Run `on note` or `on release` for every slot and collect what the scripts asked for.
    ///
    /// Group routing is intersected across slots: a slot that never calls `allow_group` does not
    /// override one that does.
    pub fn dispatch(&self, kind: &str, event: EventContext) -> Dispatch {
        self.dispatch_with(kind, event, None)
    }

    fn dispatch_with(
        &self,
        kind: &str,
        event: EventContext,
        controller: Option<(i64, i64)>,
    ) -> Dispatch {
        let mut result = Dispatch::default();
        let mut routing: Option<Vec<bool>> = None;

        for slot in 0..self.slots.len() {
            let Ok(mut state) = self.slots[slot].lock() else {
                continue;
            };
            let Some(program) = state.program.clone() else {
                continue;
            };
            if !state.initialized {
                continue;
            }
            let callbacks: Vec<Callback> = program
                .callbacks
                .iter()
                .filter(|c| c.kind == kind)
                .cloned()
                .collect();
            if callbacks.is_empty() {
                if kind == "release" {
                    state.vars.note_vars.remove(&event.id);
                }
                continue;
            }

            let mut host = EngineHost {
                slot: slot,
                state: Arc::clone(&self.engine),
                root: self.root.clone(),
                groups: Arc::clone(&self.groups),
                allowed: None,
                actions: Vec::new(),
                ignored: false,
                pgs_changed: Vec::new(),
            };
            let mut interp = Interp::new(&program, &mut host);
            interp.vars = std::mem::take(&mut state.vars);
            interp.ui = std::mem::take(&mut state.ui);
            self.install_builtins(&mut interp);
            interp.vars.set("EVENT_NOTE", Value::Int(event.note));
            interp
                .vars
                .set("EVENT_VELOCITY", Value::Int(event.velocity));
            interp.vars.set("EVENT_ID", Value::Int(event.id));
            interp.vars.set("EVENT_STATUS", Value::Int(event.status));
            interp.vars.note_context = matches!(kind, "note" | "release").then_some(event.id);
            if kind == "note" {
                interp.vars.note_vars.remove(&event.id);
            }
            if let Some((number, value)) = controller {
                interp.vars.set("CC_NUM", Value::Int(number));
                interp.vars.set("CC_VALUE", Value::Int(value));
                interp.vars.set("MIDI_BYTE_1", Value::Int(number));
                interp.vars.set("MIDI_BYTE_2", Value::Int(value));
            }

            for callback in &callbacks {
                if let Err(error) = interp.run(&callback.body) {
                    state.diagnostics.push(format!("KSP on {kind}: {error}"));
                    break;
                }
            }
            interp.vars.note_context = None;
            if kind == "release" {
                interp.vars.note_vars.remove(&event.id);
            }
            state.vars = interp.vars;
            state.ui = interp.ui;

            if let Some(mask) = host.allowed {
                routing = Some(match routing {
                    Some(existing) => existing
                        .iter()
                        .zip(mask.iter())
                        .map(|(a, b)| *a && *b)
                        .collect(),
                    None => mask,
                });
            }
            result.actions.extend(host.actions);
            result.ignore_event |= host.ignored;
            result.pgs_changed.extend(host.pgs_changed);
        }

        result.allowed_groups = routing;

        // A script that wrote a pgs key wakes the other scripts' `on pgs_changed`, which is how
        // panels coordinate state across slots. Bounded, because a callback may write again.
        if !result.pgs_changed.is_empty() {
            let changed = std::mem::take(&mut result.pgs_changed);
            for key in changed {
                self.dispatch_pgs_changed(&key, 0, &mut result);
            }
        }
        result
    }

    fn dispatch_pgs_changed(&self, key: &str, depth: usize, result: &mut Dispatch) {
        if depth >= 4 {
            return;
        }
        let mut rechanged: Vec<String> = Vec::new();
        for slot in 0..self.slots.len() {
            let Ok(mut state) = self.slots[slot].lock() else {
                continue;
            };
            let Some(program) = state.program.clone() else {
                continue;
            };
            if !state.initialized {
                continue;
            }
            let callbacks: Vec<Callback> = program
                .callbacks
                .iter()
                .filter(|c| c.kind == "pgs_changed")
                .cloned()
                .collect();
            if callbacks.is_empty() {
                continue;
            }
            let mut host = EngineHost {
                slot: slot,
                state: Arc::clone(&self.engine),
                root: self.root.clone(),
                groups: Arc::clone(&self.groups),
                allowed: None,
                actions: Vec::new(),
                ignored: false,
                pgs_changed: Vec::new(),
            };
            let mut interp = Interp::new(&program, &mut host);
            interp.vars = std::mem::take(&mut state.vars);
            interp.ui = std::mem::take(&mut state.ui);
            self.install_builtins(&mut interp);
            interp.vars.set("PGS_KEY", Value::Str(key.to_string()));
            for callback in &callbacks {
                if let Err(error) = interp.run(&callback.body) {
                    state
                        .diagnostics
                        .push(format!("KSP slot {slot} on pgs_changed({key}): {error}"));
                    break;
                }
            }
            state.vars = interp.vars;
            state.ui = interp.ui;
            result.actions.extend(host.actions);
            result.ignore_event |= host.ignored;
            rechanged.extend(host.pgs_changed);
        }
        for next in rechanged {
            self.dispatch_pgs_changed(&next, depth + 1, result);
        }
    }

    /// Set a control to a raw value (menus use item indices, not normalised values) and run its
    /// `on ui_control` callback.
    pub fn set_control_value(&self, index: usize, control: &str, value: i64) -> bool {
        let Ok(mut state) = self.slots[index].lock() else {
            return false;
        };
        let Some(program) = state.program.clone() else {
            return false;
        };
        let Some(ui) = state.ui.iter_mut().find(|c| c.name == bare(control)) else {
            return false;
        };
        ui.value = value.clamp(ui.min, ui.max);
        let stored = ui.value;
        state.vars.set(bare(control), Value::Int(stored));

        let callbacks: Vec<Callback> = program
            .callbacks
            .iter()
            .filter(|c| c.kind == "ui_control" && c.arg.as_deref().map(bare) == Some(bare(control)))
            .cloned()
            .collect();
        let mut host = EngineHost {
            slot: index,
            state: Arc::clone(&self.engine),
            root: self.root.clone(),
            groups: Arc::clone(&self.groups),
            allowed: None,
            actions: Vec::new(),
            ignored: false,
            pgs_changed: Vec::new(),
        };
        let mut interp = Interp::new(&program, &mut host);
        interp.vars = std::mem::take(&mut state.vars);
        interp.ui = std::mem::take(&mut state.ui);
        self.install_builtins(&mut interp);
        let mut ok = true;
        for callback in callbacks {
            if let Err(error) = interp.run(&callback.body) {
                state
                    .diagnostics
                    .push(format!("KSP on ui_control({control}): {error}"));
                ok = false;
            }
        }
        state.vars = interp.vars;
        state.ui = interp.ui;
        drop(state);
        self.finish_ui_callback(host);
        ok
    }

    /// Edit one step of a `ui_table`. KONTAKT writes the value straight into the array the table
    /// is bound to, so the script sees it on its next pass.
    pub fn set_table_element(
        &self,
        index: usize,
        control: &str,
        element: usize,
        value: i64,
    ) -> bool {
        let Ok(mut state) = self.slots[index].lock() else {
            return false;
        };
        let Some(ui) = state.ui.iter_mut().find(|c| c.name == bare(control)) else {
            return false;
        };
        let value = value.clamp(ui.min.min(0), ui.max.max(1));
        if let Some(step) = ui.table.get_mut(element) {
            *step = value;
        }
        let stored = ui.value;
        // A `ui_table` is bound to an array, and arrays live under their `%` name.
        let array = format!("%{}", bare(control));
        let name = if state.vars.vars.contains_key(&array) {
            array
        } else {
            bare(control).to_string()
        };
        if !matches!(state.vars.vars.get(&name), Some(Variable::Array(values)) if element < values.len())
        {
            return false;
        }
        state
            .vars
            .set_index(&name, element as i64, Value::Int(value));
        drop(state);
        self.set_control_value(index, control, stored)
    }

    /// Snapshot of every slot's controls, for the GUI.
    ///
    /// A `ui_table` is bound to an array rather than to a single value, so its steps are copied in
    /// here; without that the panel has nothing to draw bars from.
    pub fn controls(&self, index: usize) -> Vec<UiControl> {
        let Some(slot) = self.slots.get(index) else {
            return Vec::new();
        };
        let Ok(state) = slot.lock() else {
            return Vec::new();
        };
        let mut controls = state.ui.clone();
        for control in &mut controls {
            if control.kind == UiKind::Table {
                let array = format!("%{}", control.name);
                let values = state
                    .vars
                    .vars
                    .get(&array)
                    .or_else(|| state.vars.vars.get(&control.name));
                if let Some(crate::ksp::Variable::Array(values)) = values {
                    control.table = values.iter().map(|value| value.as_int()).collect();
                }
            }
        }
        controls
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut out = Vec::new();
        for slot in &self.slots {
            if let Ok(state) = slot.lock() {
                for diagnostic in &state.diagnostics {
                    out.push(diagnostic.clone());
                }
            }
        }
        if let Ok(messages) = self.engine.diagnostics.lock() {
            out.extend(messages.iter().cloned());
        }
        out
    }

    pub fn is_initialized(&self, index: usize) -> bool {
        self.slots
            .get(index)
            .and_then(|slot| slot.lock().ok())
            .map(|state| state.initialized)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn polyphonic_values_follow_event_ids_and_do_not_leak_into_controllers() {
        let runtime = KspRuntime::compile(
            &[Some(
                r#"
            on init
                declare polyphonic $saved
            end on
            on note
                $saved := $EVENT_NOTE * 100 + $EVENT_ID
            end on
            on release
                note_off($saved)
            end on
            on controller
                note_off($saved)
            end on
        "#
                .into(),
            )],
            "",
        );
        runtime.initialize();
        runtime.dispatch(
            "note",
            EventContext {
                note: 60,
                id: 11,
                ..EventContext::default()
            },
        );
        runtime.dispatch(
            "note",
            EventContext {
                note: 64,
                id: 12,
                ..EventContext::default()
            },
        );
        let control = runtime.controller(64, 0);
        assert!(matches!(
            control.actions.as_slice(),
            [ScriptAction::NoteOff { id: 0 }]
        ));
        for (id, expected) in [(11, 6011), (12, 6412)] {
            let release = runtime.dispatch(
                "release",
                EventContext {
                    id,
                    ..EventContext::default()
                },
            );
            assert!(
                matches!(release.actions.as_slice(), [ScriptAction::NoteOff { id }] if *id == expected)
            );
        }
        assert!(runtime.slots[0].lock().unwrap().vars.note_vars.is_empty());
    }

    #[test]
    fn polyphonic_state_is_freed_even_without_a_release_callback() {
        let runtime = KspRuntime::compile(
            &[Some(
                r#"
            on init
                declare polyphonic $saved
            end on
            on note
                $saved := $EVENT_ID
            end on
        "#
                .into(),
            )],
            "",
        );
        runtime.initialize();
        runtime.dispatch(
            "note",
            EventContext {
                id: 7,
                ..EventContext::default()
            },
        );
        assert_eq!(runtime.slots[0].lock().unwrap().vars.note_vars.len(), 1);
        runtime.dispatch(
            "release",
            EventContext {
                id: 7,
                ..EventContext::default()
            },
        );
        assert!(runtime.slots[0].lock().unwrap().vars.note_vars.is_empty());
    }

    use super::*;

    #[test]
    fn controller_defaults_are_per_route_until_host_or_script_input() {
        use crate::modulation::Source;
        let runtime = KspRuntime::compile(&[], "");
        let route = |saved: i32| Source::Controller {
            number: 70,
            raw_assignment: saved.to_le_bytes(),
        };
        assert_eq!(runtime.engine.controller(70), 0);
        assert_eq!(
            runtime.engine.controller_source_value(&route(127)),
            Some(1.0)
        );
        assert_eq!(runtime.engine.controller_source_value(&route(0)), Some(0.0));
        assert_eq!(runtime.engine.controller_source_value(&route(-1)), None);
        runtime.controller(70, 32);
        for saved in [-1, 0, 127] {
            assert_eq!(
                runtime.engine.controller_source_value(&route(saved)),
                Some(32.0 / 127.0)
            );
        }
        let runtime =
            KspRuntime::compile(&[Some("on init\n set_controller(70,0)\nend on".into())], "");
        runtime.initialize();
        assert_eq!(
            runtime.engine.controller_source_value(&route(127)),
            Some(0.0)
        );
    }

    #[test]
    fn named_controller_routes_are_addressed_and_experimental_envelopes_are_opt_in() {
        use crate::modulation::{
            Curve, CurvePoint, EXTERNAL_HANDLE, GroupModulation, Modulator, Source, Target,
        };
        let base = crate::Ahdsr {
            attack_ms: 400.0,
            ..crate::Ahdsr::default()
        };
        let volume = Target {
            parameter: "volume".into(),
            name: "AMP".into(),
            destination: None,
            amount: 1.0,
            inverted: false,
            curve: None,
        };
        let target = Target {
            parameter: "ahdsr_attack".into(),
            name: "TARGET".into(),
            destination: Some(0),
            amount: 0.1,
            inverted: false,
            curve: Some(Curve {
                bipolar: true,
                points: vec![
                    CurvePoint {
                        x: 0.0,
                        y: 0.0,
                        shape: 0.0,
                    },
                    CurvePoint {
                        x: 1.0,
                        y: 1.0,
                        shape: 0.0,
                    },
                ],
            }),
        };
        let mut runtime = KspRuntime::compile(
            &[Some(
                r#"
        on init
          declare $module := find_mod(0,"SOURCE")
          declare $target := find_target(0,$module,"TARGET")
          declare ui_knob $depth(0,1000000,1)
        end on
        on ui_control($depth)
          set_engine_par($ENGINE_PAR_MOD_TARGET_INTENSITY,$depth,0,$module,$target)
        end on
        "#
                .into(),
            )],
            "",
        );
        Arc::get_mut(&mut runtime.engine).unwrap().modulation = vec![GroupModulation {
            modulators: vec![
                Modulator {
                    handle: 0,
                    name: "AMPLIFIER".into(),
                    targets: vec![volume],
                    source: Source::Internal,
                    envelope: Some(base),
                },
                Modulator {
                    handle: EXTERNAL_HANDLE,
                    name: "SOURCE".into(),
                    targets: vec![target],
                    source: Source::Controller {
                        number: 84,
                        raw_assignment: [0; 4],
                    },
                    envelope: None,
                },
            ],
            diagnostics: vec![],
        }];
        runtime.initialize();
        assert_eq!(runtime.variable(0, "module"), Some(EXTERNAL_HANDLE));
        assert_eq!(runtime.variable(0, "target"), Some(0));
        runtime.controller(84, 127);
        assert_eq!(runtime.engine.modulate_amp_envelope(0, base), base);
        Arc::get_mut(&mut runtime.engine)
            .unwrap()
            .external_envelopes_enabled = true;
        let high = runtime.engine.modulate_amp_envelope(0, base).attack_ms;
        runtime.controller(84, 0);
        let low = runtime.engine.modulate_amp_envelope(0, base).attack_ms;
        assert!(high > base.attack_ms && low < base.attack_ms);
        runtime.set_control_value(0, "depth", 0);
        assert!(
            (runtime.engine.modulate_amp_envelope(0, base).attack_ms - base.attack_ms).abs() < 0.01
        );
    }

    #[test]
    fn ui_notifications_reach_other_slots_for_raw_normalized_and_restored_controls() {
        let writer = r#"
        on init
          declare ui_knob $amount(0,1000000,1)
          pgs_create_key(LEVEL,1)
        end on
        on ui_control($amount)
          pgs_set_key_val(LEVEL,0,$amount)
        end on
        "#;
        let reader = r#"
        on pgs_changed
          set_engine_par($ENGINE_PAR_GAIN1,pgs_get_key_val(LEVEL,0),-1,0,-1)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(writer.into()), Some(reader.into())], "");
        runtime.initialize();
        assert!(runtime.set_control_value(0, "amount", 750000));
        assert_eq!(
            runtime.engine.get_cached("ENGINE_PAR_GAIN1", -1, 0, -1),
            Some(750000)
        );
        assert!(runtime.set_control_normalized(0, "amount", 0.25));
        assert_eq!(
            runtime.engine.get_cached("ENGINE_PAR_GAIN1", -1, 0, -1),
            Some(250000)
        );
        runtime.apply_persistent_batch(&[(0, vec![("amount".into(), vec![500000])])]);
        assert_eq!(
            runtime.engine.get_cached("ENGINE_PAR_GAIN1", -1, 0, -1),
            Some(500000)
        );
    }

    #[test]
    fn restore_batch_notifies_receivers_after_their_saved_values_are_restored() {
        let runtime = KspRuntime::compile(
            &[
                Some(
                    r#"
        on init
          declare ui_button $send
          pgs_create_key(READY,1)
        end on
        on ui_control($send)
          pgs_set_key_val(READY,0,1)
        end on
        "#
                    .into(),
                ),
                Some(
                    r#"
        on init
          declare $mode := 1
          declare $seen := -1
        end on
        on pgs_changed
          $seen := $mode
        end on
        "#
                    .into(),
                ),
            ],
            "",
        );
        runtime.initialize();
        runtime.apply_persistent_batch(&[
            (0, vec![("send".into(), vec![1])]),
            (1, vec![("mode".into(), vec![0])]),
        ]);
        assert_eq!(runtime.variable(1, "seen"), Some(0));
    }

    #[test]
    fn ui_played_notes_keep_unique_ids_and_event_changes() {
        let runtime = KspRuntime::compile(
            &[Some(
                r#"
        on init
          declare ui_button $trigger
          declare $id
        end on
        on ui_control($trigger)
          $id := play_note(60,100,0,250000)
          change_tune($id,1200000,0)
          change_pan($id,-1000,0)
        end on
        "#
                .into(),
            )],
            "",
        );
        runtime.initialize();
        runtime.set_control_value(0, "trigger", 1);
        let first = runtime.variable(0, "id").unwrap();
        runtime.set_control_value(0, "trigger", 0);
        let second = runtime.variable(0, "id").unwrap();
        assert!(first > 0 && second > first);
        let pending = runtime.engine.take_ui_dispatches();
        assert_eq!(pending.len(), 2);
        assert!(
            matches!(pending[0].actions[0],ScriptAction::PlayNote {id,release:250000,..} if id==first)
        );
        assert!(
            matches!(&pending[0].actions[1],ScriptAction::ChangeEvent {kind,id,value:1200000,relative:false} if kind=="change_tune" && *id==first)
        );
        assert!(runtime.engine.take_ui_dispatches().is_empty());
    }

    #[test]
    fn table_edits_run_the_table_callback() {
        let runtime = KspRuntime::compile(
            &[Some(
                r#"
        on init
          declare ui_table %steps[3](1,1,100)
          declare $seen
        end on
        on ui_control(%steps)
          $seen := %steps[1]
        end on
        "#
                .into(),
            )],
            "",
        );
        runtime.initialize();
        assert!(runtime.set_table_element(0, "steps", 1, 73));
        assert_eq!(runtime.variable(0, "seen"), Some(73));
        assert!(!runtime.set_table_element(0, "steps", 3, 99));
    }

    #[test]
    fn random_is_inclusive_and_menu_rows_can_be_relabelled() {
        let runtime = KspRuntime::compile(
            &[Some(
                r#"
        on init
          declare $i
          declare $x
          declare $lowest := 100
          declare $highest := -100
          declare ui_menu $menu
          add_menu_item($menu,"before",0)
          set_menu_item_str(get_ui_id($menu),0,"after")
          while ($i < 100)
            $x := random(-3,3)
            if ($x < $lowest)
              $lowest := $x
            end if
            if ($x > $highest)
              $highest := $x
            end if
            inc($i)
          end while
        end on
        "#
                .into(),
            )],
            "",
        );
        runtime.initialize();
        assert_eq!(runtime.variable(0, "lowest"), Some(-3));
        assert_eq!(runtime.variable(0, "highest"), Some(3));
        assert_eq!(runtime.controls(0)[0].menu, vec!["after"]);
    }

    struct TestHost {
        pub sets: Vec<(String, i64, i64, i64, i64)>,
        pub state: Arc<EngineState>,
    }

    impl Default for TestHost {
        fn default() -> Self {
            Self {
                sets: Vec::new(),
                state: Arc::new(EngineState::default()),
            }
        }
    }

    impl Host for TestHost {
        fn engine_par(&mut self, _p: &str, _g: i64, _s: i64, _x: i64) -> Option<i64> {
            None
        }
        fn set_engine_par(&mut self, p: &str, v: i64, g: i64, s: i64, x: i64) {
            self.sets.push((p.to_string(), v, g, s, x));
        }
        fn engine_par_disp(&mut self, _p: &str, _g: i64, _s: i64, _x: i64) -> String {
            String::new()
        }
    }

    /// `num_elements` is how a script walks a group list. A missing implementation made every
    /// `while ($a < num_elements(%groups))` loop run zero times, so a script that disallows all
    /// groups and re-allows them in a loop allowed none of them and played silence.
    #[test]
    fn num_elements_counts_an_array() {
        let text = r#"
        on init
          declare %groups[2] := (4, 84)
          declare ui_slider $seen(0, 10000)
          declare $a
          $a := 0
          while ($a < num_elements(%groups))
            $seen := $seen * 100 + %groups[$a]
            inc($a)
          end while
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.controls(0)[0].value, 4 * 100 + 84);
    }

    /// The same shape end to end: disallow everything, then re-allow a list read from an array.
    #[test]
    fn a_group_loop_that_uses_num_elements_allows_the_listed_groups() {
        let text = r#"
        on init
          declare %groups[2] := (1, 3)
          declare $a
        end on
        on note
          disallow_group($ALL_GROUPS)
          $a := 0
          while ($a < num_elements(%groups))
            allow_group(%groups[$a])
            inc($a)
          end while
        end on
        "#;
        let groups: Vec<GroupInfo> = (0..4)
            .map(|_| GroupInfo {
                release_trigger: false,
                envelope: None,
            })
            .collect();
        let runtime = KspRuntime::compile_with_groups(&[Some(text.to_string())], "", groups);
        runtime.initialize();
        let dispatch = runtime.dispatch(
            "note",
            EventContext {
                note: 60,
                velocity: 100,
                id: 1,
                status: 1,
            },
        );
        let mask = dispatch.allowed_groups.expect("the script routed groups");
        assert_eq!(mask, vec![false, true, false, true]);
    }

    /// Restoring a saved instrument must not let a replayed `on ui_control` handler overwrite
    /// another saved variable. one library's articulation selectors all assign `$Current_Art`, so the
    /// last handler to run left it on articulation 24 instead of the saved 1 and nothing played.
    #[test]
    fn a_replayed_handler_does_not_overwrite_a_saved_variable() {
        let text = r#"
        on init
          declare $current
          declare ui_menu $picker
        end on
        on ui_control($picker)
          $current := 24
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        runtime.apply_persistent(
            0,
            &[
                ("$current".to_string(), vec![1]),
                ("$picker".to_string(), vec![1]),
            ],
        );
        let state = runtime.slots[0].lock().unwrap();
        assert_eq!(
            state.vars.get("current").as_int(),
            1,
            "the saved value must survive the handler replay"
        );
    }

    /// A button stored as *pressed* is the saved selection of a radio group, and its handler is
    /// what lays the page out: one panel's tab button is the only thing that moves the four step
    /// tables into view. Skipping every button on restore left the tab looking selected, the
    /// table at (0, 0) and the panel with a frozen, empty editor until the user clicked it.
    #[test]
    fn a_pressed_button_is_replayed_so_the_page_lays_out() {
        let text = r#"
        on init
          declare ui_button $Tab
          make_persistent($Tab)
          read_persistent_var($Tab)
          declare ui_table %Steps[4](0, 127, 127)
          make_persistent(%Steps)
          read_persistent_var(%Steps)
        end on
        on ui_control($Tab)
          move_control_px(%Steps, 16, 235)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        runtime.apply_persistent(
            0,
            &[
                ("$Tab".to_string(), vec![1]),
                ("%Steps".to_string(), vec![1, 2, 3, 4]),
            ],
        );
        let controls = runtime.controls(0);
        let table = controls
            .iter()
            .find(|control| control.name == "Steps")
            .expect("the table should exist");
        assert_eq!(
            (table.x, table.y),
            (16, 235),
            "the saved tab must run its handler, which is what places the table"
        );
        assert_eq!(table.table, vec![1, 2, 3, 4]);
    }

    /// The same restore must not fire a button that was saved *unpressed*: one panel's three
    /// direction buttons each turn the others off and then turn themselves on, so replaying all
    /// of them left the last one selected whatever the user had chosen.
    #[test]
    fn an_unpressed_button_stays_where_it_was() {
        let text = r#"
        on init
          declare ui_button $First
          declare ui_button $Second
          make_persistent($First)
          make_persistent($Second)
          read_persistent_var($First)
          read_persistent_var($Second)
        end on
        on ui_control($Second)
          $First := 0
          $Second := 1
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        runtime.apply_persistent(
            0,
            &[
                ("$First".to_string(), vec![1]),
                ("$Second".to_string(), vec![0]),
            ],
        );
        let controls = runtime.controls(0);
        let first = controls.iter().find(|c| c.name == "First").unwrap();
        assert_eq!(
            first.value, 1,
            "the saved selection must survive the restore"
        );
    }

    #[test]
    fn search_reports_the_index_of_an_element() {
        let text = r#"
        on init
          declare %groups[2] := (4, 5)
          declare ui_slider $found(0, 10)
          $found := search(%groups, 4)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let value = runtime.controls(0)[0].value;
        assert_eq!(value, 0, "4 is at index 0");

        let text = r#"
        on init
          declare %groups[2] := (4, 5)
          declare ui_slider $found(0, 10)
          $found := search(%groups, 5)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.controls(0)[0].value, 1);

        let text = r#"
        on init
          declare %groups[2] := (4, 5)
          declare ui_slider $found(0, 10)
          $found := search(%groups, 0) + 5
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        // Not present: `search` returns -1, so -1 + 5 = 4.
        assert_eq!(runtime.controls(0)[0].value, 4);
    }

    #[test]
    fn editing_a_table_lands_in_the_array_it_is_bound_to() {
        // Dragging a bar writes the step back into the array. Writing to the stripped name used
        // to create a second array the panel never read, so the bars never moved.
        let text = r#"
        on init
          declare const $steps := 4
          declare ui_table %Steps[$steps](1, 1, 127)
          %Steps[0] := 10
          %Steps[1] := 20
          %Steps[2] := 30
          %Steps[3] := 40
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert!(runtime.set_table_element(0, "Steps", 1, 99));
        let table = runtime
            .controls(0)
            .into_iter()
            .find(|control| control.name == "Steps")
            .expect("the table is a control");
        assert_eq!(table.table, vec![10, 99, 30, 40]);
    }

    #[test]
    fn a_table_reads_the_array_it_is_bound_to() {
        // `declare ui_table %Steps[n]` binds the array; the control's own name has no `%`, so a
        // writer that used the stripped name would update a slot nothing reads.
        let text = r#"
        on init
          declare const $steps := 4
          declare ui_table %Steps[$steps](1, 1, 127)
          %Steps[0] := 10
          %Steps[1] := 20
          %Steps[2] := 30
          %Steps[3] := 40
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let table = runtime
            .controls(0)
            .into_iter()
            .find(|control| control.name == "Steps")
            .expect("the table is a control");
        assert_eq!(table.table, vec![10, 20, 30, 40]);
    }

    #[test]
    fn a_menu_grows_with_its_items_and_reports_the_selected_row() {
        // `declare ui_menu` has no arguments, so the range has to come from `add_menu_item`;
        // without it every selection clamps to 0 and the menu looks dead.
        let text = r#"
        on init
          declare $picked := -1
          declare ui_menu $ir_menu
          add_menu_item($ir_menu, "IR1", 0)
          add_menu_item($ir_menu, "IR2", 1)
          add_menu_item($ir_menu, "IR3", 2)
        end on

        on ui_control($ir_menu)
          $picked := get_control_par(get_ui_id($ir_menu), $CONTROL_PAR_SELECTED_ITEM_IDX)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let menu = runtime
            .controls(0)
            .into_iter()
            .find(|c| c.name == "ir_menu")
            .unwrap();
        assert_eq!(
            (menu.min, menu.max),
            (0, 2),
            "three items give a 0..2 range"
        );
        assert_eq!(menu.menu.len(), 3);

        runtime.set_control_value(0, "ir_menu", 2);
        let value = runtime
            .controls(0)
            .into_iter()
            .find(|c| c.name == "ir_menu")
            .map(|c| c.value)
            .unwrap();
        assert_eq!(value, 2, "the last row must be selectable");
    }

    #[test]
    fn a_function_assigns_globals_and_buttons_latch() {
        // Retail panels build radio groups this way: every button calls a helper that zeroes the
        // others, then latches itself. That needs (a) functions to keep their global writes and
        // (b) `declare ui_button` to be a 0/1 control rather than a 0..0 range that clamps to 0.
        let text = r#"
        on init
          declare ui_button $A
          declare ui_button $B
        end on

        function all_off1
          $A := 0
          $B := 0
        end function

        on ui_control($A)
          call all_off1
          $A := 1
        end on

        on ui_control($B)
          call all_off1
          $B := 1
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.controls(0)[0].max, 1, "a button is a 0/1 control");

        runtime.set_control_value(0, "A", 1);
        runtime.set_control_value(0, "B", 1);
        let values: Vec<i64> = runtime.controls(0).iter().map(|c| c.value).collect();
        assert_eq!(values, vec![0, 1], "clicking B must clear A");
    }

    #[test]
    fn linked_controls_move_together_without_recursion() {
        // Ambisonic-style linkage: the visible slider and its drag handle write each other.
        let text = r#"
        on init
          declare ui_slider $One(1, 900000)
          declare ui_slider $Dot(1, 900000)
          make_persistent($One)
          move_control_px($Dot, 362, 120)
        end on

        on ui_control($One)
          $Dot := $One
        end on

        on ui_control($Dot)
          $One := $Dot
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert!(runtime.set_control_normalized(0, "One", 0.5));
        let controls = runtime.controls(0);
        let dot = controls.iter().find(|c| c.name == "Dot").unwrap();
        assert!(
            (dot.normalized() - 0.5).abs() < 0.01,
            "dot should follow the slider, got {}",
            dot.normalized()
        );
        assert!(dot.px);
    }

    #[test]
    fn picture_names_are_recorded_for_custom_panels() {
        let text = r#"
        on init
          declare ui_slider $Dot(0, 100)
          declare $DotId
          $DotId := get_ui_id($Dot)
          set_control_par_str($DotId, $CONTROL_PAR_PICTURE, "BlueDot")
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.controls(0)[0].picture.as_deref(), Some("BlueDot"));
    }

    #[test]
    /// A panel's LFO is an `on listener` callback: the playhead is a control the script moves from
    /// there, and the interval is asked for from inside that same callback. Without a clock the
    /// playhead never leaves step 1 and the panel looks frozen.
    #[test]
    fn a_listener_runs_on_its_own_clock() {
        let text = r#"
        on init
          declare $count
          declare ui_slider $pos(0, 31)
        end on
        on listener
          inc($count)
          $pos := $count
          change_listener_par($NI_SIGNAL_TIMER_MS, 250)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(
            runtime.listener_period_ms(0),
            Some(DEFAULT_LISTENER_MS),
            "a slot with a listener ticks before it has asked for a period"
        );
        runtime.listener(0);
        runtime.listener(0);
        let controls = runtime.controls(0);
        let pos = controls
            .iter()
            .find(|control| control.name == "pos")
            .unwrap();
        assert_eq!(pos.value, 2, "the playhead advances once per listener tick");
        assert_eq!(runtime.listener_period_ms(0), Some(250.0));
    }

    /// A slot without a listener has no clock at all, so the engine can skip it.
    #[test]
    fn only_a_slot_with_a_listener_gets_a_clock() {
        let text = r#"
        on init
          declare $x
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.listener_period_ms(0), None);
    }

    /// `ms_to_ticks` is what a timer script divides by to turn a tempo into a step length. The
    /// tick rate is pinned by the rate those panels run at (see `TICKS_PER_QUARTER`).
    #[test]
    fn ms_to_ticks_uses_the_host_tempo() {
        let text = r#"
        on init
          declare ui_slider $ticks(0, 100000)
          $ticks := ms_to_ticks(500)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        // One quarter note at 120 BPM is 1280 ticks.
        assert_eq!(runtime.controls(0)[0].value, 1280);

        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.engine.set_tempo_bpm(60.0);
        runtime.initialize();
        assert_eq!(runtime.controls(0)[0].value, 640);
    }

    /// A script's `set_controller` is how one panel slot hands a value to another: the LFO slot
    /// writes CC 76 and the other slots read `%CC[76]`. It has to be one shared table.
    #[test]
    fn a_script_can_write_a_controller_another_slot_reads() {
        let writer = r#"
        on listener
          set_controller(76, 99)
        end on
        "#;
        let reader = r#"
        on init
          declare ui_slider $seen(0, 127)
        end on
        on listener
          $seen := %CC[76]
        end on
        "#;
        let runtime =
            KspRuntime::compile(&[Some(writer.to_string()), Some(reader.to_string())], "");
        runtime.initialize();
        runtime.listener(0);
        assert_eq!(runtime.engine.controller(76), 99);
        runtime.listener(1);
        assert_eq!(runtime.controls(1)[0].value, 99);
    }

    fn controller_reaches_the_script_through_cc_array() {
        let text = r#"
        on init
          declare $i
        end on

        on controller
          if ($CC_NUM = 64)
            set_engine_par($ENGINE_PAR_VOLUME, %CC[64], -1, -1, $NI_BUS_OFFSET + 5)
          end if
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert!(
            (runtime.engine.bus_gain(5) - 1.0).abs() < 0.001 || runtime.engine.bus_gain(5) == 1.0
        );
        runtime.controller(64, 0);
        assert_eq!(runtime.engine.bus_gain(5), 0.0);
        runtime.controller(64, 630_000);
        assert!((runtime.engine.bus_gain(5) - 1.0).abs() < 0.001);
        // A different controller number must not move it.
        runtime.controller(1, 0);
        assert!((runtime.engine.bus_gain(5) - 1.0).abs() < 0.001);
    }

    #[test]
    fn pgs_symbolic_keys_keep_independent_values_and_sizes() {
        let text = r#"
        on init
          declare $first
          declare $second
          declare $missing
          pgs_create_key(COUNTER, 2)
          pgs_create_key(LEVEL, 1)
          pgs_set_key_val(COUNTER, 0, 11)
          pgs_set_key_val(COUNTER, 1, 12)
          pgs_set_key_val(LEVEL, 0, 99)
          $first := pgs_get_key_val(COUNTER, 1)
          $second := pgs_get_key_val(LEVEL, 0)
          $missing := pgs_key_exists(UNCREATED)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        assert_eq!(runtime.variable(0, "first"), Some(12));
        assert_eq!(runtime.variable(0, "second"), Some(99));
        assert_eq!(runtime.variable(0, "missing"), Some(0));
    }

    #[test]
    fn script_can_mute_a_group_beyond_the_bus_count() {
        let groups = (0..200)
            .map(|_| GroupInfo {
                release_trigger: false,
                envelope: None,
            })
            .collect();
        let runtime = KspRuntime::compile_with_groups(
            &[Some(
                r#"
        on init
          set_engine_par($ENGINE_PAR_VOLUME, 0, 160, -1, -1)
        end on
        "#
                .to_string(),
            )],
            "",
            groups,
        );
        runtime.initialize();
        assert_eq!(runtime.engine.group_gain(160), 0.0);
        assert_eq!(runtime.engine.group_gain(159), 1.0);
    }

    #[test]
    fn pgs_write_wakes_the_other_slots() {
        let writer = r#"
        on init
          declare $x
        end on

        on note
          pgs_create_key(MIC, 1)
          pgs_set_key_val(MIC, 0, 1234)
        end on
        "#;
        let reader = r#"
        on init
          declare $y
        end on

        on pgs_changed
          set_engine_par($ENGINE_PAR_VOLUME, pgs_get_key_val(MIC, 0), -1, -1, $NI_BUS_OFFSET + 2)
        end on
        "#;
        let runtime =
            KspRuntime::compile(&[Some(writer.to_string()), Some(reader.to_string())], "");
        runtime.initialize();
        runtime.dispatch(
            "note",
            EventContext {
                note: 60,
                velocity: 100,
                id: 1,
                status: 1,
            },
        );
        // 1234 / 1_000_000 is far below 630 000, so the bus must be attenuated but not silent.
        let gain = runtime.engine.bus_gain(2);
        assert!(gain > 0.0 && gain < 0.01, "gain {gain}");
    }

    #[test]
    fn ui_control_dispatches_set_engine_par_on_bus() {
        let text = r#"
        on init
          declare ui_slider $mic(0, 630000)
          set_control_par(get_ui_id($mic), $CONTROL_PAR_DEFAULT_VALUE, 630000)
          make_persistent($mic)
        end on

        on ui_control($mic)
          set_engine_par($ENGINE_PAR_VOLUME, $mic, -1, -1, $NI_BUS_OFFSET + 1)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let controls = runtime.controls(0);
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].value, 630_000);
        assert!(runtime.set_control_normalized(0, "mic", 0.5));
    }

    /// Panels page themselves by hiding whole controls, so `$CONTROL_PAR_HIDE` has to land in the
    /// same state `hide_part` writes. Without it every page of a paged panel draws at once.
    #[test]
    fn control_par_hide_pages_the_panel() {
        let text = r#"
        on init
          declare ui_slider $page_a (0, 100)
          declare ui_slider $page_b (0, 100)
          set_control_par(get_ui_id($page_b), $CONTROL_PAR_HIDE, $HIDE_WHOLE_CONTROL)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let controls = runtime.controls(0);
        assert_eq!(controls[0].hidden_parts & HIDE_WHOLE_CONTROL as u32, 0);
        assert_ne!(controls[1].hidden_parts & HIDE_WHOLE_CONTROL as u32, 0);
    }

    /// Drum kits paint the keyboard from `on init`: everything inactive, then one swatch per
    /// playing key. Dropping those calls leaves the whole 128-key bed looking playable.
    #[test]
    fn set_key_color_paints_the_key_range() {
        let text = r#"
        on init
          declare $n := 0
          while ($n < 128)
            set_key_color($n, $KEY_COLOR_INACTIVE)
            inc($n)
          end while
          set_key_color(36, $KEY_COLOR_RED)
          set_key_color(38, $KEY_COLOR_GREEN)
          set_key_color(39, $KEY_COLOR_NONE)
          set_key_color(200, $KEY_COLOR_BLUE)
        end on
        "#;
        let runtime = KspRuntime::compile(&[Some(text.to_string())], "");
        runtime.initialize();
        let colours = runtime.engine.key_colors();
        let swatch = |name: &str| {
            KEY_COLOR_NAMES
                .iter()
                .position(|known| *known == name)
                .map(|index| index as i64)
        };
        assert_eq!(colours[36], swatch("RED"));
        assert_eq!(colours[38], swatch("GREEN"));
        assert_eq!(colours[0], Some(KEY_COLOR_INACTIVE));
        assert_eq!(colours[60], Some(KEY_COLOR_INACTIVE));
        // `$KEY_COLOR_NONE` hands the key back to the zone mapping instead of storing a swatch.
        assert_eq!(colours[39], None);
        // Out-of-range notes are dropped, and a key beyond 127 does not exist to paint.
        assert_eq!(colours[127], Some(KEY_COLOR_INACTIVE));
    }

    #[test]
    fn key_colour_constants_are_numbered_in_swatch_order() {
        // `$KEY_COLOR_DEFAULT` is the blue every mapped key gets, and `$KEY_COLOR_INACTIVE` is
        // last; the sixteen hues in between follow NI's listing order.
        assert_eq!(symbol("KEY_COLOR_DEFAULT"), Some(Value::Int(0)));
        assert_eq!(symbol("KEY_COLOR_RED"), Some(Value::Int(1)));
        assert_eq!(symbol("KEY_COLOR_FUCHSIA"), Some(Value::Int(16)));
        assert_eq!(symbol("KEY_COLOR_INACTIVE"), Some(Value::Int(17)));
        // The neutral colours sit above the swatches and never collide with one.
        assert_eq!(symbol("KEY_COLOR_NONE"), Some(Value::Int(KEY_COLOR_NONE)));
        assert_eq!(symbol("KEY_COLOR_BLACK"), Some(Value::Int(KEY_COLOR_BLACK)));
        assert_eq!(symbol("KEY_COLOR_WHITE"), Some(Value::Int(KEY_COLOR_WHITE)));
        assert_eq!(symbol("KEY_COLOR_SOMETHING"), None);
    }
}
