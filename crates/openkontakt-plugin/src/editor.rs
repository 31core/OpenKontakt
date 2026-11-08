use crate::params::OpenKontaktParams;
use crate::state::LoadedInstrument;
use crate::state::{LoadStatus, SharedState};
use egui::{
    Align2, Color32, FontFamily, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::ParamSetter;
use nice_plug::prelude::{FloatParam, Param};
use nice_plug_egui::NiceEguiApp;
use openkontakt::{KspControl, KspControlKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

const DESIGN_WIDTH: f32 = 1180.0;
const DESIGN_HEIGHT: f32 = 780.0;
const SIDEBAR_WIDTH: f32 = 340.0;
const HEADER_HEIGHT: f32 = 50.0;
/// Global Vol / Global Tune strip below the logo bar.
const GLOBAL_ROW_HEIGHT: f32 = 44.0;
const CONTENT_TOP: f32 = HEADER_HEIGHT + GLOBAL_ROW_HEIGHT;
const INSTRUMENT_HEADER_HEIGHT: f32 = 62.0;
const KEYBOARD_HEIGHT: f32 = 104.0;

// OpenKontakt's palette: warm varnish against a near-black Kontakt-style chassis.
const BG: Color32 = Color32::from_rgb(17, 17, 15);
const PANEL: Color32 = Color32::from_rgb(25, 25, 22);
const PANEL_LIGHT: Color32 = Color32::from_rgb(32, 32, 28);
const LINE: Color32 = Color32::from_rgb(55, 53, 46);
const TEXT: Color32 = Color32::from_rgb(218, 213, 201);
const MUTED: Color32 = Color32::from_rgb(139, 133, 119);
const DIM: Color32 = Color32::from_rgb(93, 89, 80);
const ACCENT: Color32 = Color32::from_rgb(95, 163, 195);
const ACCENT_SOFT: Color32 = Color32::from_rgb(150, 192, 214);
const GREEN: Color32 = Color32::from_rgb(128, 176, 125);
const KEY_WHITE: Color32 = Color32::from_rgb(198, 198, 198);
const KEY_WHITE_DOWN: Color32 = Color32::from_rgb(181, 181, 181);
const KEY_BLACK: Color32 = Color32::from_rgb(47, 47, 47);
const KEY_BLACK_DOWN: Color32 = Color32::from_rgb(64, 64, 64);
const KEY_GAP: Color32 = Color32::from_rgb(45, 58, 62);
/// The 1 px rail where the keys meet the keybed chassis.
const KEY_ROOT: Color32 = Color32::from_rgb(0, 201, 255);
/// KONTAKT paints every mapped key (a zone covers it) with `$KEY_COLOR_DEFAULT`, its standard
/// blue, and leaves unmapped keys black and white.
const KEY_WHITE_MAPPED: Color32 = Color32::from_rgb(129, 199, 218);
const KEY_WHITE_MAPPED_DOWN: Color32 = Color32::from_rgb(118, 188, 207);
const KEY_BLACK_MAPPED: Color32 = Color32::from_rgb(31, 101, 120);
const KEY_BLACK_MAPPED_DOWN: Color32 = Color32::from_rgb(32, 80, 93);

/// Where the panel background comes from, and what it currently shows. Used to notice when the
/// instrument changed and the texture has to be rebuilt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BackgroundSource {
    /// Image file named by the program's filename table.
    file: Option<PathBuf>,
    /// Picture resource the script assigned to `$INST_WALLPAPER_ID`.
    script: Option<String>,
}

/// One KSP picture resource: a sprite sheet plus the frame layout from its `.txt` sidecar.
struct PictureResource {
    texture: TextureHandle,
    frames: usize,
    horizontal: bool,
    /// Frame size in native pixels.
    frame: Vec2,
}

/// Parse a picture's `.txt` sidecar: sprite frame count and animation axis.
fn picture_metadata_from_text(text: &str) -> (usize, bool) {
    let mut frames = 1usize;
    let mut horizontal = true;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("number of animations:") {
            frames = rest.trim().parse().unwrap_or(1);
        } else if lower.starts_with("horizontal animation:") {
            horizontal = lower.contains("yes");
        }
    }
    (frames, horizontal)
}

/// Which page the main area shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Library,
    Instrument,
}

pub struct OpenKontaktEditor {
    params: Arc<OpenKontaktParams>,
    shared: Arc<SharedState>,
    open_state: Option<OpenEditorState>,
    background: Option<TextureHandle>,
    background_source: Option<BackgroundSource>,
    /// Instrument header icon a script assigned to `$INST_ICON_ID`.
    header_icon: Option<TextureHandle>,
    header_icon_source: Option<String>,
    /// KSP picture resources shipped by the instrument (`Resources/pictures/*.png`).
    pictures: HashMap<String, PictureResource>,
    pictures_root: Option<PathBuf>,
    show_file_menu: bool,
    show_recent_menu: bool,
    recent_instruments: Vec<PathBuf>,
    file_menu_rect: Option<Rect>,
    file_dialog_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    ksp_values: Vec<(String, f32)>,
    ksp_slots: Vec<(String, usize)>,
    ksp_labels: Vec<(String, String)>,
    /// Menu control currently expanded, if any.
    open_menu: Option<String>,
    /// Instrument whose saved panel values have already been restored.
    panel_restored: Option<String>,
    mouse_note: Option<u8>,
    computer_notes: Vec<egui::Key>,
    /// Instrument-bar solo toggle. With a single instrument it only changes the button state.
    instrument_solo: bool,
    /// A KSP menu was opened this frame; ignore the click that opened it when checking for
    /// click-away closes.
    menu_just_opened: bool,
    /// Page shown below the global strip. The library page is the default, like the original.
    view: ViewMode,
    last_view_load: Option<Instant>,
    /// Decaying disk read activity for the header's DISK meter.
    disk_load: f32,
    last_disk_tick: Instant,
    last_load_marker: Option<Instant>,
}

struct OpenEditorState {
    _egui_ctx: egui::Context,
    gui_context: GuiContext,
}

impl OpenKontaktEditor {
    pub fn new(params: Arc<OpenKontaktParams>, shared: Arc<SharedState>) -> Self {
        Self {
            params,
            shared,
            open_state: None,
            background: None,
            background_source: None,
            header_icon: None,
            header_icon_source: None,
            pictures: HashMap::new(),
            pictures_root: None,
            show_file_menu: false,
            show_recent_menu: false,
            recent_instruments: Vec::new(),
            file_menu_rect: None,
            file_dialog_rx: None,
            ksp_values: Vec::new(),
            ksp_slots: Vec::new(),
            ksp_labels: Vec::new(),
            open_menu: None,
            panel_restored: None,
            mouse_note: None,
            computer_notes: Vec::new(),
            instrument_solo: false,
            menu_just_opened: false,
            view: ViewMode::Library,
            last_view_load: None,
            disk_load: 0.0,
            last_disk_tick: Instant::now(),
            last_load_marker: None,
        }
    }

    fn load_path(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return;
        }
        self.recent_instruments.retain(|recent| recent != &path);
        self.recent_instruments.insert(0, path.clone());
        self.recent_instruments.truncate(8);
        self.shared.spawn_load(path);
        self.view = ViewMode::Instrument;
        self.show_file_menu = false;
        self.show_recent_menu = false;
    }

    fn open_file_dialog(&mut self) {
        self.show_file_menu = false;
        self.show_recent_menu = false;
        if self.file_dialog_rx.is_some() {
            return;
        }

        let current = self.params.path();
        let start_dir = if current.is_empty() {
            None
        } else {
            PathBuf::from(current)
                .parent()
                .filter(|parent| parent.is_dir())
                .map(Path::to_path_buf)
        };

        // The native dialog must not run inline inside the egui draw callback: macOS dispatches
        // the panel to the main queue, which would re-enter baseview's RefCell while it is
        // borrowed. A worker thread blocks on the dialog instead; the GUI polls this channel.
        let (tx, rx) = std::sync::mpsc::channel();
        self.file_dialog_rx = Some(rx);
        std::thread::Builder::new()
            .name("openkontakt-file-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Load Kontakt Instrument")
                    .add_filter("Kontakt instrument", &["nki"]);
                if let Some(directory) = start_dir {
                    dialog = dialog.set_directory(directory);
                }
                let _ = tx.send(dialog.pick_file());
            })
            .ok();
    }

    fn reset_instrument(&mut self) {
        self.shared.loaded.store(None);
        *self.shared.status.lock().unwrap() = LoadStatus::Empty;
        self.shared.all_notes_off();
        self.shared
            .active_voices
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.params.set_instrument("", "");
        self.background = None;
        self.background_source = None;
        self.header_icon = None;
        self.header_icon_source = None;
        self.ksp_values.clear();
    }

    /// Load the instrument's KSP picture resources. They are ordinary PNG sprite sheets with a
    /// `.txt` sidecar describing the frame layout, which is what gives a custom panel its real
    /// control sizes.
    fn ensure_pictures(
        &mut self,
        ctx: &egui::Context,
        loaded: Option<&crate::state::LoadedInstrument>,
    ) {
        let Some(loaded) = loaded else {
            return;
        };
        // Libraries either ship loose `Resources/pictures/*.png` or keep the same files inside a
        // resource archive; `picture_source` names whichever one this library uses.
        let source = loaded.instrument.picture_source();
        if self.pictures_root == source {
            return;
        }
        self.pictures.clear();
        self.pictures_root = source.clone();
        if source.is_none() {
            return;
        }
        for asset in loaded.instrument.picture_assets() {
            let Ok(image) = image::load_from_memory(&asset.image) else {
                continue;
            };
            let (frames, horizontal) = asset
                .text
                .as_deref()
                .map(picture_metadata_from_text)
                .unwrap_or((1, true));
            let size = [image.width() as usize, image.height() as usize];
            if size[0] == 0 || size[1] == 0 {
                continue;
            }
            let frames = frames
                .max(1)
                .min(if horizontal { size[0] } else { size[1] });
            let frame = if horizontal {
                Vec2::new(size[0] as f32 / frames as f32, size[1] as f32)
            } else {
                Vec2::new(size[0] as f32, size[1] as f32 / frames as f32)
            };
            let rgba = image.to_rgba8();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let key = asset.stem.to_ascii_lowercase();
            let texture = ctx.load_texture(
                format!("openkontakt-picture-{key}"),
                color_image,
                TextureOptions::LINEAR,
            );
            self.pictures.insert(
                key,
                PictureResource {
                    texture,
                    frames,
                    horizontal,
                    frame,
                },
            );
        }
    }

    /// Load the instrument's panel background.
    ///
    /// There are two sources. A library can point at an image file from the program's filename
    /// table, or — as every Kontakt 2-era library does — build its panel at runtime with
    /// `set_control_par_str($INST_WALLPAPER_ID, $CONTROL_PAR_PICTURE, "skin")`, in which case the
    /// image is just another picture resource. Neither is optional for the panel to look right.
    fn ensure_background(
        &mut self,
        ctx: &egui::Context,
        loaded: Option<&crate::state::LoadedInstrument>,
    ) {
        let icon = loaded.and_then(|loaded| loaded.ksp.engine.icon_picture());
        if icon != self.header_icon_source {
            self.header_icon = icon
                .as_ref()
                .and_then(|name| self.pictures.get(&name.to_ascii_lowercase()))
                .map(|resource| resource.texture.clone());
            self.header_icon_source = icon;
        }

        let desired = BackgroundSource {
            file: loaded.and_then(|loaded| loaded.instrument.wallpaper_path.clone()),
            script: loaded.and_then(|loaded| loaded.ksp.engine.wallpaper_picture()),
        };
        if self.background_source.as_ref() == Some(&desired) {
            return;
        }
        self.background = None;
        self.background_source = Some(desired.clone());

        if let Some(path) = &desired.file {
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            let Ok(image) = image::load_from_memory(&bytes) else {
                return;
            };
            let rgba = image.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            self.background = Some(ctx.load_texture(
                format!("openkontakt-wallpaper-{}", path.display()),
                color_image,
                TextureOptions::LINEAR,
            ));
            return;
        }

        if let Some(name) = &desired.script {
            self.background = self
                .pictures
                .get(&name.to_ascii_lowercase())
                .map(|resource| resource.texture.clone());
        }
    }

    fn release_held_notes(&mut self) {
        if let Some(note) = self.mouse_note.take() {
            self.shared.send_note(note, 0.0, false);
        }
        for key in self.computer_notes.drain(..) {
            if let Some(note) = computer_key_note(key) {
                self.shared.send_note(note, 0.0, false);
            }
        }
    }

    /// Mirror the executed script state: values come from the interpreter, not from the
    /// declarations, so a slider the script moved (or a persisted value) shows correctly.
    fn sync_ksp(&mut self, loaded: &LoadedInstrument) {
        let mut values = Vec::new();
        let mut slots = Vec::new();
        let mut labels = Vec::new();
        let mut state = String::new();
        for slot in 0..loaded.ksp.slot_count() {
            for control in loaded.ksp.controls(slot) {
                let name = format!("${}", control.name);
                values.push((name.clone(), control.normalized()));
                slots.push((name.clone(), slot));
                labels.push((name, control.label.clone()));
                state.push_str(&format!("{slot}:{}={};", control.name, control.value));
            }
        }
        self.ksp_values = values;
        self.ksp_slots = slots;
        self.ksp_labels = labels;

        // Remember the panel as it stands, so a reload restores what the user set rather than
        // what the .nki was saved with. Keyed by instrument path: a different library must not
        // inherit these values.
        let path = loaded.instrument.source_path.display().to_string();
        let serialised = format!("{path}|{state}");
        if let Ok(mut stored) = self.params.panel_values.lock() {
            if *stored != serialised {
                *stored = serialised;
            }
        }
    }

    /// Apply panel values saved with the session, once per instrument.
    fn restore_panel_values(&mut self, loaded: &LoadedInstrument) {
        let path = loaded.instrument.source_path.display().to_string();
        // The marker is cleared whenever a different instrument is loaded, so returning to an
        // instrument restores its values again.
        if let Some(previous) = self.panel_restored.as_ref() {
            if previous != &path {
                self.panel_restored = None;
            }
        }
        if self.panel_restored.as_deref() == Some(path.as_str()) {
            return;
        }
        self.panel_restored = Some(path.clone());
        let Ok(stored) = self.params.panel_values.lock() else {
            return;
        };
        let Some(rest) = stored.strip_prefix(&format!("{path}|")) else {
            return;
        };
        for entry in rest.split(';').filter(|entry| !entry.is_empty()) {
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };
            let Some((slot, name)) = key.split_once(':') else {
                continue;
            };
            if let (Ok(slot), Ok(value)) = (slot.parse::<usize>(), value.parse::<i64>()) {
                loaded.ksp.set_control_value(slot, name, value);
            }
        }
    }
}

