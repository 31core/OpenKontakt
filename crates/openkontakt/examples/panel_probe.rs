//! The panel geometry a library declares.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    println!(
        "ksp_panel: {}x{} px",
        instrument.ksp_panel.ui_width_px, instrument.ksp_panel.ui_height_px
    );
    // Which controls the panel would draw near the top edge, where the reference shows none.
    let root = instrument.library_root.display().to_string();
    let texts: Vec<Option<String>> = instrument.scripts.iter().map(|s| s.text.clone()).collect();
    let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| openkontakt::ksp::GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let runtime = std::sync::Arc::new(openkontakt::KspRuntime::compile_with_groups(
        &texts, &root, groups,
    ));
    runtime.initialize();
    println!("controls near the top edge:");
    for slot in 0..runtime.slot_count() {
        for control in runtime.controls(slot) {
            let visible = control.px || control.x > 0 || control.y > 0;
            if visible && control.y < 60 {
                println!(
                    "  slot {slot} {:<24} y={:<4} x={:<4} px={} kind={:?} picture={:?}",
                    control.name, control.y, control.x, control.px, control.kind, control.picture
                );
            }
        }
    }
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
