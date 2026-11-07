//! Every gain stage the engine multiplies into a voice, for the zones a given key hits.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: gain_probe <instrument.nki> [key] [velocity]");
    let key: i16 = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(67);
    let velocity: i16 = std::env::args()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(96);
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let params = &instrument.program.params;
    println!(
        "program: volume={} pan={} transpose={}",
        params.volume, params.pan, params.transpose
    );
    let mut total = 0.0f64;
    for zone in &instrument.zones {
        if !zone.matches(key as u8, velocity as u8) {
            continue;
        }
        let group = instrument.group_for_zone(zone).unwrap();
        if group.muted {
            continue;
        }
        let gain = params.volume as f64 * zone.volume as f64 * group.volume as f64;
        println!(
            "zone {:<3} group {:<12} zone.volume={:.4} group.volume={:.4} pan={:+.3} tune={:.4} -> {:.4} ({:+.2} dB)",
            zone.index,
            group.name,
            zone.volume,
            group.volume,
            zone.pan + group.pan,
            zone.tune,
            gain,
            20.0 * gain.log10()
        );
        total += gain;
    }
    println!(
        "sum of in-phase gains: {:.4} ({:+.2} dB), velocity gain linear {:.4} ({:+.2} dB)",
        total,
        20.0 * total.log10(),
        velocity as f64 / 127.0,
        20.0 * (velocity as f64 / 127.0).log10()
    );
}
