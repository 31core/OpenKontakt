//! Print zone key ranges, roots and the sample each one resolves to.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: zone_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    println!(
        "program tune={} pan={}",
        instrument.program.params.tune, instrument.program.params.pan
    );
    let limit: usize = std::env::args()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or(40);
    for (index, zone) in instrument.zones.iter().enumerate().take(limit) {
        let sample = instrument
            .sample_path_for_zone(zone)
            .map(|path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default();
        let group = instrument
            .group_for_zone(zone)
            .map(|g| format!("{}({})", g.name, g.index))
            .unwrap_or_default();
        let (sample_root, tuning) = zone
            .sample
            .as_ref()
            .map(|s| (s.root_note, s.tuning))
            .unwrap_or((0, 0.0));
        println!(
            "zone {index:3} {:<12} vel={:3}..{:3} key={:3}..{:3} zone_root={:3} sample_root={:3} tune={:+.3} zone_tune={:+.3}  {}",
            group,
            zone.low_velocity,
            zone.high_velocity,
            zone.low_key,
            zone.high_key,
            zone.root_key,
            sample_root,
            tuning,
            zone.tune,
            sample
        );
    }
}
