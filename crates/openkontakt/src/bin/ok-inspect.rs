use openkontakt::{Instrument, load_samples};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: ok-inspect <instrument.nki> [--samples] [--zones]");
        std::process::exit(2);
    };
    let path = PathBuf::from(path);
    match Instrument::load(&path) {
        Ok(instrument) => {
            let stats = instrument.stats();
            let params = &instrument.program.params;
            println!("{}", instrument.name);
            println!(
                "  container={} version=0x{:02X} file={} B",
                instrument.container.container,
                instrument.program.version,
                instrument.container.file_bytes
            );
            println!(
                "  groups={} zones={} scripts={} private={} B",
                stats.groups,
                stats.zones,
                stats.scripts,
                instrument.program.private.len()
            );
            println!(
                "  volume={:.4} pan={:.4} tune={:.4} keys={}-{} vel={}-{}",
                params.volume,
                params.pan,
                params.tune,
                params.low_key,
                params.high_key,
                params.low_velocity,
                params.high_velocity
            );
            if let Some(table) = &instrument.filename_table {
                println!(
                    "  filename table v{}: {} samples, {} others, {} trailing B",
                    table.version,
                    table.sample_count(),
                    table.other.len(),
                    table.trailing.len()
                );
            }
            println!(
                "  resolved sample files={} unresolved={}",
                stats.resolved_sample_files, stats.unresolved_sample_files
            );
            if let Some(wallpaper) = &instrument.wallpaper_path {
                println!("  wallpaper={}", wallpaper.display());
            } else if let Some(requested) = &instrument.wallpaper_requested {
                println!("  wallpaper unresolved: {requested}");
            }
            println!(
                "  KSP UI controls={} ({})",
                instrument.ksp_panel.controls.len(),
                if instrument.ksp_panel.has_ui {
                    "parsed"
                } else {
                    "none"
                }
            );
            // Round robin is *inferred*: see the note on `Group::round_robin`. Print the cycles
            // this reader believes in, so a library that looks wrong can be checked by eye.
            let mut grouped: std::collections::BTreeMap<Vec<u8>, Vec<(usize, String, u32)>> =
                std::collections::BTreeMap::new();
            for group in &instrument.groups {
                if let Some((position, key)) = group.round_robin() {
                    grouped.entry(key).or_default().push((
                        group.index,
                        group.name.clone(),
                        position,
                    ));
                }
            }
            let cycles = grouped;
            for members in cycles.into_values().filter(|members| members.len() > 1) {
                let mut members = members;
                members.sort_by_key(|(_, _, position)| *position);
                let label: Vec<String> = members
                    .iter()
                    .map(|(index, name, position)| format!("{index}:{name}@{position}"))
                    .collect();
                println!("  round robin (inferred): {}", label.join("  "));
            }
            for diagnostic in &instrument.diagnostics {
                println!(
                    "  [{:?}] {}: {}",
                    diagnostic.severity, diagnostic.area, diagnostic.message
                );
            }
            if std::env::args().any(|arg| arg == "--dump") {
                for group in &instrument.groups {
                    println!(
                        "GROUP\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        group.index,
                        group.version,
                        group.private.len(),
                        group.raw_public.len(),
                        group.name,
                        group.volume,
                        group.pan,
                        group.tune,
                        group.key_tracking as u8,
                        group.midi_channel,
                    );
                }
                for zone in &instrument.zones {
                    let sample = zone.sample.as_ref();
                    println!(
                        "ZONE\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{}",
                        zone.index,
                        zone.group_index,
                        zone.version,
                        zone.low_velocity,
                        zone.high_velocity,
                        zone.low_key,
                        zone.high_key,
                        zone.root_key,
                        zone.volume,
                        zone.pan,
                        zone.tune,
                        sample.map(|s| s.sample_rate).unwrap_or_default(),
                        sample.map(|s| s.channels).unwrap_or_default(),
                        sample.map(|s| s.frames).unwrap_or_default(),
                        sample.map(|s| s.filename_id).unwrap_or(-1),
                        instrument
                            .sample_path_for_zone(zone)
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    );
                }
            }
            if std::env::args().any(|arg| arg == "--zones") {
                for zone in instrument.zones.iter().take(20) {
                    println!(
                        "  zone {:3} g{} key {}-{} vel {}-{} root {} sample {:?}",
                        zone.index,
                        zone.group_index,
                        zone.low_key,
                        zone.high_key,
                        zone.low_velocity,
                        zone.high_velocity,
                        zone.root_key,
                        zone.sample.as_ref().map(|s| s.filename_id)
                    );
                }
            }
            if std::env::args().any(|arg| arg == "--samples") {
                let loaded = load_samples(&instrument);
                println!(
                    "  loaded {} sample files, {:.1} MiB",
                    loaded.samples.len(),
                    loaded.bytes_loaded as f64 / 1024.0 / 1024.0
                );
                for diagnostic in loaded.diagnostics.iter().take(10) {
                    println!("  [sample] {}", diagnostic.message);
                }
            }
        }
        Err(error) => {
            eprintln!("failed: {error}");
            std::process::exit(1);
        }
    }
}
