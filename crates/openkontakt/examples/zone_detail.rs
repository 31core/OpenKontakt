//! Print the zones a key hits, with their sample, start/end and loop.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let key: i16 = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let velocity: i16 = std::env::args()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let instrument = Instrument::load(&path).unwrap();
    for zone in &instrument.zones {
        if !zone.matches(key as u8, velocity as u8) {
            continue;
        }
        let sample = instrument
            .sample_path_for_zone(zone)
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .unwrap_or_default();
        let loop_info = zone
            .main_loop()
            .map(|l| (l.mode, l.loop_start, l.loop_length));
        println!(
            "zone {:<4} group {:<4} key {}..{} root {} vel {}..{}  start={} end={} vol={:.4}  {} loop={:?}",
            zone.index,
            zone.group_index,
            zone.low_key,
            zone.high_key,
            zone.root_key,
            zone.low_velocity,
            zone.high_velocity,
            zone.sample_start,
            zone.sample_end,
            zone.volume,
            sample,
            loop_info
        );
    }
}
