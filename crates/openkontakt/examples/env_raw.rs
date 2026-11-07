//! Dump every amplitude-envelope record inside each group's modulator array.
use openkontakt::Instrument;
use openkontakt::envelope::parse_ahdsr;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: env_raw <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    for (index, group) in instrument.groups.iter().enumerate() {
        let Some(array) = group.children.iter().find(|c| c.id == 0x3B) else {
            println!("group {index:2} {:<24} no 0x3B", group.name);
            continue;
        };
        let body = &array.body;
        println!("group {index:2} {:<24} 0x3B len={}", group.name, body.len());
        let mut found = 0;
        for at in 0..body.len().saturating_sub(3 + 25) {
            let slice = &body[at..];
            if slice[0] != 0 || !matches!(slice[1], 0x10 | 0x11) || slice[2] != 0 {
                continue;
            }
            let Some(envelope) = parse_ahdsr(slice) else {
                continue;
            };
            if envelope.attack_ms > 100_000.0 || envelope.release_ms > 100_000.0 {
                continue;
            }
            println!(
                "  @{at:04x} v{:02x} attack={:>8.2} decay={:>8.2} hold={:>6.2} release={:>8.2} sustain={:>5.2}",
                slice[1],
                envelope.attack_ms,
                envelope.decay_ms,
                envelope.hold_ms,
                envelope.release_ms,
                envelope.sustain
            );
            let tail_end = (at + 0x1C + 64).min(body.len());
            println!("      {}", hex(&body[at..tail_end]));
            found += 1;
            if found > 4 {
                break;
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
