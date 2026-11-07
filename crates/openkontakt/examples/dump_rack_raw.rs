//! Write the raw FX rack chunks of an instrument, for offline inspection.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_rack_raw <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/rack.bin".into());
    let mut bytes = Vec::new();
    let mut count = 0;
    for child in &instrument.program.children {
        if child.id == 0x3A {
            bytes.extend_from_slice(&child.body);
            count += 1;
        }
    }
    std::fs::write(&out, &bytes).unwrap();
    println!("{count} rack chunks, {} bytes -> {out}", bytes.len());
}
