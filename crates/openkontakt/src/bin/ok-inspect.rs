use openkontakt::{Instrument, load_samples};
use std::path::PathBuf;

/// Put the saved values into the runtime before anything reads it.
///
/// A patch is not its declarations: a loaded instrument shows the values saved with it, and some
/// panels only lay themselves out in the `on ui_control` handlers those values replay.
fn restore_stored_values(runtime: &openkontakt::KspRuntime, instrument: &Instrument) {
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: ok-inspect <instrument.nki> [--samples] [--zones] [--scripts] [--persistent] \
             [--keys] [--dump] [--press NAME] [--note KEY] [--listener N] [--var NAME]"
        );
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
                "  resolved sample files={} packed={} unresolved={}",
                stats.resolved_sample_files,
                stats.packed_sample_files,
                stats.unresolved_sample_files
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
            if std::env::args().any(|arg| arg == "--keys") {
                // Key colours only exist once the scripts have run, so build the runtime here.
                let root = instrument
                    .source_path
                    .parent()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let texts: Vec<Option<String>> = instrument
                    .scripts
                    .iter()
                    .map(|slot| slot.text.clone())
                    .collect();
                let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
                    .groups
                    .iter()
                    .map(|group| openkontakt::ksp::GroupInfo {
                        release_trigger: group.release_trigger,
                        envelope: openkontakt::envelope::group_amp_envelope(group),
                    })
                    .collect();
                let runtime = openkontakt::KspRuntime::compile_with_groups(&texts, &root, groups);
                runtime.initialize();
                restore_stored_values(&runtime, &instrument);
                for message in runtime.diagnostics() {
                    println!("  [ksp] {message}");
                }
                let colours = runtime.engine.key_colors();
                let swatches: Vec<String> = colours
                    .iter()
                    .enumerate()
                    .filter_map(|(note, colour)| {
                        colour.map(|index| {
                            format!("{note}:{}", openkontakt::ksp::key_color_name(index))
                        })
                    })
                    .collect();
                println!("  key colours: {}", swatches.join(" "));
            }
            if std::env::args().any(|arg| arg == "--scripts") {
                for slot in &instrument.scripts {
                    let text = slot.text.as_deref().unwrap_or_default();
                    println!(
                        "  script {}: {} bytes, {} persistent, bypass={}{}",
                        slot.index,
                        text.len(),
                        slot.persistent.len(),
                        slot.bypass,
                        if slot.password.is_empty() {
                            String::new()
                        } else {
                            let hex: Vec<String> = slot
                                .password
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect();
                            format!(", password {} B [{}]", slot.password.len(), hex.join(" "))
                        }
                    );
                    println!("{text}");
                }
            }
            if std::env::args().any(|arg| arg == "--persistent") {
                for slot in &instrument.scripts {
                    for entry in &slot.persistent {
                        let head: Vec<String> = entry
                            .values
                            .iter()
                            .take(40)
                            .map(|value| value.to_string())
                            .collect();
                        println!(
                            "  persistent slot {} {:?}: {} value(s) [{}]{}",
                            slot.index,
                            entry.name,
                            entry.values.len(),
                            head.join(" "),
                            if entry.values.len() > 40 { " ..." } else { "" }
                        );
                    }
                }
            }
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
            if let Some(index) = instrument.nkc_index() {
                // Group the catalogue by extension: a packed sample library and a resource-only
                // catalogue look very different, and the counts say which one this is.
                let mut kinds: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for entry in &index.entries {
                    let extension = entry
                        .name
                        .rsplit_once('.')
                        .map(|(_, tail)| tail.to_ascii_lowercase())
                        .unwrap_or_else(|| "(none)".into());
                    *kinds.entry(extension).or_default() += 1;
                }
                let kinds: Vec<String> = kinds
                    .iter()
                    .map(|(extension, count)| format!("{count} .{extension}"))
                    .collect();
                println!(
                    "  nkc index: {} entries (sub 0x{:04X}, {})",
                    index.entries.len(),
                    index.sub,
                    kinds.join(", ")
                );
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
            if std::env::args().any(|arg| arg == "--panel") {
                // The *executed* panel: scripts move, relabel and repaint controls from their
                // callbacks, so this compiles the scripts first.
                let root = instrument
                    .source_path
                    .parent()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let texts: Vec<Option<String>> = instrument
                    .scripts
                    .iter()
                    .map(|slot| slot.text.clone())
                    .collect();
                let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
                    .groups
                    .iter()
                    .map(|group| openkontakt::ksp::GroupInfo {
                        release_trigger: group.release_trigger,
                        envelope: openkontakt::envelope::group_amp_envelope(group),
                    })
                    .collect();
                let runtime = openkontakt::KspRuntime::compile_with_groups(&texts, &root, groups);
                runtime.initialize();
                restore_stored_values(&runtime, &instrument);
                // `--press NAME` runs that control's `on ui_control`, which is what a click does.
                if let Some(position) = std::env::args().position(|arg| arg == "--press") {
                    if let Some(name) = std::env::args().nth(position + 1) {
                        let mut pressed = false;
                        for slot in 0..runtime.slot_count() {
                            if runtime.set_control_normalized(slot, &name, 1.0) {
                                pressed = true;
                            }
                        }
                        println!("  pressed {name}: {pressed}");
                    }
                }
                // `--note KEY` feeds a note-on: a panel's listener usually only steps while a key
                // is held, because the scripts gate the LFO on `$keys_down`.
                if let Some(position) = std::env::args().position(|arg| arg == "--note") {
                    let key: i64 = std::env::args()
                        .nth(position + 1)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(60);
                    runtime.dispatch(
                        "note",
                        openkontakt::ksp::EventContext {
                            note: key,
                            velocity: 100,
                            id: 1,
                            status: 1,
                        },
                    );
                }
                // `--listener N` runs N listener ticks, which is what the audio engine's timer
                // does while the host is playing.
                if let Some(position) = std::env::args().position(|arg| arg == "--listener") {
                    let ticks: usize = std::env::args()
                        .nth(position + 1)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1);
                    for slot in 0..runtime.slot_count() {
                        for _ in 0..ticks {
                            runtime.listener(slot);
                        }
                    }
                }
                // `--var NAME` prints one script variable per slot; a panel's state machine is
                // unreadable from the controls alone.
                if let Some(position) = std::env::args().position(|arg| arg == "--var") {
                    if let Some(name) = std::env::args().nth(position + 1) {
                        for slot in 0..runtime.slot_count() {
                            if let Some(value) = runtime.variable(slot, &name) {
                                println!("  slot {slot} {name} = {value}");
                            }
                        }
                    }
                }
                for slot in 0..runtime.slot_count() {
                    if let Some(period) = runtime.listener_period_ms(slot) {
                        println!("  slot {slot} listener every {period} ms");
                    }
                    for control in runtime.controls(slot) {
                        // A table is bound to an array, so say what it currently holds; an empty
                        // one draws no bars and looks like a broken panel.
                        let table = if control.kind == openkontakt::ksp::parser::UiKind::Table {
                            let head: Vec<String> = control
                                .table
                                .iter()
                                .take(6)
                                .map(|value| value.to_string())
                                .collect();
                            format!(
                                " table={} [{}]{}",
                                control.table.len(),
                                head.join(" "),
                                if control.table.len() > 6 { " ..." } else { "" }
                            )
                        } else {
                            String::new()
                        };
                        println!(
                            "  slot {} {:>10} {:>9} {:>5},{:<5} {:>4}x{:<4} value={:<6} hidden={:<3} picture={:?} text={:?}{table}",
                            slot,
                            control.name,
                            format!("{:?}", control.kind),
                            control.x,
                            control.y,
                            control.pixel_width.unwrap_or(control.width),
                            control.pixel_height.unwrap_or(control.height),
                            control.value,
                            control.hidden_parts,
                            control.picture,
                            control.label,
                        );
                    }
                }
            }
            if std::env::args().any(|arg| arg == "--envelope-raw") {
                for group in &instrument.groups {
                    for child in &group.children {
                        if child.id == 0x3B {
                            let hex: Vec<String> = child
                                .body
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect();
                            println!(
                                "  group {} 0x3B ({} B): {}",
                                group.index,
                                child.body.len(),
                                hex.join(" ")
                            );
                        }
                    }
                }
            }
            if std::env::args().any(|arg| arg == "--trailer") {
                let wanted: usize = std::env::args()
                    .nth(
                        std::env::args()
                            .position(|arg| arg == "--trailer")
                            .unwrap_or(0)
                            + 1,
                    )
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                for group in &instrument.groups {
                    if group.index != wanted {
                        continue;
                    }
                    let start = group.private.len().saturating_sub(206);
                    let body = &group.private[start..];
                    for (line, chunk) in body.chunks(16).enumerate() {
                        let text: String = chunk
                            .iter()
                            .map(|b| {
                                if (32..127).contains(b) {
                                    *b as char
                                } else {
                                    '.'
                                }
                            })
                            .collect();
                        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                        println!("  {:4}  {:<47} {text}", line * 16, hex.join(" "));
                    }
                }
            }
            if std::env::args().any(|arg| arg == "--racks") {
                for group in &instrument.groups {
                    let Some(rack) = openkontakt::effects::group_rack(&group.private) else {
                        continue;
                    };
                    let used: Vec<String> = rack
                        .slots
                        .iter()
                        .enumerate()
                        .filter_map(|(index, slot)| {
                            let slot = slot.as_ref()?;
                            Some(format!(
                                "{index}:0x{:02X}{}{}",
                                slot.module,
                                if slot.bypass { "(bypassed)" } else { "" },
                                match openkontakt::effects::decode(slot) {
                                    openkontakt::effects::ModuleParams::Filter {
                                        cutoff,
                                        resonance,
                                    } => format!(" cutoff={cutoff:.4} res={resonance:.4}"),
                                    _ => String::new(),
                                }
                            ))
                        })
                        .collect();
                    if !used.is_empty() {
                        println!("  group {} rack: {}", group.index, used.join(" "));
                    }
                }
            }
            if std::env::args().any(|arg| arg == "--envelopes") {
                for group in &instrument.groups {
                    match openkontakt::envelope::group_amp_envelope(group) {
                        Some(env) => println!(
                            "  group {} {}: attack {:.1} ms (curve {:.2}) hold {:.1} decay {:.1} sustain {:.3} release {:.1}",
                            group.index,
                            group.name,
                            env.attack_ms,
                            env.attack_curve,
                            env.hold_ms,
                            env.decay_ms,
                            env.sustain,
                            env.release_ms
                        ),
                        None => println!("  group {} {}: no envelope", group.index, group.name),
                    }
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
