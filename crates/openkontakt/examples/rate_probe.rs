//! Compare the sample rate the instrument declares with the one the WAV actually has.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: rate_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let mut seen = std::collections::BTreeMap::new();
    for zone in instrument.zones.iter().take(60) {
        let declared = zone.sample.as_ref().map(|s| s.sample_rate).unwrap_or(0);
        let actual = instrument
            .sample_path_for_zone(zone)
            .and_then(|p| openkontakt::wav::parse_wav_info(p).ok())
            .map(|info| info.0.sample_rate as i32)
            .unwrap_or(-1);
        *seen.entry((declared, actual)).or_insert(0usize) += 1;
    }
    for ((declared, actual), count) in seen {
        println!("declared={declared:6} wav={actual:6}  ({count} zones)");
    }
}
