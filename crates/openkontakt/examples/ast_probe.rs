//! Print the parsed callback bodies of one script slot.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let slot: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let instrument = Instrument::load(&path).unwrap();
    let text = instrument.scripts[slot].text.as_deref().unwrap_or("");
    let program = openkontakt::ksp::parser::parse(text).unwrap();
    for cb in &program.callbacks {
        if cb.kind == "release" || cb.kind == "pgs_changed" {
            println!("=== {} arg {:?}", cb.kind, cb.arg);
            println!("{:#?}", cb.body);
        }
    }
}
