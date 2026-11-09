//! Read-only extraction of KSP `ui_*` declarations.
//!
//! This is **not** a KSP interpreter. It only mirrors what the library author declared through
//! `declare ui_*`, `set_text`, `move_control`, `set_knob_unit` and `make_persistent` so a GUI can
//! display the script's panel. Actual script execution is explicitly out of scope in
//! `docs/18-scripts-and-ksp.md`. Values are taken from the slot's persisted user values when
//! present, otherwise from the declared default.

use crate::script::ScriptSlot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspControlKind {
    Knob,
    Slider,
    Label,
    Menu,
    Switch,
    Button,
    ValueEdit,
    /// `ui_table`: a row of bars, one per element of the array it is bound to.
    Table,
    Other,
}

#[derive(Clone, Debug)]
pub struct KspControl {
    pub name: String,
    pub kind: KspControlKind,
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub min: i64,
    pub max: i64,
    pub default: i64,
    pub value: i64,
    pub unit: Option<String>,
    pub persistent: bool,
    /// `move_control_px` coordinates instead of grid cells.
    pub px: bool,
    /// `$CONTROL_PAR_PICTURE` name; custom panels draw themselves from these.
    pub picture: Option<String>,
    /// `add_menu_item` labels, for menus.
    pub menu: Vec<String>,
    /// `$CONTROL_PAR_MOUSE_BEHAVIOUR`, 0 = absolute, negative = relative.
    pub mouse_behaviour: i32,
    /// `set_knob_label` readout; separate from the control's name.
    pub value_text: Option<String>,
    /// `$CONTROL_PAR_WIDTH` / `$CONTROL_PAR_HEIGHT`: explicit pixel size, used by picture buttons.
    pub pixel_width: Option<i32>,
    pub pixel_height: Option<i32>,
    /// A `ui_table`'s current values, one per step.
    pub table: Vec<i64>,
    /// `$CONTROL_PAR_BAR_COLOR`, the colour a table is drawn in.
    pub bar_color: Option<i64>,
    /// Parts hidden with `hide_part`: 1 = background, 2 = cursor, 4 = value, 8 = title,
    /// 16 = whole control.
    pub hidden_parts: u32,
}

impl KspControl {
    pub fn normalized_value(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) as f64 / (self.max - self.min) as f64) as f32
    }

    pub fn display_value(&self) -> String {
        match self.unit.as_deref() {
            Some("dB") => {
                // Persisted KSP UI values are integer controller values; the script owns the
                // actual mapping. Show the controller value rather than inventing a dB law.
                format!("{}", self.value)
            }
            Some("Hz") | Some("ms") | Some("%") => {
                format!("{} {}", self.value, self.unit.as_deref().unwrap())
            }
            Some(unit) => format!("{} {unit}", self.value),
            None => self.value.to_string(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct KspPanel {
    pub controls: Vec<KspControl>,
    pub has_ui: bool,
    /// Native KSP performance-view width. Kontakt's grid is fixed at 633 px unless changed by
    /// `set_ui_width_px`.
    pub ui_width_px: f32,
    /// Native KSP performance-view height from `set_ui_height_px`.
    pub ui_height_px: f32,
}

impl KspPanel {
    /// Build the panel from every script slot.
    ///
    /// A multi-script instrument can declare its controls in one slot and its panel geometry in
    /// another (`set_ui_height_px` usually lives with the artwork script). Taking only the first
    /// non-empty slot loses both, so geometry is the widest/tallest any slot asks for and the
    /// control list comes from the slot that declares the most of them.
    pub fn from_slots(slots: &[ScriptSlot]) -> Self {
        let mut panel = Self::default();
        let mut best = 0usize;
        let mut height: Option<f32> = None;
        for slot in slots {
            if slot.is_empty() {
                continue;
            }
            let (candidate, height_requested) = Self::build(slot);
            panel.ui_width_px = panel.ui_width_px.max(candidate.ui_width_px);
            if height_requested {
                height = Some(height.map_or(candidate.ui_height_px, |current: f32| {
                    current.max(candidate.ui_height_px)
                }));
            }
            if candidate.controls.len() > best {
                best = candidate.controls.len();
                panel.controls = candidate.controls;
                panel.has_ui = candidate.has_ui;
            }
        }
        if let Some(height) = height {
            panel.ui_height_px = height;
        }
        panel
    }

    pub fn from_slot(slot: &ScriptSlot) -> Self {
        Self::build(slot).0
    }

    /// Build one slot's panel and report whether the script set the panel height itself.
    fn build(slot: &ScriptSlot) -> (Self, bool) {
        let Some(text) = &slot.text else {
            return (Self::default(), false);
        };
        let mut builder = Builder::default();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('{') {
                continue;
            }
            builder.consume(line);
        }

        // Apply persisted values, which override declarations.
        for control in &mut builder.controls {
            if let Some(values) = slot.persistent_value(&control.name) {
                if let Some(first) = values.first() {
                    control.value = *first;
                }
            }
        }
        builder
            .controls
            .retain(|c| c.kind != KspControlKind::Other || !c.label.is_empty());
        builder.controls.sort_by_key(|c| (c.y, c.x, c.name.clone()));
        (
            Self {
                has_ui: !builder.controls.is_empty(),
                controls: builder.controls,
                ui_width_px: builder.ui_width_px,
                ui_height_px: builder.ui_height_px,
            },
            builder.height_requested,
        )
    }

    pub fn find(&self, needle: &str) -> Option<&KspControl> {
        self.controls
            .iter()
            .find(|c| c.name.to_ascii_lowercase().contains(needle))
    }

    pub fn section_label(&self, needle: &str) -> Option<&KspControl> {
        self.controls.iter().find(|c| {
            c.kind == KspControlKind::Label
                && c.label
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
        })
    }
}

struct Builder {
    controls: Vec<KspControl>,
    ui_width_px: f32,
    ui_height_px: f32,
    /// Set only when the script calls `set_ui_height_px`, so the default never masks a slot that
    /// asks for a shorter panel than another slot.
    height_requested: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            controls: Vec::new(),
            ui_width_px: 633.0,
            ui_height_px: 400.0,
            height_requested: false,
        }
    }
}

