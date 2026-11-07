use openkontakt::Instrument;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    for group in &instrument.groups {
        println!(
            "group {} {:<28} private_bus={:?}",
            group.index, group.name, group.private_bus
        );
    }
}
