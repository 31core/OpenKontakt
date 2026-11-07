//! Drive a KSP control the way the panel does and report the engine parameter it writes.
use openkontakt::envelope::group_amp_envelope;
use openkontakt::ksp::GroupInfo;
use openkontakt::{Instrument, KspRuntime};
use std::sync::Arc;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let control = std::env::args().nth(2).unwrap_or_else(|| "reverb".into());
    let instrument = Instrument::load(&path).unwrap();
    let root = instrument.library_root.display().to_string();
    let texts: Vec<Option<String>> = instrument.scripts.iter().map(|s| s.text.clone()).collect();
    let groups: Vec<GroupInfo> = instrument
        .groups
        .iter()
        .map(|g| GroupInfo {
            release_trigger: g.release_trigger,
            envelope: group_amp_envelope(g),
        })
        .collect();
    let runtime = Arc::new(KspRuntime::compile_with_groups(&texts, &root, groups));
    runtime.initialize();
    for slot in &instrument.scripts {
        let entries: Vec<(String, Vec<i64>)> = slot
            .persistent
            .iter()
            .map(|e| (e.name.clone(), e.values.clone()))
            .collect();
        runtime.apply_persistent(slot.index, &entries);
    }
    let param = "ENGINE_PAR_SENDLEVEL_0";
    println!(
        "after load:                 {param} = {:?}",
        runtime.engine.parameter_any(param)
    );
    for slot in 0..runtime.slot_count() {
        for c in runtime.controls(slot) {
            if c.name.eq_ignore_ascii_case(&control) {
                println!("found control {} in slot {slot}, value {}", c.name, c.value);
            }
        }
    }
    for choice in 0..8 {
        let ok = runtime.set_control_value(0, &control, choice);
        let value = runtime
            .controls(0)
            .iter()
            .find(|c| c.name == control)
            .map(|c| c.value)
            .unwrap_or(-1);
        println!("  select {choice} -> ok {ok}, value {value}");
    }
    runtime.set_control_value(0, &control, 0);
    runtime.set_control_value(2, &control, 0);
    println!(
        "after setting {control} = 0: {param} = {:?}",
        runtime.engine.parameter_any(param)
    );
    runtime.set_control_value(2, &control, 348731);
    println!(
        "after restoring it:         {param} = {:?}",
        runtime.engine.parameter_any(param)
    );

    // Picture buttons: clicking one runs its `on ui_control`, which is how radio groups work.
    let names: Vec<String> = (0..runtime.slot_count())
        .flat_map(|slot| runtime.controls(slot))
        .filter(|c| {
            use openkontakt::ksp::parser::UiKind;
            matches!(c.kind, UiKind::Button | UiKind::Switch)
        })
        .map(|c| {
            println!(
                "  {:<16} {:?} px={:?}x{:?} value={}",
                c.name, c.kind, c.pixel_width, c.pixel_height, c.value
            );
            c.name.clone()
        })
        .collect();
    for name in [
        "timbral_switch",
        "ButtonFwd1",
        "ButtonRev1",
        "ButtonFwdRev1",
        "tremoloLFO",
    ] {
        let ok = runtime.set_control_value(0, name, 1);
        println!("set_control_value({name}, 1) -> {ok}");
        println!("  diagnostics: {:?}", runtime.diagnostics());
        let state: Vec<(String, i64)> = (0..runtime.slot_count())
            .flat_map(|slot| runtime.controls(slot))
            .filter(|c| {
                use openkontakt::ksp::parser::UiKind;
                matches!(c.kind, UiKind::Button | UiKind::Switch)
            })
            .map(|c| (c.name.clone(), c.value))
            .collect();
        println!("after clicking {name}: {state:?}");
    }
}