impl OpenKontaktEditor {
    /// Forget every texture created for the previous editor session.
    fn forget_textures(&mut self) {
        self.background = None;
        self.background_source = None;
        self.header_icon = None;
        self.header_icon_source = None;
        self.pictures.clear();
        self.pictures_root = None;
    }
}

impl NiceEguiApp for OpenKontaktEditor {
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        _frame: &mut nice_plug_egui::Frame,
    ) -> Result<(), nice_plug_egui::baseview::HandlerError> {
        // Opening the editor again creates a brand new egui context. Every texture handle cached
        // during the previous session is dead in the new context, and drawing with it shows up as
        // a black KSP panel, so drop them all and let `ensure_pictures`/`ensure_background`
        // rebuild them from disk.
        self.forget_textures();
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = Color32::from_rgb(14, 14, 12);
        visuals.faint_bg_color = PANEL_LIGHT;
        visuals.override_text_color = Some(TEXT);
        visuals.widgets.inactive.bg_fill = PANEL_LIGHT;
        visuals.widgets.inactive.weak_bg_fill = PANEL_LIGHT;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(44, 48, 52);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_SOFT);
        visuals.widgets.active.bg_fill = Color32::from_rgb(52, 57, 62);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        visuals.selection.stroke = Stroke::new(1.0, TEXT);
        egui_ctx.set_visuals(visuals);

        egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = Vec2::new(6.0, 5.0);
            style.spacing.window_margin = egui::Margin::same(0);
            style.visuals.window_shadow = egui::epaint::Shadow::NONE;
        });

        self.open_state = Some(OpenEditorState {
            _egui_ctx: egui_ctx,
            gui_context: nice_gui_ctx,
        });
        Ok(())
    }

    fn editor_closed(&mut self) {
        self.release_held_notes();
        self.open_state = None;
        self.forget_textures();
        self.show_file_menu = false;
        self.show_recent_menu = false;
        self.file_menu_rect = None;
        self.file_dialog_rx = None;
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut nice_plug_egui::Frame) {
        let Some(gui_context) = self
            .open_state
            .as_ref()
            .map(|state| state.gui_context.clone())
        else {
            return;
        };
        let setter = gui_context.param_setter();
        if self.file_dialog_rx.is_none()
            && ui.input(|input| {
                (input.modifiers.command || input.modifiers.ctrl) && input.key_pressed(egui::Key::O)
            })
        {
            self.open_file_dialog();
        }
        let full = ui.max_rect();
        let scale = (full.width() / DESIGN_WIDTH)
            .min(full.height() / DESIGN_HEIGHT)
            .max(0.5);
        let size = Vec2::new(DESIGN_WIDTH * scale, DESIGN_HEIGHT * scale);
        let origin = full.center() - size * 0.5;
        let view = Rect::from_min_size(origin, size);

        ui.painter()
            .rect_filled(full, 0.0, Color32::from_rgb(8, 8, 7));
        ui.painter().rect_filled(view, 0.0, BG);

        // Drag and drop.
        let dropped: Vec<PathBuf> = ui.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.into_iter().find(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("nki"))
        }) {
            self.load_path(path);
        }

        // On-screen and computer keyboard input. The modal grabs keyboard focus, so do not play
        // notes while the user is typing a path.
        if !self.show_file_menu && !self.show_recent_menu && !ui.ctx().egui_wants_keyboard_input() {
            let (pressed, released) = ui.input(|input| {
                let mapping = computer_keyboard();
                let pressed: Vec<egui::Key> = mapping
                    .iter()
                    .map(|(key, _)| *key)
                    .filter(|key| input.key_pressed(*key))
                    .collect();
                let released: Vec<egui::Key> = mapping
                    .iter()
                    .map(|(key, _)| *key)
                    .filter(|key| input.key_released(*key))
                    .collect();
                (pressed, released)
            });
            for key in pressed {
                if !self.computer_notes.contains(&key) {
                    self.computer_notes.push(key);
                    if let Some(note) = computer_key_note(key) {
                        self.shared.send_note(note, 0.85, true);
                    }
                }
            }
            for key in released {
                if let Some(index) = self.computer_notes.iter().position(|held| *held == key) {
                    self.computer_notes.remove(index);
                    if let Some(note) = computer_key_note(key) {
                        self.shared.send_note(note, 0.0, false);
                    }
                }
            }
        } else if !self.computer_notes.is_empty() {
            self.release_held_notes();
        }

        let loaded = self.shared.current();
        if let Some(loaded) = loaded.as_ref() {
            if self.last_view_load != Some(loaded.loaded_at) {
                self.last_view_load = Some(loaded.loaded_at);
                self.view = ViewMode::Instrument;
            }
        }
        let status = self.shared.status.lock().unwrap().clone();
        if let LoadStatus::Ready { path, name, .. } = &status {
            if self.params.path() != path.display().to_string() {
                self.params
                    .set_instrument(&path.display().to_string(), name);
            }
            if self.recent_instruments.first() != Some(path) {
                self.recent_instruments.retain(|recent| recent != path);
                self.recent_instruments.insert(0, path.clone());
                self.recent_instruments.truncate(8);
            }
        }
        let dialog_result = self
            .file_dialog_rx
            .as_ref()
            .map(|receiver| receiver.try_recv());
        match dialog_result {
            Some(Ok(Some(path))) => {
                self.file_dialog_rx = None;
                self.load_path(path);
            }
            Some(Ok(None)) => {
                self.file_dialog_rx = None;
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.file_dialog_rx = None;
            }
            _ => {}
        }

        // Pictures first: a script-assigned wallpaper is one of them.
        self.ensure_pictures(ui.ctx(), loaded.as_deref());
        self.ensure_background(ui.ctx(), loaded.as_deref());
        if let Some(loaded) = loaded.as_ref() {
            self.restore_panel_values(loaded.as_ref());
            self.sync_ksp(loaded.as_ref());
        }

        let mut p = Painter { ui, origin, scale };

        self.draw_header(&p);
        if self.view == ViewMode::Instrument {
            self.draw_global_bar(&p, &setter);
        }
        match self.view {
            ViewMode::Instrument => {
                self.draw_sidebar(&p, &status, &setter);
                self.draw_main(&p, &status, loaded.as_deref());
            }
            ViewMode::Library => self.draw_library(&p),
        }
        self.draw_status_bar(&p, &status, loaded.as_deref());
        self.draw_file_menu(&mut p);

        let busy = status.is_loading()
            || self
                .shared
                .active_voices
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
            || self.shared.active_notes[0].load(std::sync::atomic::Ordering::Relaxed) != 0
            || self.shared.active_notes[1].load(std::sync::atomic::Ordering::Relaxed) != 0;
        if busy {
            // `request_repaint_after` is not always honored by the standalone event loop, so drive
            // animations and status transitions with an explicit repaint while work is happening.
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
    }
}

struct Painter<'a> {
    ui: &'a mut Ui,
    origin: Pos2,
    scale: f32,
}

