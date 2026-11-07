use openkontakt::Instrument;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    for s in &instrument.scripts {
        if s.is_empty() {
            continue;
        }
        println!("=== slot {} ===", s.index);
        println!("{}", s.text.as_deref().unwrap_or(""));
    }
}
