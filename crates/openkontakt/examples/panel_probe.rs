//! The panel geometry a library declares.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    println!(
        "ksp_panel: {}x{} px",
        instrument.ksp_panel.ui_width_px, instrument.ksp_panel.ui_height_px
    );
    for script in &instrument.scripts {
        if let Some(text) = &script.text {
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with("set_ui") || line.starts_with("make_perfview") {
                    println!("  slot {}: {line}", script.index);
                }
            }
        }
    }
}
