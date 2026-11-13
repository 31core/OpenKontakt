//! Follow the round-robin counters across a few note-on/note-off pairs.
use openkontakt::ksp::{EventContext, GroupInfo};
use openkontakt::{Instrument, KspRuntime};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let note: i64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(67);
    let instrument = Instrument::load(&path).unwrap();
    let root = instrument
        .source_path
        .parent()
        .unwrap()
        .display()
        .to_string();
    let texts: Vec<Option<String>> = instrument.scripts.iter().map(|s| s.text.clone()).collect();
    let groups: Vec<GroupInfo> = instrument
        .groups
        .iter()
        .map(|g| GroupInfo {
            release_trigger: g.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(g),
        })
        .collect();
    let runtime = KspRuntime::compile_with_groups(&texts, &root, groups);
    runtime.initialize();
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|e| (e.name.clone(), e.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }
    for message in runtime.diagnostics() {
        println!("diagnostic: {message}");
    }
    for i in 0..6 {
        let d = runtime.dispatch(
            "note",
            EventContext {
                note,
                velocity: 64,
                id: i + 1,
                status: 1,
            },
        );
        let allowed: Vec<usize> = d
            .allowed_groups
            .as_ref()
            .map(|m| {
                m.iter()
                    .enumerate()
                    .filter(|(_, b)| **b)
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        println!("note {i}: allowed {allowed:?} actions {}", d.actions.len());
        for v in ["Art_01_RR_Counter", "Art_Buffer", "RR_Counter", "keys_down"] {
            println!("  {v} = {:?}", runtime.variable(2, v));
        }
        runtime.dispatch(
            "release",
            EventContext {
                note,
                velocity: 0,
                id: i + 1,
                status: 0,
            },
        );
        for v in [
            "Art_01_RR_Counter",
            "Art_Buffer",
            "RR_Counter",
            "keys_down",
            "Sequencer_Bypass",
        ] {
            println!("  after release {v} = {:?}", runtime.variable(2, v));
        }
    }
}
