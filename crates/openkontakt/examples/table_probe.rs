//! Check that editing a `ui_table` step lands in the control snapshot.
use openkontakt::{Instrument, KspRuntime};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: table_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument.scripts.iter().map(|s| s.text.clone()).collect();
    let runtime = KspRuntime::compile(&texts, &root);
    runtime.initialize();
    for slot in 0..runtime.slot_count() {
        let tables: Vec<_> = runtime
            .controls(slot)
            .into_iter()
            .filter(|c| !c.table.is_empty())
            .collect();
        for table in tables {
            println!(
                "slot {slot} table {} steps={} first={:?}",
                table.name,
                table.table.len(),
                &table.table[..table.table.len().min(6)]
            );
            let ok = runtime.set_table_element(slot, &table.name, 1, 99);
            let after = runtime
                .controls(slot)
                .into_iter()
                .find(|c| c.name == table.name)
                .map(|c| c.table.get(1).copied());
            println!("   set_table_element -> {ok}, step1 now {after:?}");
        }
    }
}
