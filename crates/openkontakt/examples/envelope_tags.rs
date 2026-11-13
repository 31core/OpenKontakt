//! Dump the modulator array of a group, with ASCII tags, to see which envelope targets volume.
use openkontakt::Instrument;

fn ascii(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut run = String::new();
    for &b in bytes {
        if (0x20..0x7f).contains(&b) {
            run.push(b as char);
        } else {
            if run.len() >= 4 {
                out.push(run.clone());
            }
            run.clear();
        }
    }
    if run.len() >= 4 {
        out.push(run);
    }
    out
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let index: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let instrument = Instrument::load(&path).unwrap();
    let group = &instrument.groups[index];
    println!("group {index} {}", group.name);
    let array = group.children.iter().find(|c| c.id == 0x3B).unwrap();
    let body = &array.body;
    println!("0x3B len {}", body.len());
    println!("ascii: {:?}", ascii(body));
    // Print presence chunk list at offset 0.
    let mut at = 0;
    while at + 7 <= body.len() {
        if body[at] != 1 {
            println!("stop at {at}: {:02x}", body[at]);
            break;
        }
        let id = u16::from_le_bytes([body[at + 1], body[at + 2]]);
        let len =
            u32::from_le_bytes([body[at + 3], body[at + 4], body[at + 5], body[at + 6]]) as usize;
        println!("chunk at {at}: id 0x{id:02X} len {len}");
        let payload = &body[at + 7..(at + 7 + len).min(body.len())];
        println!("  ascii: {:?}", ascii(payload));
        println!(
            "  hex: {}",
            payload
                .iter()
                .take(160)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        at += 7 + len;
    }
}