impl Painter<'_> {
    fn pos(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.pos(x, y), Vec2::new(w, h) * self.scale)
    }

    fn s(&self, value: f32) -> f32 {
        value * self.scale
    }

    fn text(
        &self,
        painter: &egui::Painter,
        x: f32,
        y: f32,
        text: impl ToString,
        size: f32,
        color: Color32,
        align: Align2,
    ) {
        painter.text(
            self.pos(x, y),
            align,
            text.to_string(),
            FontId::new(self.s(size), FontFamily::Proportional),
            color,
        );
    }

    /// Text on its own translucent pill. Kontakt gives every knob caption its own backdrop, so the
    /// name and the value do not share one box.
    fn mono(
        &self,
        painter: &egui::Painter,
        x: f32,
        y: f32,
        text: impl ToString,
        size: f32,
        color: Color32,
        align: Align2,
    ) {
        painter.text(
            self.pos(x, y),
            align,
            text.to_string(),
            FontId::new(self.s(size), FontFamily::Monospace),
            color,
        );
    }

    fn button(
        &self,
        id: impl std::hash::Hash + std::fmt::Debug,
        rect: Rect,
        label: &str,
        active: bool,
        accent: bool,
    ) -> Response {
        let response = self
            .ui
            .interact(rect, self.ui.scope_id().with(id), Sense::click());
        let hover = response.hovered();
        let fill = if active || accent {
            ACCENT.gamma_multiply(if hover { 0.9 } else { 0.78 })
        } else if hover {
            Color32::from_rgb(44, 48, 52)
        } else {
            PANEL_LIGHT
        };
        self.ui.painter().rect_filled(rect, self.s(4.0), fill);
        if !accent && !active {
            self.ui.painter().rect_stroke(
                rect,
                self.s(4.0),
                Stroke::new(1.0, LINE),
                StrokeKind::Inside,
            );
        }
        self.text(
            self.ui.painter(),
            rect.center().x / self.scale - self.origin.x / self.scale,
            rect.center().y / self.scale - self.origin.y / self.scale,
            label,
            11.0,
            if accent || active {
                Color32::from_rgb(18, 27, 32)
            } else {
                TEXT
            },
            Align2::CENTER_CENTER,
        );
        if hover {
            self.ui
                .ctx()
                .set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

impl OpenKontaktEditor {
    fn draw_header(&mut self, p: &Painter<'_>) {
        let painter = p.ui.painter();
        painter.rect_filled(
            p.rect(0.0, 0.0, DESIGN_WIDTH, HEADER_HEIGHT),
            0.0,
            Color32::from_rgb(21, 21, 19),
        );
        painter.line_segment(
            [
                p.pos(0.0, HEADER_HEIGHT),
                p.pos(DESIGN_WIDTH, HEADER_HEIGHT),
            ],
            Stroke::new(1.0, LINE),
        );
        let logo_rect = p.rect(10.0, 6.0, 242.0, 38.0);
        let logo_response = p.ui.interact(
            logo_rect,
            p.ui.scope_id().with("openkontakt-logo-menu"),
            Sense::click(),
        );
        if logo_response.hovered() {
            p.ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if logo_response.clicked() {
            self.release_held_notes();
            self.show_file_menu = !self.show_file_menu;
            self.show_recent_menu = false;
        }
        p.text(
            painter,
            18.0,
            25.0,
            "OPENKONTAKT",
            20.0,
            if self.show_file_menu {
                ACCENT_SOFT
            } else {
                TEXT
            },
            Align2::LEFT_CENTER,
        );
        // The original uses two view icons: a play triangle for the instrument and a stack of
        // books for the library browser.
        for (view, x, id) in [
            (ViewMode::Instrument, 350.0, "view-instrument"),
            (ViewMode::Library, 392.0, "view-library"),
        ] {
            let rect = p.rect(x, 11.0, 34.0, 30.0);
            let response =
                p.ui.interact(rect, p.ui.scope_id().with(id), Sense::click());
            if response.hovered() {
                painter.rect_filled(rect, p.s(3.0), Color32::from_rgb(38, 38, 34));
                p.ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let color = if self.view == view { TEXT } else { MUTED };
            if view == ViewMode::Instrument {
                draw_play_icon(painter, p, rect.center(), color);
            } else {
                draw_library_icon(painter, p, rect, color);
            }
            if response.clicked() {
                self.release_held_notes();
                self.view = view;
                self.show_file_menu = false;
                self.show_recent_menu = false;
            }
        }

        // Status cluster, icon + value rows as in the original: voices/memory, then CPU/DISK.
        if let Some(loaded) = self.shared.current() {
            if self.last_load_marker != Some(loaded.loaded_at) {
                self.last_load_marker = Some(loaded.loaded_at);
                let megabytes = loaded.bytes_loaded as f64 / 1e6;
                let seconds = (loaded.elapsed_ms as f64 / 1000.0).max(0.001);
                // Disk read throughput while the instrument was loading, relative to a 200 MB/s
                // reference. It decays back to zero once the load is finished.
                self.disk_load = ((megabytes / seconds) / 200.0 * 100.0).clamp(0.0, 100.0) as f32;
            }
        }
        let now = Instant::now();
        let elapsed = (now - self.last_disk_tick).as_secs_f32().clamp(0.0, 1.0);
        self.last_disk_tick = now;
        self.disk_load *= (-elapsed / 0.8).exp();

        let voices = self
            .shared
            .active_voices
            .load(std::sync::atomic::Ordering::Relaxed);
        draw_voice_icon(painter, p, 962.0, 17.0);
        p.mono(
            painter,
            976.0,
            17.0,
            voices.to_string(),
            9.0,
            TEXT,
            Align2::LEFT_CENTER,
        );
        let memory = self
            .shared
            .current()
            .map(|loaded| loaded.bytes_loaded as f64 / 1e6)
            .unwrap_or(0.0);
        draw_memory_icon(painter, p, 962.0, 33.0);
        p.mono(
            painter,
            976.0,
            33.0,
            format!("{memory:.1}M"),
            9.0,
            TEXT,
            Align2::LEFT_CENTER,
        );

        let cpu = self
            .shared
            .cpu_load
            .load(std::sync::atomic::Ordering::Relaxed);
        draw_cpu_icon(painter, p, 1058.0, 17.0);
        p.mono(
            painter,
            1072.0,
            17.0,
            format!("{cpu:.1} %"),
            9.0,
            TEXT,
            Align2::LEFT_CENTER,
        );
        draw_disk_icon(painter, p, 1058.0, 33.0);
        p.mono(
            painter,
            1072.0,
            33.0,
            format!("{:.1} %", self.disk_load),
            9.0,
            TEXT,
            Align2::LEFT_CENTER,
        );

        // The global Vol/Tune knobs live in their own strip below the logo, like the original.

        // The panic button.
        let panic = p.rect(1152.0, 15.0, 18.0, 22.0);
        if p.button("panic", panic, "!", false, false).clicked() {
            self.shared.request_panic();
        }
    }

    /// The strip under the logo bar with the master controls, in the order of the original:
    /// Global Vol followed by Global Tune.
    fn draw_global_bar(&mut self, p: &Painter<'_>, setter: &ParamSetter<'_>) {
        let painter = p.ui.painter();
        painter.rect_filled(
            p.rect(0.0, HEADER_HEIGHT, DESIGN_WIDTH, GLOBAL_ROW_HEIGHT),
            0.0,
            Color32::from_rgb(23, 23, 20),
        );
        painter.line_segment(
            [p.pos(0.0, CONTENT_TOP), p.pos(DESIGN_WIDTH, CONTENT_TOP)],
            Stroke::new(1.0, LINE),
        );
        if self.view == ViewMode::Instrument {
            painter.line_segment(
                [
                    p.pos(SIDEBAR_WIDTH, HEADER_HEIGHT),
                    p.pos(SIDEBAR_WIDTH, CONTENT_TOP),
                ],
                Stroke::new(1.0, LINE),
            );
        }

        self.global_knob(
            p,
            SIDEBAR_WIDTH + 18.0,
            &self.params.master_volume,
            setter,
            "Global Vol",
        );
        self.global_knob(
            p,
            SIDEBAR_WIDTH + 168.0,
            &self.params.tune,
            setter,
            "Global Tune",
        );
    }

    /// A knob with its name and current value to the right, as in the original global strip.
    fn global_knob(
        &self,
        p: &Painter<'_>,
        x: f32,
        param: &FloatParam,
        setter: &ParamSetter<'_>,
        label: &str,
    ) {
        let knob = p.rect(x, HEADER_HEIGHT + 6.0, 32.0, 32.0);
        let (response, normalized) = self.knob_drag(p, knob, param, setter, ("global-knob", label));
        draw_gap_knob(p.ui.painter(), p, knob, normalized);
        let value = param.normalized_value_to_string(normalized, true);
        let painter = p.ui.painter();
        p.text(
            painter,
            x + 40.0,
            HEADER_HEIGHT + 15.0,
            label,
            8.5,
            MUTED,
            Align2::LEFT_CENTER,
        );
        p.text(
            painter,
            x + 40.0,
            HEADER_HEIGHT + 31.0,
            &value,
            10.5,
            TEXT,
            Align2::LEFT_CENTER,
        );
        response.on_hover_text(format!("{label}\n{value}"));
    }

    /// The browser page: search field and category tabs. Adding libraries is not implemented yet,
    /// so the tile grid stays empty.
    fn draw_library(&mut self, p: &Painter<'_>) {
        let painter = p.ui.painter();
        painter.rect_filled(
            p.rect(
                0.0,
                HEADER_HEIGHT,
                DESIGN_WIDTH,
                DESIGN_HEIGHT - HEADER_HEIGHT,
            ),
            0.0,
            Color32::from_rgb(20, 20, 18),
        );

        let search = p.rect(24.0, HEADER_HEIGHT + 20.0, DESIGN_WIDTH - 48.0, 34.0);
        painter.rect_filled(search, p.s(4.0), Color32::from_rgb(31, 31, 28));
        painter.rect_stroke(search, p.s(4.0), Stroke::new(1.0, LINE), StrokeKind::Inside);
        let lens = p.pos(42.0, HEADER_HEIGHT + 37.0);
        painter.circle_stroke(lens, p.s(4.2), Stroke::new(1.2, MUTED));
        painter.line_segment(
            [
                lens + Vec2::new(p.s(3.0), p.s(3.0)),
                lens + Vec2::new(p.s(6.2), p.s(6.2)),
            ],
            Stroke::new(1.2, MUTED),
        );
        p.text(
            painter,
            58.0,
            HEADER_HEIGHT + 37.0,
            "Search",
            11.0,
            DIM,
            Align2::LEFT_CENTER,
        );

        let tabs = [
            ("Instruments", 26.0, true),
            ("Combined", 122.0, false),
            ("Tools", 214.0, false),
            ("Loops", 268.0, false),
            ("One-shots", 332.0, false),
        ];
        for (label, x, active) in tabs {
            p.text(
                painter,
                x,
                HEADER_HEIGHT + 82.0,
                label,
                11.0,
                if active { TEXT } else { MUTED },
                Align2::LEFT_CENTER,
            );
        }
        painter.line_segment(
            [
                p.pos(26.0, HEADER_HEIGHT + 93.0),
                p.pos(108.0, HEADER_HEIGHT + 93.0),
            ],
            Stroke::new(2.0, ACCENT),
        );
        painter.line_segment(
            [
                p.pos(0.0, HEADER_HEIGHT + 99.0),
                p.pos(DESIGN_WIDTH, HEADER_HEIGHT + 99.0),
            ],
            Stroke::new(1.0, LINE),
        );

        p.text(
            painter,
            DESIGN_WIDTH * 0.5,
            HEADER_HEIGHT + 250.0,
            "No libraries added yet",
            12.0,
            DIM,
            Align2::CENTER_CENTER,
        );
    }

    fn draw_sidebar(&mut self, p: &Painter<'_>, status: &LoadStatus, setter: &ParamSetter<'_>) {
        let painter = p.ui.painter();
        let sidebar = p.rect(0.0, CONTENT_TOP, SIDEBAR_WIDTH, DESIGN_HEIGHT - CONTENT_TOP);
        painter.rect_filled(sidebar, 0.0, Color32::from_rgb(20, 20, 18));
        painter.line_segment(
            [
                p.pos(SIDEBAR_WIDTH, CONTENT_TOP),
                p.pos(SIDEBAR_WIDTH, DESIGN_HEIGHT),
            ],
            Stroke::new(1.0, LINE),
        );

        // Instrument slot: the old bottom instrument navigator folded into the top bar, as in
        // the original. Square artwork, name, solo/mute, then the pan and volume knobs.
        let slot = p.rect(12.0, 102.0, 316.0, 60.0);
        painter.rect_filled(slot, p.s(3.0), Color32::from_rgb(27, 27, 25));
        painter.rect_stroke(slot, p.s(3.0), Stroke::new(1.0, ACCENT), StrokeKind::Inside);

        let thumb = p.rect(18.0, 110.0, 44.0, 44.0);
        painter.rect_filled(thumb, p.s(2.0), Color32::from_rgb(38, 38, 35));
        if let Some(texture) = &self.background {
            // Square, center-cropped library artwork.
            let size = texture.size_vec2();
            let side = size.x.min(size.y);
            let uv = Rect::from_min_max(
                Pos2::new(
                    (size.x - side) / (2.0 * size.x),
                    (size.y - side) / (2.0 * size.y),
                ),
                Pos2::new(
                    (size.x + side) / (2.0 * size.x),
                    (size.y + side) / (2.0 * size.y),
                ),
            );
            painter
                .with_clip_rect(thumb)
                .image(texture.id(), thumb, uv, Color32::WHITE);
        }

        let name = match status {
            LoadStatus::Empty => {
                let cached = self.params.name();
                if cached.is_empty() {
                    "No instrument".to_string()
                } else {
                    cached
                }
            }
            LoadStatus::Loading { .. } => "Loading\u{2026}".to_string(),
            LoadStatus::Ready { name, .. } => name.clone(),
            LoadStatus::Failed { .. } => "Load failed".to_string(),
        };
        p.text(
            painter,
            78.0,
            132.0,
            truncate(&name, 20),
            11.0,
            TEXT,
            Align2::LEFT_CENTER,
        );

        let muted = self
            .shared
            .instrument_muted
            .load(std::sync::atomic::Ordering::Relaxed);
        if p.button(
            "slot-s",
            p.rect(196.0, 122.0, 18.0, 20.0),
            "S",
            self.instrument_solo,
            false,
        )
        .clicked()
        {
            self.instrument_solo = !self.instrument_solo;
        }
        if p.button(
            "slot-m",
            p.rect(218.0, 122.0, 18.0, 20.0),
            "M",
            muted,
            false,
        )
        .clicked()
        {
            self.shared
                .instrument_muted
                .store(!muted, std::sync::atomic::Ordering::Relaxed);
        }
        self.mini_knob(
            p,
            p.rect(255.0, 121.0, 20.0, 20.0),
            &self.params.pan,
            setter,
            "Pan",
            true,
        );
        self.mini_knob(
            p,
            p.rect(289.0, 121.0, 20.0, 20.0),
            &self.params.volume,
            setter,
            "Volume",
            false,
        );

        // The library list (Instruments / Combined / …) is not implemented yet and stays empty.
    }

    fn draw_main(
        &mut self,
        p: &Painter<'_>,
        status: &LoadStatus,
        loaded: Option<&crate::state::LoadedInstrument>,
    ) {
        let painter = p.ui.painter();
        let main_x = SIDEBAR_WIDTH;
        let main_w = DESIGN_WIDTH - SIDEBAR_WIDTH;

        // Instrument header.
        painter.rect_filled(
            p.rect(main_x, CONTENT_TOP, main_w, INSTRUMENT_HEADER_HEIGHT),
            0.0,
            Color32::from_rgb(22, 22, 19),
        );
        painter.line_segment(
            [
                p.pos(main_x, CONTENT_TOP + INSTRUMENT_HEADER_HEIGHT),
                p.pos(DESIGN_WIDTH, CONTENT_TOP + INSTRUMENT_HEADER_HEIGHT),
            ],
            Stroke::new(1.0, LINE),
        );

        let (name, subtitle) = match (loaded, status) {
            (Some(loaded), _) => (
                loaded.instrument.name.clone(),
                format!(
                    "Program 0x{:02X}  ·  {} groups  ·  {} zones  ·  {} sample files",
                    loaded.instrument.program.version,
                    loaded.instrument.groups.len(),
                    loaded.instrument.zones.len(),
                    loaded.samples.len()
                ),
            ),
            (None, LoadStatus::Loading { path, .. }) => (
                "Loading instrument…".to_string(),
                path.display().to_string(),
            ),
            (None, LoadStatus::Failed { message, .. }) => {
                ("OpenKontakt".to_string(), message.clone())
            }
            _ => (
                "No instrument loaded".to_string(),
                "Open a .nki with loose WAV samples to begin".to_string(),
            ),
        };
        // A library may ship its own header icon through `$INST_ICON_ID`; draw it in front of
        // the instrument name, which is where the browser shows it too.
        let mut text_x = main_x + 22.0;
        if let Some(icon) = self.header_icon.clone() {
            let side = 22.0;
            let center_y = CONTENT_TOP + 26.0;
            painter.image(
                icon.id(),
                p.rect(text_x - 2.0, center_y - side * 0.5, side, side),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            text_x += side + 12.0;
        }
        p.text(
            painter,
            text_x,
            CONTENT_TOP + 22.0,
            truncate(&name, 52),
            16.0,
            TEXT,
            Align2::LEFT_CENTER,
        );
        p.text(
            painter,
            text_x,
            CONTENT_TOP + 42.0,
            truncate(&subtitle, 72),
            9.0,
            MUTED,
            Align2::LEFT_CENTER,
        );

        // Solo/mute on the right side of the instrument header, as in the original rack.
        let muted = self
            .shared
            .instrument_muted
            .load(std::sync::atomic::Ordering::Relaxed);
        if p.button(
            "header-s",
            p.rect(DESIGN_WIDTH - 112.0, CONTENT_TOP + 21.0, 18.0, 20.0),
            "S",
            self.instrument_solo,
            false,
        )
        .clicked()
        {
            self.instrument_solo = !self.instrument_solo;
        }
        if p.button(
            "header-m",
            p.rect(DESIGN_WIDTH - 90.0, CONTENT_TOP + 21.0, 18.0, 20.0),
            "M",
            muted,
            false,
        )
        .clicked()
        {
            self.shared
                .instrument_muted
                .store(!muted, std::sync::atomic::Ordering::Relaxed);
        }

        // Artwork.
        let art = p.rect(
            main_x,
            CONTENT_TOP + INSTRUMENT_HEADER_HEIGHT,
            main_w,
            DESIGN_HEIGHT - KEYBOARD_HEIGHT - CONTENT_TOP - INSTRUMENT_HEADER_HEIGHT,
        );
        painter.rect_filled(art, 0.0, Color32::from_rgb(22, 15, 11));

        // The wallpaper is the KSP performance panel: it is drawn 1:1 with the panel's own aspect,
        // fits the frame left-to-right, and leaves whatever is below it empty. Kontakt never
        // stretches or crops it, and the panel's controls use the same origin and width scale,
        // which is why a cropped background made every control look misplaced.
        // The picture keeps its own aspect and is anchored to the *bottom* of the KSP panel:
        // Kontakt never stretches it horizontally, and the panel is the viewport, so the strip
        // that does not fit is cut from the top. That is what makes the controls line up with the
        // labels baked into the image — a top-anchored or cropped-to-fill image does not.
        if let Some(texture) = &self.background {
            let ui_width = loaded.map_or(632.0, |loaded| {
                loaded.instrument.ksp_panel.ui_width_px.max(1.0)
            });
            // The height a panel declares is often missing: `set_ui_height(4)` sizes it in grid
            // rows and never reaches `ui_height_px`. Falling back to 1 px there clipped the
            // wallpaper to a two-pixel strip, which reads as a black panel. When nothing is
            // declared the picture itself is the panel, so its own aspect is the honest answer.
            let declared_height =
                loaded.map_or(0.0, |loaded| loaded.instrument.ksp_panel.ui_height_px);
            let image_size = texture.size_vec2();
            let ui_height = if declared_height > 0.0 {
                declared_height
            } else if image_size.x > 0.0 {
                image_size.y * ui_width / image_size.x
            } else {
                loaded
                    .and_then(|loaded| loaded.ksp.engine.panel_height())
                    .unwrap_or(400.0)
            };
            let scale = art.width() / ui_width;
            let panel = Rect::from_min_size(
                art.min,
                Vec2::new(art.width(), (ui_height * scale).min(art.height())),
            );
            let image_height = image_size.y * scale;
            let image = Rect::from_min_size(
                Pos2::new(art.min.x, panel.max.y - image_height),
                Vec2::new(image_size.x * scale, image_height),
            );
            painter.with_clip_rect(panel).image(
                texture.id(),
                image,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else if let Some(name) = self
            .background_source
            .as_ref()
            .and_then(|source| source.script.as_deref())
        {
            // The script asked for a panel image this library does not ship as a loose file or
            // in its resource archive. Say so instead of leaving a blank black panel.
            p.text(
                painter,
                art.min.x + 14.0,
                art.max.y - 16.0,
                &format!(
                    "panel picture \"{name}\" not found in Resources/pictures/ or the .nkr archive"
                ),
                9.0,
                Color32::from_rgb(150, 96, 60),
                Align2::LEFT_CENTER,
            );
        }

        painter.rect_stroke(
            art,
            0.0,
            Stroke::new(1.0, Color32::from_rgb(70, 59, 43)),
            StrokeKind::Inside,
        );

        if let Some(loaded) = loaded {
            self.draw_ksp_panel(p, loaded, art);
        } else {
            match status {
                LoadStatus::Loading { path, started } => {
                    self.draw_loading_state(p, art, path, *started);
                }
                LoadStatus::Failed { message, .. } => {
                    self.draw_failed_state(p, art, message);
                }
                _ => self.draw_empty_state(p, art),
            }
        }

        // Keyboard separator.
        painter.line_segment(
            [
                p.pos(main_x, art.max.y / p.scale - p.origin.y / p.scale),
                p.pos(DESIGN_WIDTH, art.max.y / p.scale - p.origin.y / p.scale),
            ],
            Stroke::new(1.0, LINE),
        );
        self.draw_keyboard(
            p,
            Rect::from_min_max(
                p.pos(main_x, DESIGN_HEIGHT - KEYBOARD_HEIGHT),
                p.pos(DESIGN_WIDTH, DESIGN_HEIGHT),
            ),
        );
    }

    fn draw_empty_state(&mut self, p: &Painter<'_>, art: Rect) {
        let painter = p.ui.painter();
        let center = art.center();
        painter.circle_filled(
            center - Vec2::new(0.0, p.s(40.0)),
            p.s(42.0),
            Color32::from_rgba_unmultiplied(10, 9, 8, 160),
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale - 44.0,
            "♪",
            46.0,
            ACCENT_SOFT,
            Align2::CENTER_CENTER,
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 10.0,
            "OPEN A KONTAKT INSTRUMENT",
            18.0,
            TEXT,
            Align2::CENTER_CENTER,
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 34.0,
            "Drop a .nki here, or use FILE → open  ·  NISound + loose WAV only",
            10.0,
            MUTED,
            Align2::CENTER_CENTER,
        );
        let button_rect = p.rect(
            center.x / p.scale - p.origin.x / p.scale - 70.0,
            center.y / p.scale - p.origin.y / p.scale + 64.0,
            140.0,
            32.0,
        );
        if p.button("empty-open", button_rect, "OPEN INSTRUMENT", false, true)
            .clicked()
        {
            self.release_held_notes();
            self.open_file_dialog();
        }
    }

    fn draw_loading_state(
        &self,
        p: &Painter<'_>,
        art: Rect,
        path: &std::path::Path,
        started: std::time::Instant,
    ) {
        let painter = p.ui.painter();
        let center = art.center();
        let elapsed = started.elapsed().as_secs_f32();
        let spin = (self.ksp_values.len() as f32 + elapsed * 2.0) * 1.8;
        let radius = p.s(24.0);
        let mut points = Vec::new();
        for step in 0..=18 {
            let angle = spin + step as f32 / 18.0 * std::f32::consts::PI * 1.5;
            points.push(
                center - Vec2::new(0.0, p.s(48.0)) + Vec2::new(angle.cos(), angle.sin()) * radius,
            );
        }
        painter.add(egui::Shape::line(points, Stroke::new(p.s(3.0), ACCENT)));
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 4.0,
            "LOADING INSTRUMENT",
            16.0,
            TEXT,
            Align2::CENTER_CENTER,
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 28.0,
            truncate(
                &path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("instrument.nki")
                    .to_string(),
                50,
            ),
            10.0,
            MUTED,
            Align2::CENTER_CENTER,
        );
        p.mono(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 50.0,
            format!("{elapsed:.1}s  ·  decoding WAV into RAM"),
            8.5,
            DIM,
            Align2::CENTER_CENTER,
        );
    }

    fn draw_failed_state(&self, p: &Painter<'_>, art: Rect, message: &str) {
        let painter = p.ui.painter();
        let center = art.center();
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale - 26.0,
            "COULD NOT LOAD INSTRUMENT",
            17.0,
            ACCENT,
            Align2::CENTER_CENTER,
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 2.0,
            truncate(message, 90),
            10.0,
            MUTED,
            Align2::CENTER_CENTER,
        );
        p.text(
            painter,
            center.x / p.scale - p.origin.x / p.scale,
            center.y / p.scale - p.origin.y / p.scale + 26.0,
            "Use the logo menu to choose another file.",
            9.0,
            DIM,
            Align2::CENTER_CENTER,
        );
    }

    fn draw_ksp_panel(&mut self, p: &Painter<'_>, loaded: &LoadedInstrument, art: Rect) {
        self.menu_just_opened = false;
        let panel = &loaded.instrument.ksp_panel;
        // Draw the *executed* state, not the declarations: scripts move and relabel controls from
        // their callbacks, which is what makes a library panel animate.
        let live: Vec<KspControl> = (0..loaded.ksp.slot_count())
            .flat_map(|slot| loaded.ksp.controls(slot))
            .map(|control| KspControl {
                name: format!("${}", control.name),
                kind: match control.kind {
                    openkontakt::ksp::parser::UiKind::Knob => KspControlKind::Knob,
                    openkontakt::ksp::parser::UiKind::Slider => KspControlKind::Slider,
                    openkontakt::ksp::parser::UiKind::Label => KspControlKind::Label,
                    openkontakt::ksp::parser::UiKind::Menu => KspControlKind::Menu,
                    openkontakt::ksp::parser::UiKind::Switch => KspControlKind::Switch,
                    openkontakt::ksp::parser::UiKind::Button => KspControlKind::Button,
                    openkontakt::ksp::parser::UiKind::ValueEdit => KspControlKind::ValueEdit,
                    openkontakt::ksp::parser::UiKind::Table => KspControlKind::Label,
                },
                label: control.label.clone(),
                x: control.x,
                y: control.y,
                width: control.width,
                height: control.height,
                min: control.min,
                max: control.max,
                default: control.min,
                value: control.value,
                unit: control.unit.clone(),
                persistent: control.persistent,
                px: control.px,
                picture: control.picture.clone(),
                menu: control.menu.clone(),
                mouse_behaviour: control.mouse_behaviour,
                value_text: control.value_text.clone(),
                pixel_width: control.pixel_width,
                pixel_height: control.pixel_height,
            })
            .collect();
        let controls: &[KspControl] = if live.is_empty() {
            &panel.controls
        } else {
            &live
        };
        let painter = p.ui.painter();
        if !panel.has_ui && live.is_empty() {
            p.text(
                painter,
                art.center().x / p.scale - p.origin.x / p.scale,
                art.center().y / p.scale - p.origin.y / p.scale,
                "NO KSP UI DECLARATIONS IN THIS INSTRUMENT",
                12.0,
                MUTED,
                Align2::CENTER_CENTER,
            );
            return;
        }

        let art_x = art.min.x / p.scale - p.origin.x / p.scale;
        let art_y = art.min.y / p.scale - p.origin.y / p.scale;
        // Kontakt's standard KSP grid is fixed in native pixels. These constants come straight
        // from the KSP reference:
        //   x = ((grid_x - 1) * 92) + 66
        //   y = ((grid_y - 1) * 21) + 2
        //   width = grid_w * 92 - 5, height = grid_h * 21 - 3
        let ksp_scale = (art.width() / p.scale) / panel.ui_width_px.max(1.0);
        let origin_x = art_x;
        let origin_y = art_y;
        let native = |x: f32, y: f32, w: f32, h: f32| {
            p.rect(
                origin_x + x * ksp_scale,
                origin_y + y * ksp_scale,
                w * ksp_scale,
                h * ksp_scale,
            )
        };
        let mut menu_action: Option<String> = None;
        {
            let pictures = &self.pictures;
            let control_rect = |control: &KspControl| {
                // `move_control_px` positions a control in native pixels; `move_control` uses the
                // fixed grid. Custom pictures have their own pixel footprint, not a grid cell.
                let (w, h) = match (control.pixel_width, control.pixel_height) {
                    // A picture button states its size explicitly; that is the hit area Kontakt
                    // uses. Scripts often set only one of the two, so a zero is "unset".
                    (Some(width), Some(height)) if width > 0 && height > 0 => {
                        (width as f32, height as f32)
                    }
                    _ => match picture_frame_of(pictures, control) {
                        Some(frame) => (frame.x, frame.y),
                        None => match picture_size(control) {
                            Some((w, h)) => (w, h),
                            None => (
                                control.width.max(1) as f32 * 92.0 - 5.0,
                                control.height.max(1) as f32 * 21.0 - 3.0,
                            ),
                        },
                    },
                };
                if control.px {
                    native(control.x as f32, control.y as f32, w, h)
                } else {
                    let x = 66.0 + (control.x.max(1) - 1) as f32 * 92.0;
                    let y = 2.0 + (control.y.max(1) - 1) as f32 * 21.0;
                    native(x, y, w, h)
                }
            };

            for control in controls {
                if !control.px && (control.x <= 0 && control.y <= 0) {
                    continue;
                }
                let rect = control_rect(control);
                let value = self
                    .ksp_values
                    .iter()
                    .find(|(name, _)| name == &control.name)
                    .map(|(_, value)| *value)
                    .unwrap_or_else(|| control.normalized_value());
                self.draw_ksp_control(p, control, rect, ksp_scale);
                let draggable = matches!(
                    control.kind,
                    KspControlKind::Knob | KspControlKind::Slider | KspControlKind::ValueEdit
                );
                // Picture buttons and switches are *controls*: a script that draws six of them is
                // meant to be clicked. `Sense::hover` here made every button in a library panel
                // inert.
                let clickable = matches!(
                    control.kind,
                    KspControlKind::Button | KspControlKind::Switch | KspControlKind::Menu
                );
                let response = p.ui.interact(
                    rect,
                    p.ui.scope_id().with(("ksp", &control.name)),
                    if draggable {
                        Sense::click_and_drag()
                    } else if clickable {
                        Sense::click()
                    } else {
                        Sense::hover()
                    },
                );
                if matches!(control.kind, KspControlKind::Menu) {
                    if response.clicked() {
                        menu_action = Some(control.name.clone());
                        p.ui.ctx().request_repaint();
                    }
                } else if matches!(
                    control.kind,
                    KspControlKind::Button | KspControlKind::Switch
                ) && response.clicked()
                {
                    if let Some((_, slot)) = self
                        .ksp_slots
                        .iter()
                        .find(|(name, _)| name == &control.name)
                    {
                        // A button is momentary (the script decides what stays on, which is how
                        // radio groups are built); a switch toggles.
                        let value = if matches!(control.kind, KspControlKind::Button) {
                            if control.value == 1 { 0 } else { 1 }
                        } else {
                            if control.value == 0 {
                                control.max.max(1)
                            } else {
                                control.min
                            }
                        };
                        loaded.ksp.set_control_value(
                            *slot,
                            control.name.trim_start_matches('$'),
                            value,
                        );
                        p.ui.ctx().request_repaint();
                    }
                } else if draggable && (response.dragged() || response.clicked()) {
                    if let Some((_, slot)) = self
                        .ksp_slots
                        .iter()
                        .find(|(name, _)| name == &control.name)
                    {
                        let delta = if matches!(
                            control.kind,
                            KspControlKind::Knob | KspControlKind::ValueEdit
                        ) {
                            // Knobs follow a vertical drag, exactly like the plugin's own knobs
                            // (and like Kontakt) instead of jumping to the pointer position.
                            -response.drag_delta().y / p.s(150.0)
                        } else if let Some(pos) = response.interact_pointer_pos() {
                            // Sliders follow the pointer horizontally.
                            ((pos.x - rect.min.x) / rect.width().max(1.0)).clamp(0.0, 1.0) - value
                        } else {
                            0.0
                        };
                        if delta != 0.0 {
                            let normalized = (value + delta).clamp(0.0, 1.0);
                            loaded.ksp.set_control_normalized(
                                *slot,
                                control.name.trim_start_matches('$'),
                                normalized,
                            );
                            p.ui.ctx().request_repaint();
                        }
                    }
                }
                let _ = response.on_hover_text(format!(
                "{}{}\nKSP grid: ({}, {})  ·  native cell {}×{}\nController value: {} / {} (raw)\nSet by the script's on ui_control handler.",
                control.label,
                control
                    .unit
                    .as_ref()
                    .map(|unit| format!(" · declared unit {unit}"))
                    .unwrap_or_default(),
                control.x,
                control.y,
                control.width,
                control.height,
                control.value,
                control.max
            ));
            }
        }

        if let Some(name) = menu_action {
            let opening = self.open_menu.as_deref() != Some(name.as_str());
            self.open_menu = if opening { Some(name) } else { None };
            self.menu_just_opened = opening;
        }
        self.draw_menu_popup(p, loaded, art, ksp_scale);

        let badge = p.rect(
            art_x + 12.0,
            art_y + art.height() / p.scale - 26.0,
            188.0,
            18.0,
        );
        painter.rect_filled(
            badge,
            p.s(3.0),
            Color32::from_rgba_unmultiplied(12, 11, 9, 210),
        );
        p.text(
            painter,
            badge.min.x / p.scale - p.origin.x / p.scale + 8.0,
            badge.center().y / p.scale - p.origin.y / p.scale,
            "KSP UI · LIVE · ON UI_CONTROL",
            7.5,
            ACCENT_SOFT,
            Align2::LEFT_CENTER,
        );
    }

    /// Draw the sprite frame that corresponds to the control's current value.
    fn draw_custom_picture(
        &self,
        p: &Painter<'_>,
        control: &KspControl,
        cell: Rect,
        value: f32,
    ) -> bool {
        let Some(name) = control.picture.as_deref().map(str::to_ascii_lowercase) else {
            return false;
        };
        let Some(resource) = self.pictures.get(&name) else {
            return false;
        };
        let last = resource.frames.saturating_sub(1).max(1);
        let index = ((value.clamp(0.0, 1.0) * last as f32).round() as usize).min(last);
        let step = 1.0 / resource.frames.max(1) as f32;
        let uv = if resource.horizontal {
            Rect::from_min_max(
                Pos2::new(index as f32 * step, 0.0),
                Pos2::new((index + 1) as f32 * step, 1.0),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(0.0, index as f32 * step),
                Pos2::new(1.0, (index + 1) as f32 * step),
            )
        };
        p.ui.painter()
            .image(resource.texture.id(), cell, uv, Color32::WHITE);
        true
    }

    /// Drop-down list for an expanded menu control.
    fn draw_menu_popup(
        &mut self,
        p: &Painter<'_>,
        loaded: &LoadedInstrument,
        art: Rect,
        _ksp_scale: f32,
    ) {
        let Some(open) = self.open_menu.clone() else {
            return;
        };
        let Some(control) = (0..loaded.ksp.slot_count())
            .flat_map(|slot| loaded.ksp.controls(slot))
            .find(|control| format!("${}", control.name) == open)
        else {
            self.open_menu = None;
            return;
        };
        let Some((_, slot)) = self
            .ksp_slots
            .iter()
            .find(|(name, _)| name == &open)
            .cloned()
        else {
            return;
        };
        let anchor = panel_rect(
            &self.pictures,
            art,
            loaded.instrument.ksp_panel.ui_width_px,
            &KspControl {
                name: open.clone(),
                kind: KspControlKind::Menu,
                label: control.label.clone(),
                x: control.x,
                y: control.y,
                width: control.width,
                height: control.height,
                min: control.min,
                max: control.max,
                default: control.min,
                value: control.value,
                unit: control.unit.clone(),
                persistent: control.persistent,
                px: control.px,
                picture: control.picture.clone(),
                menu: control.menu.clone(),
                mouse_behaviour: control.mouse_behaviour,
                value_text: control.value_text.clone(),
                pixel_width: control.pixel_width,
                pixel_height: control.pixel_height,
            },
        );
        let painter = p.ui.painter();
        let row_h = p.s(14.0);
        let width = anchor.width().max(p.s(90.0));
        let height = row_h * control.menu.len().max(1) as f32;
        let panel = Rect::from_min_size(
            Pos2::new(anchor.min.x, anchor.max.y + p.s(2.0)),
            Vec2::new(width, height),
        );
        painter.rect_filled(panel, p.s(3.0), Color32::from_rgb(43, 43, 44));
        painter.rect_stroke(
            panel,
            p.s(3.0),
            Stroke::new(1.0, Color32::from_rgb(86, 84, 78)),
            StrokeKind::Inside,
        );
        let mut picked = None;
        for (index, item) in control.menu.iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(panel.min.x, panel.min.y + row_h * index as f32),
                Vec2::new(width, row_h),
            );
            let response = p.ui.interact(
                row,
                p.ui.scope_id().with(("ksp-menu", item)),
                Sense::click(),
            );
            let selected = control.value == index as i64;
            if selected {
                painter.rect_filled(row, 0.0, Color32::from_rgb(70, 66, 56));
            } else if response.hovered() {
                painter.rect_filled(row, 0.0, Color32::from_rgb(56, 58, 62));
                p.ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            p.text(
                painter,
                row.min.x / p.scale - p.origin.x / p.scale + 6.0,
                row.center().y / p.scale - p.origin.y / p.scale,
                truncate(item, 22),
                8.0,
                TEXT,
                Align2::LEFT_CENTER,
            );
            if response.clicked() {
                let _ =
                    loaded
                        .ksp
                        .set_control_value(slot, open.trim_start_matches('$'), index as i64);
                picked = Some(());
            }
        }
        let escape = p.ui.input(|input| input.key_pressed(egui::Key::Escape));
        let clicked_outside = !self.menu_just_opened
            && p.ui.input(|input| {
                input.pointer.any_click()
                    && !panel.contains(input.pointer.interact_pos().unwrap_or(Pos2::ZERO))
            });
        if picked.is_some() || clicked_outside || escape {
            self.open_menu = None;
            p.ui.ctx().request_repaint();
        }
    }

    fn draw_ksp_control(&self, p: &Painter<'_>, control: &KspControl, cell: Rect, _ksp_scale: f32) {
        let painter = p.ui.painter();
        let value = self
            .ksp_values
            .iter()
            .find(|(name, _)| name == &control.name)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| control.normalized_value());
        // The script may relabel a control through `set_text` / `set_knob_label`; prefer that.
        let runtime_label = self
            .ksp_labels
            .iter()
            .find(|(name, _)| name == &control.name)
            .map(|(_, label)| label.clone())
            .filter(|label| !label.is_empty());
        let control_label = runtime_label
            .clone()
            .unwrap_or_else(|| control.label.clone());

        if self.draw_custom_picture(p, control, cell, value) {
            return;
        }
        if let Some(picture) = control.picture.as_deref() {
            if draw_picture_control(p, picture, cell, value, control_label.as_str()) {
                return;
            }
        }

        match control.kind {
            KspControlKind::Label => {
                // Only the label has a backing bar; KSP does not draw one big frame around a
                // whole section.
                painter.rect_filled(
                    cell,
                    p.s(2.0),
                    Color32::from_rgba_unmultiplied(20, 16, 12, 190),
                );
                p.text(
                    painter,
                    cell.min.x / p.scale - p.origin.x / p.scale + 8.0,
                    cell.center().y / p.scale - p.origin.y / p.scale,
                    control_label.clone(),
                    9.0,
                    // Every label is drawn in the same ink. Kontakt has no per-label colour, and
                    // the old "highlight anything called 'mic'" rule made one heading in a panel
                    // look selected for no reason.
                    TEXT,
                    Align2::LEFT_CENTER,
                );
            }
            KspControlKind::Knob | KspControlKind::ValueEdit => {
                let cell_left = cell.min.x / p.scale - p.origin.x / p.scale;
                let cell_top = cell.min.y / p.scale - p.origin.y / p.scale;
                let knob_rect = Rect::from_center_size(
                    p.pos(cell_left + 24.0, cell_top + 24.0),
                    Vec2::splat(p.s(34.0)),
                );
                draw_ksp_knob(painter, p, knob_rect, value);
                // Kontakt prints only what `set_knob_label` provides; a bare percentage is ours
                // and would be wrong next to a value like "500.0 ms".
                ksp_knob_caption(
                    painter,
                    p,
                    knob_rect,
                    cell_left + 50.0,
                    truncate(&control_label, 14),
                    control.value_text.clone(),
                );
            }
            KspControlKind::Slider => {
                // KONTAKT's panel slider: one long right triangle across the whole bar (white to
                // the left of the thumb, grey-black to its right), a framed 8:5 right-triangle
                // thumb, and floating minus / plus symbols that fade out as the thumb reaches
                // them.
                let bar = cell.shrink2(Vec2::new(0.0, p.s(1.5)));
                let border = Color32::BLACK;
                painter.rect_filled(bar, 0.0, Color32::from_rgb(85, 85, 85));
                painter.rect_stroke(bar, 0.0, Stroke::new(p.s(1.0), border), StrokeKind::Inside);

                // The wedge keeps a little padding inside the bar.
                let padding = p.s(2.5);
                let wedge = Rect::from_min_max(
                    Pos2::new(bar.min.x + padding, bar.min.y + padding),
                    Pos2::new(bar.max.x - padding, bar.max.y - padding),
                );
                let bottom = wedge.max.y;
                let height = wedge.height().max(p.s(2.0));
                // The thumb is a square as tall as the bar, so its top and bottom edges coincide
                // with the bar's.
                let thumb_width = bar.height();
                let corner = p.s(2.0);
                let travel = (bar.width() - corner * 2.0 - thumb_width).max(0.0);
                let position = bar.min.x + corner + travel * value.clamp(0.0, 1.0);
                let thumb = Rect::from_min_max(
                    Pos2::new(position, bar.min.y),
                    Pos2::new(position + thumb_width, bar.max.y),
                );

                // Split under the middle of the thumb, so the wedge shows through its glass.
                let split = (position + thumb_width * 0.5).clamp(wedge.min.x, wedge.max.x);
                let edge_height = height * (split - wedge.min.x).max(0.0) / wedge.width().max(1.0);
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(wedge.min.x, bottom),
                        Pos2::new(split, bottom),
                        Pos2::new(split, bottom - edge_height),
                    ],
                    Color32::from_rgb(240, 240, 237),
                    Stroke::NONE,
                ));
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(split, bottom),
                        Pos2::new(wedge.max.x, bottom),
                        Pos2::new(wedge.max.x, bottom - height),
                        Pos2::new(split, bottom - edge_height),
                    ],
                    Color32::from_rgb(61, 61, 61),
                    Stroke::NONE,
                ));

                // Minus and plus float over the bar and fade as the thumb closes in.
                let arm = (height * 0.14).max(p.s(1.0));
                let centre_y = bar.center().y;
                let minus = Pos2::new(bar.min.x + height * 0.55, centre_y);
                let plus = Pos2::new(bar.max.x - height * 0.55, centre_y);
                let fade = |distance: f32| (distance / (height * 0.7)).clamp(0.0, 1.0);
                let left_alpha = (fade(thumb.min.x - (minus.x + arm)) * 255.0) as u8;
                let right_alpha = (fade((plus.x - arm) - thumb.max.x) * 255.0) as u8;
                let left_symbol = Color32::from_rgba_unmultiplied(0, 0, 0, left_alpha);
                let right_symbol = Color32::from_rgba_unmultiplied(0, 0, 0, right_alpha);
                painter.line_segment(
                    [minus - Vec2::new(arm, 0.0), minus + Vec2::new(arm, 0.0)],
                    Stroke::new(p.s(1.6), left_symbol),
                );
                painter.line_segment(
                    [plus - Vec2::new(arm, 0.0), plus + Vec2::new(arm, 0.0)],
                    Stroke::new(p.s(1.6), right_symbol),
                );
                painter.line_segment(
                    [plus - Vec2::new(0.0, arm), plus + Vec2::new(0.0, arm)],
                    Stroke::new(p.s(1.6), right_symbol),
                );

                // The thumb frame, with padding around the 8:5 triangle inside it.
                painter.rect_filled(thumb, 0.0, Color32::from_rgba_unmultiplied(64, 64, 62, 235));
                painter.rect_stroke(
                    thumb,
                    0.0,
                    Stroke::new(p.s(1.0), border),
                    StrokeKind::Inside,
                );
                // The icon's base is two thirds of the frame's inner edge, and it sits dead
                // centre.
                let triangle_base = (thumb.width() - p.s(2.0)).max(p.s(2.0)) * 2.0 / 3.0;
                let triangle_height = triangle_base * 5.0 / 8.0;
                let centre = thumb.center();
                let right = centre.x + triangle_base * 0.5;
                let bottom = centre.y + triangle_height * 0.5;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(right, bottom),
                        Pos2::new(right, bottom - triangle_height),
                        Pos2::new(right - triangle_base, bottom),
                    ],
                    Color32::from_rgb(240, 240, 237),
                    Stroke::NONE,
                ));
            }
            KspControlKind::Menu => {
                let box_rect = cell.shrink2(Vec2::new(p.s(4.0), p.s(7.0)));
                painter.rect_filled(box_rect, p.s(3.0), Color32::from_rgb(39, 37, 32));
                painter.rect_stroke(
                    box_rect,
                    p.s(3.0),
                    Stroke::new(1.0, LINE),
                    StrokeKind::Inside,
                );
                let selected = control
                    .menu
                    .get(control.value.max(0) as usize)
                    .cloned()
                    .unwrap_or_else(|| control_label.clone());
                p.text(
                    painter,
                    box_rect.min.x / p.scale - p.origin.x / p.scale + 7.0,
                    box_rect.center().y / p.scale - p.origin.y / p.scale,
                    truncate(&selected, 22),
                    8.0,
                    TEXT,
                    Align2::LEFT_CENTER,
                );
                p.text(
                    painter,
                    box_rect.max.x / p.scale - p.origin.x / p.scale - 7.0,
                    box_rect.center().y / p.scale - p.origin.y / p.scale,
                    "▾",
                    9.0,
                    MUTED,
                    Align2::RIGHT_CENTER,
                );
            }
            KspControlKind::Switch | KspControlKind::Button => {
                p.text(
                    painter,
                    cell.min.x / p.scale - p.origin.x / p.scale + 8.0,
                    cell.center().y / p.scale - p.origin.y / p.scale,
                    truncate(&control.label, 16),
                    8.0,
                    MUTED,
                    Align2::LEFT_CENTER,
                );
            }
            KspControlKind::Other => {}
        }
    }

    /// Compact knob for the instrument bar: drag to change, hover for the value.
    /// Shared drag/double-click handling for the small knobs; returns the live normalised value.
    fn knob_drag(
        &self,
        p: &Painter<'_>,
        rect: Rect,
        param: &FloatParam,
        setter: &ParamSetter<'_>,
        id: (&'static str, &str),
    ) -> (Response, f32) {
        let response = p.ui.interact(
            rect,
            p.ui.scope_id().with((id.0, id.1, rect.min.x as i32)),
            Sense::click_and_drag(),
        );
        let mut normalized = param.modulated_normalized_value();
        if response.dragged() {
            let delta =
                -response.drag_delta().y / p.s(150.0) + response.drag_delta().x / p.s(300.0);
            normalized = (normalized + delta).clamp(0.0, 1.0);
            if response.drag_started() {
                setter.begin_set_parameter(param);
            }
            setter.set_parameter_normalized(param, normalized);
        }
        if response.drag_stopped() {
            setter.end_set_parameter(param);
        }
        if response.double_clicked() {
            setter.begin_set_parameter(param);
            setter.set_parameter(param, param.default_plain_value());
            setter.end_set_parameter(param);
        }
        if response.hovered() {
            p.ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        (response, normalized)
    }

    /// Knob for the instrument bar: a plain ring with a single tick, like KONTAKT's small
    /// pan/volume knobs. The pan knob also carries the fixed 12 o'clock tick.
    fn mini_knob(
        &self,
        p: &Painter<'_>,
        rect: Rect,
        param: &FloatParam,
        setter: &ParamSetter<'_>,
        label: &str,
        top_tick: bool,
    ) -> Response {
        let (response, normalized) = self.knob_drag(p, rect, param, setter, ("mini-knob", label));
        draw_mini_knob(p.ui.painter(), p, rect, normalized, top_tick);
        if response.hovered() {
            // egui's own tooltips are tied to the repaint-after path, which the standalone event
            // loop does not always honour. Draw the value box ourselves so hovering always shows
            // the current pan/volume regardless of repaint timing.
            self.hover_value(
                p,
                rect,
                label,
                &param.normalized_value_to_string(normalized, true),
            );
        }
        response
    }

    /// Small value bubble above a knob, used instead of egui's delayed tooltip.
    fn hover_value(&self, p: &Painter<'_>, rect: Rect, label: &str, value: &str) {
        let painter = p.ui.painter();
        let text = format!("{label}  {value}");
        let font = egui::FontId::proportional(p.s(9.0));
        let galley = painter.layout_no_wrap(text, font, TEXT);
        let size = galley.size();
        let origin = Pos2::new(
            rect.center().x - size.x * 0.5,
            rect.min.y - size.y - p.s(7.0),
        );
        let background = Rect::from_min_size(
            origin - Vec2::new(p.s(5.0), p.s(3.0)),
            size + Vec2::new(p.s(10.0), p.s(6.0)),
        );
        painter.rect_filled(
            background,
            p.s(3.0),
            Color32::from_rgba_unmultiplied(14, 14, 12, 240),
        );
        painter.rect_stroke(
            background,
            p.s(3.0),
            Stroke::new(1.0, LINE),
            StrokeKind::Inside,
        );
        painter.galley(origin, galley, TEXT);
    }

    fn draw_keyboard(&mut self, p: &Painter<'_>, rect: Rect) {
        let painter = p.ui.painter();
        painter.rect_filled(rect, 0.0, Color32::from_rgb(15, 15, 13));
        let wheel_rect = p.rect(
            SIDEBAR_WIDTH + 10.0,
            DESIGN_HEIGHT - KEYBOARD_HEIGHT + 18.0,
            18.0,
            62.0,
        );
        for (index, (label, value)) in [("P", 0.5f32), ("M", 0.0f32)].into_iter().enumerate() {
            let r = wheel_rect.translate(Vec2::new(index as f32 * p.s(24.0), 0.0));
            painter.rect_filled(r, p.s(8.0), Color32::from_rgb(31, 30, 26));
            painter.rect_stroke(r, p.s(8.0), Stroke::new(1.0, LINE), StrokeKind::Inside);
            let y = r.max.y - r.height() * (0.15 + value * 0.7);
            painter.line_segment(
                [
                    Pos2::new(r.min.x + p.s(3.0), y),
                    Pos2::new(r.max.x - p.s(3.0), y),
                ],
                Stroke::new(2.0, ACCENT_SOFT),
            );
            p.text(
                painter,
                r.center().x / p.scale - p.origin.x / p.scale,
                r.max.y / p.scale - p.origin.y / p.scale + 8.0,
                label,
                7.0,
                DIM,
                Align2::CENTER_TOP,
            );
        }

        let keys = Rect::from_min_max(
            p.pos(SIDEBAR_WIDTH + 68.0, DESIGN_HEIGHT - KEYBOARD_HEIGHT + 8.0),
            p.pos(DESIGN_WIDTH - 14.0, DESIGN_HEIGHT - 10.0),
        );
        // Kontakt's original keyboard covers C-2 through C8 (MIDI 0..120, eleven octave
        // groups). The keys are narrower than a piano's 88 keys so the whole range fits,
        // separated by a 1 px gap.
        let first_note = 0u8;
        let last_note = 120u8;
        let white_notes: Vec<u8> = (first_note..=last_note)
            .filter(|note| !is_black_key(*note))
            .collect();
        let white_width = keys.width() / white_notes.len() as f32;
        let gap = p.s(1.0);
        // Cut the same strip off the top of the whole keybed so the white keys line up with
        // the shortened black keys; the strip above stays as the dark chassis.
        let top_trim = p.s(7.0);
        let key_top = keys.min.y + top_trim;
        let key_height = (keys.max.y - key_top).max(1.0);

        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(keys.min.x, key_top),
                Vec2::new(keys.width(), key_height),
            ),
            0.0,
            KEY_GAP,
        );

        let white_rects: Vec<(u8, Rect)> = white_notes
            .iter()
            .enumerate()
            .map(|(index, note)| {
                let is_last = index + 1 == white_notes.len();
                let width = if is_last {
                    white_width
                } else {
                    (white_width - gap).max(1.0)
                };
                (
                    *note,
                    Rect::from_min_size(
                        Pos2::new(keys.min.x + white_width * index as f32, key_top),
                        Vec2::new(width, key_height),
                    ),
                )
            })
            .collect();

        let mut black_rects: Vec<(u8, Rect)> = Vec::new();
        for note in first_note..=last_note {
            if !is_black_key(note) {
                continue;
            }
            let Some(previous_white) = (first_note..note).rev().find(|n| !is_black_key(*n)) else {
                continue;
            };
            let white_index = white_notes
                .iter()
                .position(|n| *n == previous_white)
                .unwrap_or(0);
            let x = keys.min.x + white_width * (white_index as f32 + 1.0);
            black_rects.push((
                note,
                Rect::from_min_size(
                    Pos2::new(x - white_width * 0.30, key_top),
                    Vec2::new(
                        white_width * 0.60,
                        (keys.height() * 0.62 - top_trim).max(p.s(4.0)),
                    ),
                ),
            ));
        }

        let mut hit_rects: Vec<(u8, Rect)> = white_rects.clone();
        hit_rects.extend(black_rects.iter().copied());

        // Process the mouse before drawing so a note-off frame immediately paints the key as
        // released. The previous implementation handled the mouse after drawing and could leave
        // the last drawn frame showing a stuck key until another mouse event forced a repaint.
        if !self.show_file_menu && !self.show_recent_menu {
            let (pressed, released, pointer_pos) = p.ui.input(|input| {
                (
                    input.pointer.button_pressed(egui::PointerButton::Primary),
                    input.pointer.button_released(egui::PointerButton::Primary),
                    input.pointer.interact_pos(),
                )
            });

            // Kontakt's on-screen keyboard starts a note only on mouse-down and holds that same
            // note until mouse-up. Moving across keys while held does not glissando, and moving
            // out of the keyboard does not cut the note.
            if pressed {
                if let Some(previous) = self.mouse_note.take() {
                    self.shared.send_note(previous, 0.0, false);
                }
                if let Some(note) = pointer_pos.and_then(|position| {
                    hit_rects
                        .iter()
                        .rev()
                        .find(|(_, rect)| rect.contains(position))
                        .map(|(note, _)| *note)
                }) {
                    self.shared.send_note(note, 0.85, true);
                    self.mouse_note = Some(note);
                }
            }
            if released {
                if let Some(previous) = self.mouse_note.take() {
                    self.shared.send_note(previous, 0.0, false);
                }
            }
        } else if self.mouse_note.is_some() {
            self.release_held_notes();
        }

        // The keybed is painted before the status bar, and the bar overlaps the bottom of the
        // keys rect, so place the octave labels just above the bar.
        let label_y = DESIGN_HEIGHT - 28.0;
        let mapped = self
            .shared
            .current()
            .map(|loaded| loaded.instrument.mapped_keys());
        for (note, key) in &white_rects {
            let active = self.shared.is_note_active(*note) || self.mouse_note == Some(*note);
            let (up, down) = if mapped.as_ref().is_some_and(|mapped| mapped[*note as usize]) {
                (KEY_WHITE_MAPPED, KEY_WHITE_MAPPED_DOWN)
            } else {
                (KEY_WHITE, KEY_WHITE_DOWN)
            };
            painter.rect_filled(*key, p.s(1.0), if active { down } else { up });
            if note % 12 == 0 {
                // Kontakt prints only the octave group on each C key (C-2, C-1, C0 … C8).
                p.mono(
                    painter,
                    key.center().x / p.scale - p.origin.x / p.scale,
                    label_y,
                    format!("{}", (*note as i32 / 12) - 2),
                    7.0,
                    Color32::from_rgb(76, 71, 61),
                    Align2::CENTER_BOTTOM,
                );
            }
        }

        for (note, key) in &black_rects {
            let active = self.shared.is_note_active(*note) || self.mouse_note == Some(*note);
            let (up, down) = if mapped.as_ref().is_some_and(|mapped| mapped[*note as usize]) {
                (KEY_BLACK_MAPPED, KEY_BLACK_MAPPED_DOWN)
            } else {
                (KEY_BLACK, KEY_BLACK_DOWN)
            };
            painter.rect_filled(*key, p.s(1.0), if active { down } else { up });
            painter.rect_stroke(
                *key,
                p.s(1.0),
                Stroke::new(1.0, Color32::from_rgb(5, 5, 4)),
                StrokeKind::Inside,
            );
        }

        // The keybed root rail — the 1 px cyan line the keys hang from — only runs under the
        // mapped range, so it marks the playable span exactly like KONTAKT's.
        if let Some(mapped) = mapped.as_ref() {
            let rail = hit_rects
                .iter()
                .filter(|(note, _)| mapped[*note as usize])
                .map(|(_, rect)| rect);
            let min_x = rail
                .clone()
                .fold(f32::INFINITY, |min, rect| min.min(rect.min.x));
            let max_x = rail.fold(f32::NEG_INFINITY, |max, rect| max.max(rect.max.x));
            if min_x <= max_x {
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(min_x, key_top - p.s(1.0)),
                        Vec2::new(max_x - min_x, p.s(1.0)),
                    ),
                    0.0,
                    KEY_ROOT,
                );
            }
        }
    }

    fn draw_status_bar(
        &self,
        p: &Painter<'_>,
        status: &LoadStatus,
        loaded: Option<&crate::state::LoadedInstrument>,
    ) {
        let painter = p.ui.painter();
        let y = DESIGN_HEIGHT - 24.0;
        let left = if self.view == ViewMode::Library {
            0.0
        } else {
            SIDEBAR_WIDTH
        };
        painter.rect_filled(
            p.rect(left, y, DESIGN_WIDTH - left, 24.0),
            0.0,
            Color32::from_rgb(16, 16, 14),
        );
        painter.line_segment(
            [p.pos(left, y), p.pos(DESIGN_WIDTH, y)],
            Stroke::new(1.0, LINE),
        );
        let text = match status {
            LoadStatus::Loading { path, started } => format!(
                "Loading {} · {:.1}s",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("instrument"),
                started.elapsed().as_secs_f32()
            ),
            LoadStatus::Ready {
                sample_files,
                warnings,
                elapsed_ms,
                ..
            } => format!(
                "READY · {sample_files} sample files · {warnings} diagnostics · loaded in {:.2}s",
                *elapsed_ms as f64 / 1000.0
            ),
            LoadStatus::Failed { message, .. } => format!("ERROR · {message}"),
            LoadStatus::Empty => "READY · no instrument".to_string(),
        };
        p.text(
            painter,
            left + 14.0,
            y + 12.0,
            truncate(&text, 76),
            8.0,
            match status {
                LoadStatus::Failed { .. } => ACCENT,
                LoadStatus::Ready { .. } => GREEN,
                _ => MUTED,
            },
            Align2::LEFT_CENTER,
        );
        if let Some(loaded) = loaded {
            p.text(
                painter,
                left + 570.0,
                y + 12.0,
                format!(
                    "ROOT  {}",
                    truncate(&loaded.instrument.library_root.display().to_string(), 50)
                ),
                7.5,
                DIM,
                Align2::LEFT_CENTER,
            );
            // KSP errors are printed to the console; a truncated tail in the corner said nothing.
            if let Some(diagnostic) = loaded
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.area != "ksp")
            {
                p.text(
                    painter,
                    DESIGN_WIDTH - 14.0,
                    y + 12.0,
                    truncate(&diagnostic.message, 55),
                    7.5,
                    ACCENT_SOFT,
                    Align2::RIGHT_CENTER,
                );
            }
        }
    }

    fn menu_row(
        &self,
        p: &Painter<'_>,
        rect: Rect,
        id: impl std::hash::Hash + std::fmt::Debug,
        label: &str,
        shortcut: Option<&str>,
        enabled: bool,
    ) -> Response {
        let response = p.ui.interact(
            rect,
            p.ui.scope_id().with(("file-menu", id)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if enabled && response.hovered() {
            p.ui.painter()
                .rect_filled(rect, p.s(2.0), Color32::from_rgb(64, 60, 52));
            p.ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let color = if enabled { TEXT } else { DIM };
        p.text(
            p.ui.painter(),
            rect.min.x / p.scale - p.origin.x / p.scale + p.s(10.0),
            rect.center().y / p.scale - p.origin.y / p.scale,
            label,
            10.0,
            color,
            Align2::LEFT_CENTER,
        );
        if let Some(shortcut) = shortcut {
            p.text(
                p.ui.painter(),
                rect.max.x / p.scale - p.origin.x / p.scale - p.s(10.0),
                rect.center().y / p.scale - p.origin.y / p.scale,
                shortcut,
                9.0,
                if enabled { MUTED } else { DIM },
                Align2::RIGHT_CENTER,
            );
        }
        response
    }

    fn draw_file_menu(&mut self, p: &mut Painter<'_>) {
        if !self.show_file_menu {
            self.file_menu_rect = None;
            return;
        }
        if let Some(previous) = self.file_menu_rect {
            let (clicked, pointer) =
                p.ui.input(|input| (input.pointer.any_click(), input.pointer.interact_pos()));
            if clicked && pointer.is_some_and(|position| !previous.contains(position)) {
                self.show_file_menu = false;
                self.show_recent_menu = false;
                self.file_menu_rect = None;
                return;
            }
        }

        let menu = p.rect(12.0, HEADER_HEIGHT + 2.0, 292.0, 190.0);
        let painter = p.ui.painter().clone();
        painter.rect_filled(menu, p.s(3.0), Color32::from_rgb(43, 43, 44));
        painter.rect_stroke(
            menu,
            p.s(3.0),
            Stroke::new(1.0, Color32::from_rgb(86, 84, 78)),
            StrokeKind::Inside,
        );

        let mut y = menu.min.y / p.scale - p.origin.y / p.scale + 6.0;
        let row = |y: f32, height: f32| p.rect(18.0, y, 280.0, height);

        let load_rect = row(y, 30.0);
        if self
            .menu_row(p, load_rect, "load", "Load…", Some("Cmd+O"), true)
            .clicked()
        {
            self.open_file_dialog();
            return;
        }
        y += 30.0;

        let recent_rect = row(y, 30.0);
        if self.recent_instruments.is_empty() {
            let current = self.params.path();
            if !current.is_empty() {
                self.recent_instruments.push(PathBuf::from(current));
            }
        }
        let has_recent = !self.recent_instruments.is_empty();
        let recent = self.menu_row(
            p,
            recent_rect,
            "load-recent",
            "Load recent",
            Some("›"),
            has_recent,
        );
        if recent.clicked() && has_recent {
            self.show_recent_menu = !self.show_recent_menu;
        }
        y += 30.0;

        painter.line_segment(
            [p.pos(22.0, y + 4.0), p.pos(294.0, y + 4.0)],
            Stroke::new(1.0, Color32::from_rgb(76, 73, 67)),
        );
        y += 10.0;

        let current_path = self.params.path();
        let has_current = !current_path.is_empty();
        let reload_rect = row(y, 30.0);
        if self
            .menu_row(
                p,
                reload_rect,
                "reload",
                "Reload instrument",
                None,
                has_current,
            )
            .clicked()
            && has_current
        {
            self.load_path(PathBuf::from(current_path));
            return;
        }
        y += 30.0;

        let reset_rect = row(y, 30.0);
        if self
            .menu_row(
                p,
                reset_rect,
                "reset",
                "Reset instrument",
                None,
                has_current,
            )
            .clicked()
            && has_current
        {
            self.reset_instrument();
            self.show_file_menu = false;
            self.show_recent_menu = false;
            self.file_menu_rect = None;
            return;
        }
        y += 30.0;

        painter.line_segment(
            [p.pos(22.0, y + 4.0), p.pos(294.0, y + 4.0)],
            Stroke::new(1.0, Color32::from_rgb(76, 73, 67)),
        );
        y += 10.0;

        let options_rect = row(y, 30.0);
        self.menu_row(p, options_rect, "options", "Options…", Some("F12"), false);

        let mut total_rect = menu;
        if self.show_recent_menu && has_recent {
            let submenu = p.rect(
                310.0,
                recent_rect.top() / p.scale - p.origin.y / p.scale,
                380.0,
                (self.recent_instruments.len().min(8) as f32 * 27.0) + 8.0,
            );
            painter.rect_filled(submenu, p.s(3.0), Color32::from_rgb(43, 43, 44));
            painter.rect_stroke(
                submenu,
                p.s(3.0),
                Stroke::new(1.0, Color32::from_rgb(86, 84, 78)),
                StrokeKind::Inside,
            );
            total_rect = total_rect.union(submenu);
            let mut item_y = submenu.min.y / p.scale - p.origin.y / p.scale + 4.0;
            let mut selected = None;
            for (index, path) in self.recent_instruments.iter().take(8).enumerate() {
                let item = p.rect(316.0, item_y, 368.0, 27.0);
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("instrument.nki"));
                let label = truncate(name, 43);
                let response = self.menu_row(p, item, ("recent", index), &label, None, true);
                if response.clicked() {
                    selected = Some(path.clone());
                }
                item_y += 27.0;
            }
            if let Some(path) = selected {
                self.load_path(path);
                return;
            }
        }

        if p.ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.show_file_menu = false;
            self.show_recent_menu = false;
            self.file_menu_rect = None;
        } else {
            self.file_menu_rect = Some(total_rect);
        }
    }
}

