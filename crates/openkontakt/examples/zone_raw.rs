//! Hexdump two zones' public and private sections side by side.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let a: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let b: usize = std::env::args()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let instrument = Instrument::load(&path).unwrap();
    for index in [a, b] {
        let zone = &instrument.zones[index];
        println!(
            "zone {index}: public {} B, private {} B, tail {} B",
            zone.raw_public.len(),
            zone.private.len(),
            zone.public_tail.len()
        );
        println!("  public : {}", hex(&zone.raw_public));
        println!("  tail   : {}", hex(&zone.public_tail));
        println!("  private: {}", hex(&zone.private));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("")
}
