//! Print script slot metadata.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: slot_dump <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    for slot in &instrument.scripts {
        println!(
            "slot {}: bypass={} desc={:?} textfile={:?} len={}",
            slot.index,
            slot.bypass,
            slot.description,
            slot.textfile_name,
            slot.text.as_ref().map(|t| t.len()).unwrap_or(0)
        );
    }
}