/// Screen rectangle of a control inside the KSP panel, in native KSP coordinates scaled to `art`.
fn panel_rect(
    pictures: &HashMap<String, PictureResource>,
    art: Rect,
    ui_width: f32,
    control: &KspControl,
) -> Rect {
    let scale = art.width() / ui_width.max(1.0);
    let (w, h) = match picture_frame_of(pictures, control) {
        Some(frame) => (frame.x, frame.y),
        None => (
            control.width.max(1) as f32 * 92.0 - 5.0,
            control.height.max(1) as f32 * 21.0 - 3.0,
        ),
    };
    let (x, y) = if control.px {
        (control.x as f32, control.y as f32)
    } else {
        (
            66.0 + (control.x.max(1) - 1) as f32 * 92.0,
            2.0 + (control.y.max(1) - 1) as f32 * 21.0,
        )
    };
    Rect::from_min_size(
        Pos2::new(art.min.x + x * scale, art.min.y + y * scale),
        Vec2::new(w * scale, h * scale),
    )
}

/// Native frame size of a control's picture resource, when the library ships one.
fn picture_frame_of(
    pictures: &HashMap<String, PictureResource>,
    control: &KspControl,
) -> Option<Vec2> {
    let name = control.picture.as_deref()?.to_ascii_lowercase();
    pictures.get(&name).map(|resource| resource.frame)
}

