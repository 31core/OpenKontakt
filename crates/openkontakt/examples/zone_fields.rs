//! Print every parsed field of the zones that a key hits.
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
        let z = &instrument.zones[index];
        println!(
            "zone {index}: version=0x{:02X} flag={} keys {}..{} root {} vol={:.4} pan={:.3} tune={:.4}",
            z.version, z.flag, z.low_key, z.high_key, z.root_key, z.volume, z.pan, z.tune
        );
        match &z.sample {
            Some(s) => println!(
                "   sample: rate={} ch={} frames={} root={} tuning={:.4} res1={} res2={:?} res3={} res4={} prefix={:02X?}",
                s.sample_rate,
                s.channels,
                s.frames,
                s.root_note,
                s.tuning,
                s.reserved1,
                s.reserved2,
                s.reserved3,
                s.reserved4,
                s.prefix
            ),
            None => println!("   no sample"),
        }
    }
}
