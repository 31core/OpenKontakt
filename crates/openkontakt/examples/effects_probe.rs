//! Dump the effect racks of an instrument.
//!
//! ```text
//! cargo run -p openkontakt --example effects_probe -- path/to/instrument.nki
//! ```

use openkontakt::Instrument;
use openkontakt::effects::decode;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: effects_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let racks = openkontakt::effects::program_racks(&instrument.program.children);
    println!("{} program racks", racks.len());
    for (index, rack) in racks.iter().enumerate() {
        println!("rack {index}:");
        for (slot_index, slot) in rack.slots.iter().enumerate() {
            let Some(slot) = slot else { continue };
            println!(
                "  slot {slot_index}: module 0x{:02X} v0x{:02X} bypass={} out={:.3} dry={:.3} -> {:?}",
                slot.module,
                slot.module_version,
                slot.bypass,
                slot.output_gain,
                slot.dry_level,
                decode(slot)
            );
        }
    }
    for group in instrument.groups.iter().take(2) {
        println!(
            "group {} child ids: {:?}",
            group.index,
            group
                .children
                .iter()
                .map(|c| format!("0x{:02X}:{}", c.id, c.body.len()))
                .collect::<Vec<_>>()
        );
    }
    for group in instrument.groups.iter().take(4) {
        match openkontakt::envelope::group_amp_envelope(group) {
            Some(envelope) => println!(
                "group {} envelope: A {:.1} ms  D {:.1} ms  H {:.1} ms  R {:.1} ms  S {:.3}  curve {:.2}",
                group.index,
                envelope.attack_ms,
                envelope.decay_ms,
                envelope.hold_ms,
                envelope.release_ms,
                envelope.sustain,
                envelope.attack_curve
            ),
            None => println!("group {}: no AHDSR found", group.index),
        }
    }
    for group in instrument.groups.iter().take(2) {
        match openkontakt::effects::group_rack(&group.private) {
            Some(rack) => {
                println!("group {} rack:", group.index);
                for (slot_index, slot) in rack.slots.iter().enumerate() {
                    let Some(slot) = slot else { continue };
                    println!(
                        "  slot {slot_index}: module 0x{:02X} bypass={} out={:.3} -> {:?}",
                        slot.module,
                        slot.bypass,
                        slot.output_gain,
                        decode(slot)
                    );
                }
            }
            None => println!("group {}: no rack", group.index),
        }
    }
}