/// Native pixel footprint of a KSP picture resource.
///
/// Kontakt ships these as built-in GUI resources referenced by name; the file does not carry
/// their pixel size. These are the sizes our renderer assumes, chosen to match the layouts the
/// scripts were authored against (a dot handle, a vertical line, a standard horizontal slider).
fn picture_size(control: &KspControl) -> Option<(f32, f32)> {
    let picture = control.picture.as_deref()?.to_ascii_lowercase();
    if picture.contains("dot") {
        Some((18.0, 18.0))
    } else if picture.contains("line") {
        Some((10.0, 120.0))
    } else if picture.contains("slider") {
        Some((110.0, 14.0))
    } else {
        None
    }
}

/// Draw a control that uses a custom picture. Returns `false` when the picture is unknown, so the
/// caller falls back to the generic widget.
fn draw_picture_control(
    p: &Painter<'_>,
    picture: &str,
    cell: Rect,
    value: f32,
    _label: &str,
) -> bool {
    let painter = p.ui.painter();
    let lower = picture.to_ascii_lowercase();
    let value = value.clamp(0.0, 1.0);

    if lower.contains("dot") {
        let center = cell.center();
        let radius = (cell.width().min(cell.height()) * 0.5).max(2.0);
        painter.circle_filled(center, radius, Color32::from_rgb(28, 92, 86));
        painter.circle_stroke(center, radius, Stroke::new(p.s(1.0), ACCENT_SOFT));
        painter.circle_filled(
            center + Vec2::new(0.0, (0.5 - value) * radius * 0.6),
            p.s(2.0),
            TEXT,
        );
        return true;
    }

    if lower.contains("line") {
        let x = cell.center().x;
        painter.line_segment(
            [Pos2::new(x, cell.top()), Pos2::new(x, cell.bottom())],
            Stroke::new(p.s(1.5), Color32::from_rgb(160, 155, 140)),
        );
        let y = cell.bottom() - value * cell.height();
        painter.circle_filled(Pos2::new(x, y), p.s(3.5), ACCENT_SOFT);
        return true;
    }

    if lower.contains("slider") {
        let track = Rect::from_min_max(
            Pos2::new(cell.min.x + p.s(2.0), cell.center().y - p.s(2.0)),
            Pos2::new(cell.max.x - p.s(2.0), cell.center().y + p.s(2.0)),
        );
        painter.rect_filled(track, p.s(2.0), Color32::from_rgb(44, 42, 36));
        let filled =
            Rect::from_min_size(track.min, Vec2::new(track.width() * value, track.height()));
        painter.rect_filled(filled, p.s(2.0), ACCENT_SOFT);
        return true;
    }

    false
}

