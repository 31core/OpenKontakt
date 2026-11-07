//! Emit `path<TAB>root_key<TAB>low_key<TAB>high_key<TAB>group` for pitch cross-checks.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: zone_tsv <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    for zone in &instrument.zones {
        let Some(sample) = instrument.sample_path_for_zone(zone) else {
            continue;
        };
        println!(
            "{}\t{}\t{}\t{}\t{}",
            sample.display(),
            zone.root_key,
            zone.low_key,
            zone.high_key,
            zone.group_index
        );
    }
}
