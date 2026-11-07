//! Look for a per-zone gain in the private block.
use openkontakt::Instrument;
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    let mut histogram: BTreeMap<String, usize> = BTreeMap::new();
    for zone in &instrument.zones {
        let value = zone
            .private
            .get(12..16)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(f32::NAN);
        *histogram.entry(format!("{value}")).or_default() += 1;
        if instrument.zones.len() <= 20 {
            println!(
                "zone {:<4} keys {:3}..{:3} vol {:.4} private[12] = {value}",
                zone.index, zone.low_key, zone.high_key, zone.volume
            );
        }
    }
    println!("histogram of private[12..16] as f32: {histogram:?}");
    // and the byte right after it
    let mut next: BTreeMap<String, usize> = BTreeMap::new();
    for zone in &instrument.zones {
        let value = zone
            .private
            .get(16..20)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(f32::NAN);
        *next.entry(format!("{value}")).or_default() += 1;
    }
    println!("histogram of private[16..20] as f32: {next:?}");
}