/// Small processor icon (outlined square with a centre) for the CPU readout.
fn draw_cpu_icon(painter: &egui::Painter, p: &Painter<'_>, x: f32, y: f32) {
    let body = p.rect(x, y - 4.5, 9.0, 9.0);
    painter.rect_stroke(body, p.s(1.5), Stroke::new(1.0, MUTED), StrokeKind::Inside);
    painter.rect_filled(p.rect(x + 2.8, y - 1.7, 3.4, 3.4), p.s(0.5), MUTED);
}

/// Small platter icon (circle with a centre dot) for the disk readout.
fn draw_disk_icon(painter: &egui::Painter, p: &Painter<'_>, x: f32, y: f32) {
    let center = p.pos(x + 4.5, y);
    painter.circle_stroke(center, p.s(4.4), Stroke::new(1.0, MUTED));
    painter.circle_filled(center, p.s(1.5), MUTED);
}

/// Play triangle used for the instrument page button.
fn draw_play_icon(painter: &egui::Painter, p: &Painter<'_>, center: Pos2, color: Color32) {
    let width = p.s(6.0);
    let height = p.s(8.0);
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(center.x - width * 0.6, center.y - height),
            Pos2::new(center.x + width, center.y),
            Pos2::new(center.x - width * 0.6, center.y + height),
        ],
        Color32::TRANSPARENT,
        Stroke::new(p.s(1.6), color),
    ));
}

