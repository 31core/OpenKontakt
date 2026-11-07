//! Print what the KSP layer makes of an instrument: compiled slots, declared controls, and any
//! parse or runtime diagnostics.
//!
//! ```text
//! cargo run -p openkontakt --example ksp_probe -- path/to/instrument.nki
//! ```

use openkontakt::ksp::EventContext;
use openkontakt::{Instrument, KspRuntime};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: ksp_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
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
    println!(
        "groups: {} (release-trigger: {:?})",
        groups.len(),
        groups.iter().map(|g| g.release_trigger).collect::<Vec<_>>()
    );
    let runtime = KspRuntime::compile_with_groups(&texts, &root, groups);
    runtime.initialize();
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }

    println!("instrument: {}", instrument.name);
    println!(
        "panel: {:.0}x{:.0} px",
        instrument.ksp_panel.ui_width_px, instrument.ksp_panel.ui_height_px
    );
    println!("slots: {}", runtime.slot_count());
    for slot in 0..runtime.slot_count() {
        let controls = runtime.controls(slot);
        if controls.is_empty() {
            continue;
        }
        println!(
            "slot {slot}: {} controls (initialized: {})",
            controls.len(),
            runtime.is_initialized(slot)
        );
        for control in controls {
            println!(
                "  {:<28} {:?} value={} range={}..{} pos=({},{}) {}x{}",
                format!("{} [{}]", control.name, control.label),
                control.kind,
                control.value,
                control.min,
                control.max,
                control.x,
                control.y,
                control.width,
                control.height
            );
            println!("      px={} picture={:?}", control.px, control.picture);
        }
    }
    println!(
        "bus gains: {:?}",
        (0..8)
            .map(|bus| runtime.engine.bus_gain(bus))
            .collect::<Vec<_>>()
    );

    for (label, event) in [
        (
            "note",
            EventContext {
                note: 60,
                velocity: 100,
                id: 1,
                status: 1,
            },
        ),
        (
            "release",
            EventContext {
                note: 60,
                velocity: 100,
                id: 1,
                status: 0,
            },
        ),
    ] {
        let dispatch = runtime.dispatch(label, event);
        println!(
            "on {label}: ignore_event={} actions={:?}",
            dispatch.ignore_event, dispatch.actions
        );
        match &dispatch.allowed_groups {
            None => println!("  groups: script did not route"),
            Some(mask) => println!(
                "  groups: {} of {} allowed -> {:?}",
                mask.iter().filter(|allowed| **allowed).count(),
                mask.len(),
                mask
            ),
        }
    }
    if let Ok(request) = runtime.engine.ir_request.lock() {
        println!("IR request: {:?}", *request);
    }
    if let Ok(messages) = runtime.engine.messages.lock() {
        if !messages.is_empty() {
            println!("messages: {messages:?}");
        }
    }
    let mut cached = runtime.engine.cached_parameters();
    cached.sort();
    println!("engine parameters: {cached:?}");
    let diagnostics = runtime.diagnostics();
    if diagnostics.is_empty() {
        println!("diagnostics: none");
    } else {
        println!("diagnostics:");
        for diagnostic in diagnostics {
            println!("  {diagnostic}");
        }
    }
}