impl Builder {
    fn consume(&mut self, line: &str) {
        if let Some(args) = call_args(line, "set_ui_height_px") {
            if let Ok(height) = args.trim().parse::<f32>() {
                self.ui_height_px = height.clamp(50.0, 750.0);
                self.height_requested = true;
            }
            return;
        }
        if let Some(args) = call_args(line, "set_ui_width_px") {
            if let Ok(width) = args.trim().parse::<f32>() {
                self.ui_width_px = width.clamp(200.0, 1000.0);
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("declare ") {
            self.consume_declare(rest.trim());
            return;
        }
        if let Some(args) = call_args(line, "set_text") {
            let parts = split_args(args);
            if parts.len() >= 2 {
                if let Some(name) = var_name(parts[0]) {
                    if let Some(control) = self.control_mut(name) {
                        control.label = unquote(parts[1]);
                    }
                }
            }
            return;
        }
        if let Some(args) = call_args(line, "move_control") {
            let parts = split_args(args);
            if parts.len() >= 3 {
                if let Some(name) = var_name(parts[0]) {
                    let x = parts[1].trim().parse().unwrap_or(0);
                    let y = parts[2].trim().parse().unwrap_or(0);
                    if let Some(control) = self.control_mut(name) {
                        control.x = x;
                        control.y = y;
                    }
                }
            }
            return;
        }
        if let Some(args) = call_args(line, "set_knob_unit") {
            let parts = split_args(args);
            if parts.len() >= 2 {
                if let Some(name) = var_name(parts[0]) {
                    let unit = match parts[1].trim() {
                        "$KNOB_UNIT_HZ" => "Hz",
                        "$KNOB_UNIT_DB" => "dB",
                        "$KNOB_UNIT_MS" => "ms",
                        "$KNOB_UNIT_PERCENT" => "%",
                        "$KNOB_UNIT_ST" => "st",
                        "$KNOB_UNIT_OCT" => "oct",
                        _ => "",
                    };
                    if let Some(control) = self.control_mut(name) {
                        if !unit.is_empty() {
                            control.unit = Some(unit.to_string());
                        }
                    }
                }
            }
            return;
        }
        if let Some(args) = call_args(line, "set_knob_defval") {
            let parts = split_args(args);
            if parts.len() >= 2 {
                if let Some(name) = var_name(parts[0]) {
                    let value = parts[1].trim().parse().unwrap_or(0);
                    if let Some(control) = self.control_mut(name) {
                        control.default = value;
                        control.value = value;
                    }
                }
            }
            return;
        }
        if let Some(args) = call_args(line, "set_control_par") {
            let parts = split_args(args);
            // set_control_par(get_ui_id($x), $CONTROL_PAR_DEFAULT_VALUE, 630000)
            if parts.len() >= 3 && parts[1].contains("DEFAULT_VALUE") {
                if let Some(name) = var_name(parts[0]) {
                    let value = parts[2].trim().parse().unwrap_or(0);
                    if let Some(control) = self.control_mut(name) {
                        control.default = value;
                        control.value = value;
                    }
                }
            }
            return;
        }
        if let Some(args) = call_args(line, "make_persistent") {
            if let Some(name) = var_name(args) {
                if let Some(control) = self.control_mut(name) {
                    control.persistent = true;
                }
            }
        }
    }

    fn consume_declare(&mut self, rest: &str) {
        let mut split = rest.splitn(2, char::is_whitespace);
        let kind_token = split.next().unwrap_or_default();
        let remainder = split.next().unwrap_or_default().trim();
        let kind = match kind_token {
            "ui_knob" => KspControlKind::Knob,
            "ui_slider" => KspControlKind::Slider,
            "ui_label" => KspControlKind::Label,
            "ui_menu" => KspControlKind::Menu,
            "ui_switch" => KspControlKind::Switch,
            "ui_button" => KspControlKind::Button,
            "ui_value_edit" => KspControlKind::ValueEdit,
            _ => return,
        };
        let Some(name) = var_name(remainder) else {
            return;
        };
        let (min, max, width, height) = if let Some(open) = remainder.find('(') {
            let args = remainder[open + 1..].trim_end_matches(')');
            let parts = split_args(args);
            let nums: Vec<i64> = parts
                .iter()
                .filter_map(|part| part.trim().parse::<i64>().ok())
                .collect();
            match kind {
                KspControlKind::Knob => (
                    nums.first().copied().unwrap_or(0),
                    nums.get(1).copied().unwrap_or(1_000_000),
                    1,
                    1,
                ),
                KspControlKind::Slider => (
                    nums.first().copied().unwrap_or(0),
                    nums.get(1).copied().unwrap_or(1_000_000),
                    1,
                    1,
                ),
                KspControlKind::Label => (
                    0,
                    0,
                    nums.first().copied().unwrap_or(1) as i32,
                    nums.get(1).copied().unwrap_or(1) as i32,
                ),
                _ => (0, 0, 1, 1),
            }
        } else {
            (0, 1_000_000, 1, 1)
        };

        let label = name.trim_start_matches('$').replace('_', " ");
        self.controls.push(KspControl {
            name: name.to_string(),
            kind,
            label,
            x: 0,
            y: 0,
            width,
            height,
            min,
            max,
            default: min,
            value: min,
            unit: None,
            persistent: false,
            px: false,
            picture: None,
            menu: Vec::new(),
            mouse_behaviour: 0,
            value_text: None,
            pixel_width: None,
            pixel_height: None,
            table: Vec::new(),
            bar_color: None,
            hidden_parts: 0,
        });
    }

    fn control_mut(&mut self, name: &str) -> Option<&mut KspControl> {
        self.controls
            .iter_mut()
            .find(|control| control.name == name)
    }
}

fn call_args<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let index = line.find(name)?;
    let after = &line[index + name.len()..];
    let open = after.find('(')?;
    let close = after.rfind(')')?;
    if close <= open {
        return None;
    }
    Some(&after[open + 1..close])
}

fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut start = 0usize;
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
        } else if !in_string {
            match b {
                b'(' => depth += 1,
                b')' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(args[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out.push(args[start..].trim());
    out
}

fn var_name(token: &str) -> Option<&str> {
    let token = token.trim();
    let start = token.find('$')?;
    let rest = &token[start..];
    let end = rest
        .char_indices()
        .find(|(i, c)| *i > 0 && !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn unquote(token: &str) -> String {
    let token = token.trim();
    if let Some(stripped) = token.strip_prefix('"') {
        if let Some(end) = stripped.rfind('"') {
            return stripped[..end].replace("\\\"", "\"").replace("\\\\", "\\");
        }
    }
    token.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ScriptSlot;

    #[test]
    fn extracts_nfo_style_panel() {
        let slot = ScriptSlot {
            text: Some(
                r#"
                on init
                declare ui_knob $filter_cutoff_knob(0, 1000000, 1)
                set_text($filter_cutoff_knob, "Cutoff")
                set_knob_unit($filter_cutoff_knob, $KNOB_UNIT_HZ)
                move_control($filter_cutoff_knob, 4, 2)
                make_persistent($filter_cutoff_knob)
                end on
                "#
                .into(),
            ),
            ..Default::default()
        };
        let panel = KspPanel::from_slot(&slot);
        let control = panel.find("cutoff").unwrap();
        assert_eq!(control.label, "Cutoff");
        assert_eq!(control.unit.as_deref(), Some("Hz"));
        assert_eq!((control.x, control.y), (4, 2));
        assert!(control.persistent);
    }
}