/// The browser button: two upright books and one leaning book.
fn draw_library_icon(painter: &egui::Painter, p: &Painter<'_>, rect: Rect, color: Color32) {
    let stroke = Stroke::new(p.s(1.6), color);
    let left = rect.center().x - p.s(7.0);
    let top = rect.center().y - p.s(7.0);
    let bottom = rect.center().y + p.s(7.0);

    // Two upright books, drawn as strokes rather than outlined boxes. These coordinates are
    // already in painter space (the rect came from `Painter::rect`), so they must not go through
    // `Painter::pos` again — doing that made the icon drift when the window was resized.
    painter.line_segment([Pos2::new(left, top), Pos2::new(left, bottom)], stroke);
    painter.line_segment(
        [Pos2::new(left + 4.6, top), Pos2::new(left + 4.6, bottom)],
        stroke,
    );
    // The leaning book falls the other way: top-left to bottom-right, like a backslash.
    painter.line_segment(
        [Pos2::new(left + 9.2, top), Pos2::new(left + 13.6, bottom)],
        stroke,
    );
}

/// Small musical-note icon for the voice counter.
fn draw_voice_icon(painter: &egui::Painter, p: &Painter<'_>, x: f32, y: f32) {
    painter.circle_filled(p.pos(x + 2.2, y + 3.0), p.s(2.2), MUTED);
    painter.line_segment(
        [p.pos(x + 4.4, y + 3.0), p.pos(x + 4.4, y - 4.5)],
        Stroke::new(p.s(1.2), MUTED),
    );
    painter.line_segment(
        [p.pos(x + 4.4, y - 4.5), p.pos(x + 8.2, y - 3.0)],
        Stroke::new(p.s(1.2), MUTED),
    );
}

