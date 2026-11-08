//! Raw sub-chunks of each group, to find the start-criteria record.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    for group in &instrument.groups {
        println!("group {} {:<16} children:", group.index, group.name);
        for child in &group.children {
            let preview: Vec<String> = child
                .body
                .iter()
                .take(12)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if child.id == 0x38 {
                let full: Vec<String> = child.body.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "   id 0x{:02X}  {:>4} B  full [{}]",
                    child.id,
                    child.body.len(),
                    full.join(" ")
                );
            } else {
                println!(
                    "   id 0x{:02X}  {:>4} B  [{}]",
                    child.id,
                    child.body.len(),
                    preview.join(" ")
                );
            }
        }
    }
}
