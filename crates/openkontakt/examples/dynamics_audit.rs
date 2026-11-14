//! Inspect stored amplitude routes and their live KSP state without changing the instrument.
use openkontakt::{Instrument, KspRuntime, envelope, ksp::EventContext, modulation::Source};

fn snapshot(label: &str, runtime: &KspRuntime, instrument: &Instrument) {
    println!("\n{label}");
    for cc in [70, 71, 72, 84] {
        println!("CC {cc} = {}", runtime.engine.controller(cc));
    }
    for index in [4usize, 7, 84, 87] {
        let Some(group) = instrument.groups.get(index) else {
            continue;
        };
        println!(
            "group {index} {} base {:?}",
            group.name,
            envelope::group_amp_envelope(group)
        );
        for (name, value) in runtime.engine.cached_parameters() {
            if name.split('|').nth(1).and_then(|s| s.parse::<usize>().ok()) == Some(index)
                && (name.contains("ATTACK")
                    || name.contains("DECAY")
                    || name.contains("SUSTAIN")
                    || name.contains("RELEASE")
                    || name.contains("MOD_TARGET"))
            {
                println!("  cached {name} = {value}");
            }
        }
        for source in &runtime.engine.modulation[index].modulators {
            if !matches!(source.source, Source::Velocity | Source::Controller { .. }) {
                continue;
            }
            for target in &source.targets {
                if target.parameter == "volume" || target.parameter.starts_with("ahdsr_") {
                    println!(
                        "  {} {:?} -> {} slot {:?}, amount {}, flag {}, curve {:?}",
                        source.name,
                        source.source,
                        target.parameter,
                        target.destination,
                        target.amount,
                        target.inverted,
                        target.curve
                    );
                    if matches!(source.source, Source::Controller { .. }) {
                        println!(
                            "    effective source {:?}",
                            runtime.engine.controller_source_value(&source.source)
                        );
                    }
                }
            }
        }
    }
}

fn main() {
    let instrument = Instrument::load(std::env::args().nth(1).expect("instrument path")).unwrap();
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
    runtime.initialize();
    let persistent = instrument
        .scripts
        .iter()
        .map(|s| {
            (
                s.index,
                s.persistent
                    .iter()
                    .map(|p| (p.name.clone(), p.values.clone()))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    runtime.apply_persistent_batch(&persistent);
    for slot in 0..runtime.slot_count() {
        for control in runtime.controls(slot) {
            if [
                "Velocity",
                "Dial_Top_2",
                "Dial_Top_Bk_2",
                "Attack",
                "Decay",
                "Sustain",
                "Release",
            ]
            .contains(&control.name.as_str())
            {
                println!("control {slot}:{} = {}", control.name, control.value);
            }
        }
    }
    snapshot("after preset restore", &runtime, &instrument);
    runtime.dispatch(
        "note",
        EventContext {
            note: 69,
            velocity: 75,
            id: 1,
            status: 1,
        },
    );
    snapshot("after first note", &runtime, &instrument);
}
