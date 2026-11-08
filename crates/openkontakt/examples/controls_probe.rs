//! Every control the panel would draw, in native panel coordinates.
use openkontakt::ksp::GroupInfo;
use openkontakt::{Instrument, KspRuntime};
use std::sync::Arc;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    let root = instrument.library_root.display().to_string();
    let texts: Vec<Option<String>> = instrument.scripts.iter().map(|s| s.text.clone()).collect();
    let groups: Vec<GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let runtime = Arc::new(KspRuntime::compile_with_groups(&texts, &root, groups));
    runtime.initialize();
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }
    for slot in 0..runtime.slot_count() {
        for control in runtime.controls(slot) {
            if !control.px && (control.x <= 0 && control.y <= 0) {
                continue;
            }
            println!(
                "slot {slot} {:<22} kind={:?} pos=({},{}) px={} size={:?}x{:?}",
                control.name,
                control.kind,
                control.x,
                control.y,
                control.px,
                control.pixel_width,
                control.pixel_height
            );
        }
    }
}
