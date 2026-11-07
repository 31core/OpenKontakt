//! Tabulate the private[12..16] value against key range, velocity range and group.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    let mut rows: Vec<(i16, i16, i16, i16, usize, f32, f32)> = Vec::new();
    for zone in &instrument.zones {
        let value = zone
            .private
            .get(12..16)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(f32::NAN);
        rows.push((
            zone.low_key,
            zone.high_key,
            zone.low_velocity,
            zone.high_velocity,
            zone.group_index,
            value,
            zone.volume,
        ));
    }
    rows.sort_by(|a, b| {
        (a.4, a.2, a.0)
            .partial_cmp(&(b.4, b.2, b.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut last_group = usize::MAX;
    for (low, high, vlow, vhigh, group, value, volume) in rows {
        if group != last_group {
            println!("-- group {group}");
            last_group = group;
        }
        println!(
            "   keys {low:3}..{high:3} vel {vlow:3}..{vhigh:3}  private12={value:5}  zone volume={volume:.4}"
        );
    }
}
