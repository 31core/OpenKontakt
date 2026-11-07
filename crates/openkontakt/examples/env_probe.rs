//! Print each group's amplitude envelope as stored in the file.
use openkontakt::Instrument;
use openkontakt::envelope::group_amp_envelope;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: env_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    for (index, group) in instrument.groups.iter().enumerate() {
        match group_amp_envelope(group) {
            Some(e) => println!(
                "group {index:2} {:<28} attack={:>8.2} hold={:>7.2} decay={:>8.2} sustain={:>6.3} release={:>8.2} ms",
                group.name, e.attack_ms, e.hold_ms, e.decay_ms, e.sustain, e.release_ms
            ),
            None => println!("group {index:2} {:<28} (no envelope)", group.name),
        }
    }
}
