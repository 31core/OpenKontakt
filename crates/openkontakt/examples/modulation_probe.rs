//! Inspect group modulation chunks without interpreting undocumented fields.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: modulation_probe <instrument.nki> [group]");
    let group: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let instrument = Instrument::load(path).unwrap();
    if std::env::args().nth(3).as_deref() == Some("--parsed") {
        for g in &instrument.groups {
            let m = openkontakt::modulation::group_modulation(g);
            println!(
                "group {}: {} named modulators, amp {:?}",
                g.index,
                m.modulators.len(),
                m.amp_slot()
            );
            for d in &m.diagnostics {
                println!("diagnostic: {d}");
            }
            if g.index == group {
                for source in &m.modulators {
                    println!(
                        "  {} {} {:?} targets {:?}",
                        source.handle,
                        source.name,
                        source.source,
                        source
                            .targets
                            .iter()
                            .map(|t| (
                                &t.parameter,
                                t.destination,
                                t.amount,
                                t.inverted,
                                t.curve.as_ref().map(|c| &c.points)
                            ))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
        return;
    }
    for child in &instrument.groups[group].children {
        println!("chunk {:04x}, {} bytes", child.id, child.body.len());
        if matches!(child.id, 0x3B | 0x3C) {
            for (i, row) in child.body.chunks(16).enumerate() {
                println!(
                    "{:04x}: {:<48} {}",
                    i * 16,
                    row.iter().map(|b| format!("{b:02x} ")).collect::<String>(),
                    row.iter()
                        .map(|b| if b.is_ascii_graphic() {
                            *b as char
                        } else {
                            '.'
                        })
                        .collect::<String>()
                );
            }
        }
    }
}
