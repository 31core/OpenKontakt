//! Trace panel callbacks through shared state: control_audit <nki> [slot:name=value ...].
use openkontakt::{Instrument, KspRuntime};
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let instrument = Instrument::load(args.next().expect("instrument path")).unwrap();
    let texts = instrument
        .scripts
        .iter()
        .map(|s| s.text.clone())
        .collect::<Vec<_>>();
    let runtime = KspRuntime::compile_instrument(
        &texts,
        &instrument
            .source_path
            .parent()
            .unwrap()
            .display()
            .to_string(),
        &instrument.groups,
    );
    for g in &instrument.groups {
        runtime.engine.set_group_gain(g.index, g.volume);
    }
    runtime.initialize();
    let persistent = instrument
        .scripts
        .iter()
        .map(|slot| {
            (
                slot.index,
                slot.persistent
                    .iter()
                    .map(|e| (e.name.clone(), e.values.clone()))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    runtime.apply_persistent_batch(&persistent);
    println!(
        "loaded: {} groups, {} scripts",
        instrument.groups.len(),
        runtime.slot_count()
    );
    let mut before = runtime
        .engine
        .cached_parameters()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for command in args {
        if command == "--envelopes" {
            let event = runtime.dispatch(
                "note",
                openkontakt::ksp::EventContext {
                    note: 60,
                    velocity: 64,
                    id: 1,
                    status: 1,
                },
            );
            for g in instrument
                .groups
                .iter()
                .filter(|g| {
                    event
                        .allowed_groups
                        .as_ref()
                        .map(|mask| mask.get(g.index).copied().unwrap_or(false))
                        .unwrap_or(true)
                })
                .take(8)
            {
                if let Some(base) = openkontakt::envelope::group_amp_envelope(g) {
                    println!(
                        "envelope group {}: {:?}",
                        g.index,
                        runtime.engine.modulate_amp_envelope(g.index, base)
                    );
                }
            }
            continue;
        }
        let (slot, action) = command.split_once(':').expect("slot:name=value");
        let (name, value) = action.split_once('=').expect("name=value");
        let slot: usize = slot.parse().unwrap();
        let ok = runtime.set_control_value(slot, name, value.parse().unwrap());
        let stored = runtime
            .controls(slot)
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.value);
        println!("{command}: ok={ok}, stored={stored:?}");
        let after = runtime
            .engine
            .cached_parameters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for (key, value) in &after {
            if before.get(key) != Some(value) {
                println!("  {key}={value}");
            }
        }
        before = after;
    }
    for message in runtime.diagnostics() {
        println!("diagnostic: {message}");
    }
}
