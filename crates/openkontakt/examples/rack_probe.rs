//! Dump every effect rack slot: module id, size and private bytes.
use openkontakt::Instrument;
use openkontakt::effects::program_racks;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: rack_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let racks = program_racks(&instrument.program.children);
    println!("program racks: {}", racks.len());
    for (index, path) in instrument.resolved_other.iter().enumerate() {
        println!("other[{index}] = {path:?}");
    }
    for (rack_index, rack) in racks.iter().enumerate() {
        for (slot_index, slot) in rack.slots.iter().enumerate() {
            let Some(slot) = slot else { continue };
            println!(
                "rack {rack_index} slot {slot_index:2} module=0x{:02X} v0x{:02X} bypass={} gain={:.3} dry={:.3} params={} B",
                slot.module,
                slot.module_version,
                slot.bypass,
                slot.output_gain,
                slot.dry_level,
                slot.params.len()
            );
            println!("   decoded: {:?}", openkontakt::effects::decode(slot));
            println!("   params: {}", hex(&slot.params));
            if !slot.module_private.is_empty() {
                println!("   private: {}", hex(&slot.module_private));
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if index % 32 == 0 && index > 0 {
                format!("\n   {byte:02x}")
            } else if index % 4 == 0 {
                format!(" {byte:02x}")
            } else {
                format!("{byte:02x}")
            }
        })
        .collect::<String>()
}
