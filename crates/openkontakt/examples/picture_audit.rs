//! Which pictures a panel asks for, and which of them the library actually ships.
use openkontakt::ksp::GroupInfo;
use openkontakt::{Instrument, KspRuntime};
use std::collections::BTreeSet;
use std::sync::Arc;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    let available: BTreeSet<String> = instrument
        .picture_assets()
        .iter()
        .map(|asset| asset.stem.to_ascii_lowercase())
        .collect();
    println!("{} pictures available", available.len());

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

    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut no_picture = 0usize;
    let mut drawn = 0usize;
    for slot in 0..runtime.slot_count() {
        for control in runtime.controls(slot) {
            if !control.px && (control.x <= 0 && control.y <= 0) {
                continue;
            }
            drawn += 1;
            match &control.picture {
                Some(picture) => {
                    let key = picture.to_ascii_lowercase();
                    wanted.insert(key.clone());
                    if !available.contains(&key) {
                        missing.insert(key);
                    }
                }
                None => no_picture += 1,
            }
        }
    }
    println!(
        "{drawn} controls drawn, {no_picture} without a picture, {} picture names used",
        wanted.len()
    );
    if missing.is_empty() {
        println!("every requested picture exists");
    } else {
        println!("missing across {} name(s):", missing.len());
        for name in &missing {
            println!("  {name}");
        }
    }
}
