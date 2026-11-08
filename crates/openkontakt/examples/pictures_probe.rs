//! Which pictures an instrument can reach, on disk or inside a `.nkr`.
use openkontakt::ksp::GroupInfo;
use openkontakt::{Instrument, KspRuntime};
use std::sync::Arc;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    println!("library root: {}", instrument.library_root.display());
    match instrument.resource_archive() {
        Some(archive) => println!("archive: {} entries", archive.entries.len()),
        None => println!("archive: none"),
    }
    for picture in instrument.picture_assets() {
        println!(
            "  picture {:<24} {:>7} B  sidecar={}",
            picture.stem,
            picture.image.len(),
            picture.text.is_some()
        );
    }
    println!(
        "wallpaper from the program: {:?}",
        instrument.wallpaper_path
    );

    // The panel may also name its own wallpaper through `$INST_WALLPAPER_ID`.
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
    // Same order as the plugin's loader: persisted values are replayed after `on init`.
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }
    println!(
        "wallpaper from the panel: {:?}",
        runtime.engine.wallpaper_picture()
    );
    println!("panel height: {:?}", runtime.engine.panel_height());
}