/// Small memory-chip icon for the memory readout.
fn draw_memory_icon(painter: &egui::Painter, p: &Painter<'_>, x: f32, y: f32) {
    let body = p.rect(x, y - 3.5, 9.0, 7.0);
    painter.rect_stroke(body, p.s(1.0), Stroke::new(1.0, MUTED), StrokeKind::Inside);
    for index in 0..3 {
        let py = y - 2.0 + index as f32 * 2.0;
        painter.line_segment([p.pos(x - 1.6, py), p.pos(x, py)], Stroke::new(1.0, MUTED));
        painter.line_segment(
            [p.pos(x + 9.0, py), p.pos(x + 10.6, py)],
            Stroke::new(1.0, MUTED),
        );
    }
}

/// The original's minimal master knob: a ring with a gap at the bottom and a white line from the
/// centre to the currently selected value.
fn draw_gap_knob(painter: &egui::Painter, p: &Painter<'_>, rect: Rect, normalized: f32) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.43;
    let start = 135f32.to_radians();
    let sweep = 270f32.to_radians();
    let normalized = normalized.clamp(0.0, 1.0);

    // The ring, with a gap at the bottom.
    painter.add(egui::Shape::line(
        knob_arc(center, radius, start, start + sweep),
        Stroke::new(p.s(2.4), Color32::from_rgb(108, 104, 96)),
    ));

    // The white curve runs from the middle of the scale (12 o'clock) to the selected value.
    let middle = start + sweep * 0.5;
    let value_angle = start + sweep * normalized;
    if (value_angle - middle).abs() > 0.02 {
        let (from, to) = if value_angle > middle {
            (middle, value_angle)
        } else {
            (value_angle, middle)
        };
        painter.add(egui::Shape::line(
            knob_arc(center, radius, from, to),
            Stroke::new(p.s(2.4), Color32::WHITE),
        ));
    }
}

/// Points along a circular arc, used by the master knobs.
fn knob_arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = (((to - from).abs() / std::f32::consts::TAU) * 96.0)
        .ceil()
        .max(2.0) as usize;
    (0..=steps)
        .map(|step| {
            let angle = from + (to - from) * step as f32 / steps as f32;
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

/// KONTAKT's instrument-bar knob: just an outlined circle and one tick at the value angle,
/// painted in the same colour as the S/M labels.
fn draw_mini_knob(
    painter: &egui::Painter,
    p: &Painter<'_>,
    rect: Rect,
    normalized: f32,
    top_tick: bool,
) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5;
    let start = 135f32.to_radians();
    let sweep = 270f32.to_radians();
    let angle = start + sweep * normalized.clamp(0.0, 1.0);
    painter.circle_stroke(
        center,
        (radius - p.s(1.0)).max(1.0),
        Stroke::new(p.s(1.2), TEXT),
    );
    let direction = Vec2::new(angle.cos(), angle.sin());
    painter.line_segment(
        [
            center + direction * radius * 0.42,
            center + direction * (radius - p.s(1.5)),
        ],
        Stroke::new(p.s(1.6), TEXT),
    );
    if top_tick {
        // Fixed 12 o'clock tick, the pan knob's centre detent mark.
        painter.rect_filled(
            Rect::from_center_size(
                Pos2::new(center.x, center.y - radius - p.s(4.5)),
                Vec2::new(p.s(1.5), p.s(3.0)),
            ),
            0.0,
            TEXT,
        );
    }
}

/// The name and readout beside a KSP knob: two translucent bars whose left edge follows the
/// knob's curvature (without touching it) and whose right edges line up, as in KONTAKT. Their
/// heights keep KONTAKT's 16 : 5 : 16 proportion.
fn ksp_knob_caption(
    painter: &egui::Painter,
    p: &Painter<'_>,
    knob_rect: Rect,
    text_left: f32,
    label: String,
    value_text: Option<String>,
) {
    // `text_left` arrives in design units; everything below works in physical pixels, and the
    // window's fit-to-size scale changes when it is resized.
    let text_left = p.origin.x + text_left * p.scale;
    let bar_height = knob_rect.height() * 16.0 / 37.0;
    let gap = knob_rect.height() * 5.0 / 37.0;

    let label_galley = painter.layout_no_wrap(label, FontId::proportional(p.s(10.0)), TEXT);
    let value_galley =
        value_text.map(|text| painter.layout_no_wrap(text, FontId::proportional(p.s(9.0)), TEXT));
    let text_width = value_galley
        .as_ref()
        .map(|galley| galley.size().x.max(label_galley.size().x))
        .unwrap_or(label_galley.size().x);
    let right = text_left + text_width + p.s(6.0);

    let center = knob_rect.center();
    let arc_radius = knob_rect.width() * 0.5 + p.s(4.0);

    let draw_box = |top: f32, galley: &std::sync::Arc<egui::Galley>, color: Color32| {
        let bottom = top + bar_height;
        let mut row = top;
        while row < bottom {
            // One physical pixel per row: p.s(1.0) would be a logical pixel and leave visible
            // steps on the curve at 2x scaling.
            let next = (row + 1.0).min(bottom);
            let dy = (row + next) * 0.5 - center.y;
            let dx = (arc_radius * arc_radius - dy * dy).max(0.0).sqrt();
            let start = (center.x + dx).min(right - p.s(1.0));
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(start, row), Pos2::new(right, next)),
                0.0,
                color,
            );
            row = next;
        }
        painter.galley(
            Pos2::new(text_left, (top + bottom - galley.size().y) * 0.5),
            galley.clone(),
            TEXT,
        );
    };

    let top = knob_rect.min.y;
    draw_box(top, &label_galley, Color32::from_black_alpha(130));
    if let Some(galley) = &value_galley {
        draw_box(
            top + bar_height + gap,
            galley,
            Color32::from_black_alpha(130),
        );
    }
}

/// KONTAKT's default knob as the KSP panel draws it: a grey-black bezel, a warm grey ring and a
/// white face, with a black pointer tick and an arc of light on the bezel that follows it and
/// fades into the bezel at both ends.
fn draw_ksp_knob(painter: &egui::Painter, p: &Painter<'_>, rect: Rect, normalized: f32) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5;

    painter.circle_filled(
        center + Vec2::new(0.0, p.s(1.5)),
        radius,
        Color32::from_rgba_unmultiplied(0, 0, 0, 120),
    );
    painter.circle_filled(center, radius, Color32::from_rgb(83, 82, 83));
    painter.circle_stroke(
        center,
        (radius - p.s(0.5)).max(1.0),
        Stroke::new(p.s(1.0), Color32::from_rgb(52, 51, 52)),
    );
    painter.circle_filled(center, radius * 0.62, Color32::from_rgb(209, 209, 201));
    painter.circle_filled(center, radius * 0.44, Color32::from_rgb(250, 250, 250));

    let angle = 135f32.to_radians() + 270f32.to_radians() * normalized.clamp(0.0, 1.0);
    let direction = Vec2::new(angle.cos(), angle.sin());
    painter.line_segment(
        [
            center + direction * radius * 0.18,
            center + direction * radius * 0.48,
        ],
        Stroke::new(p.s(2.0), Color32::from_rgb(42, 41, 39)),
    );

    // The bezel highlight: a short arc along the rim, brightest in the middle and blending into
    // the grey-black bezel towards both ends.
    let arc_radius = radius * 0.84;
    let half_span = 0.26f32;
    let steps = 20;
    let mut previous =
        center + Vec2::new((angle - half_span).cos(), (angle - half_span).sin()) * arc_radius;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let theta = angle - half_span + 2.0 * half_span * t;
        let point = center + Vec2::new(theta.cos(), theta.sin()) * arc_radius;
        let fade = (1.0 - ((t - 0.5).abs() * 2.0)).powf(0.55);
        let color = Color32::from_rgba_unmultiplied(
            236,
            236,
            232,
            (235.0 * fade).round().clamp(0.0, 255.0) as u8,
        );
        painter.line_segment([previous, point], Stroke::new(p.s(3.0), color));
        previous = point;
    }
}

fn computer_keyboard() -> [(egui::Key, u8); 17] {
    [
        (egui::Key::A, 0),
        (egui::Key::W, 1),
        (egui::Key::S, 2),
        (egui::Key::E, 3),
        (egui::Key::D, 4),
        (egui::Key::F, 5),
        (egui::Key::T, 6),
        (egui::Key::G, 7),
        (egui::Key::Y, 8),
        (egui::Key::H, 9),
        (egui::Key::U, 10),
        (egui::Key::J, 11),
        (egui::Key::K, 12),
        (egui::Key::O, 13),
        (egui::Key::L, 14),
        (egui::Key::P, 15),
        (egui::Key::Semicolon, 16),
    ]
}

fn computer_key_note(key: egui::Key) -> Option<u8> {
    computer_keyboard()
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, offset)| 60u8.saturating_add(*offset))
}

fn is_black_key(note: u8) -> bool {
    matches!(note % 12, 1 | 3 | 6 | 8 | 10)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn reopening_the_editor_forgets_cached_textures() {
        let params = Arc::new(crate::params::OpenKontaktParams::default());
        let (shared, _notes) = crate::state::SharedState::new();
        let mut editor = OpenKontaktEditor::new(params, shared);

        editor.pictures_root = Some(PathBuf::from("/tmp/pictures"));
        editor.background_source = Some(BackgroundSource {
            file: None,
            script: Some("skin".to_string()),
        });
        editor.header_icon_source = Some("logo".to_string());

        editor.forget_textures();

        assert!(editor.pictures.is_empty());
        assert!(editor.pictures_root.is_none());
        assert!(editor.background_source.is_none());
        assert!(editor.header_icon_source.is_none());
    }
}
