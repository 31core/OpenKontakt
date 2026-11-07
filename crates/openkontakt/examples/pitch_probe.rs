//! Compare what the instrument says about pitch with the samples it points at.
use openkontakt::Instrument;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: pitch_probe <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let params = &instrument.program.params;
    println!(
        "program: transpose={} tune={} volume={} keys={}..{}",
        params.transpose, params.tune, params.volume, params.low_key, params.high_key
    );
    for (index, group) in instrument.groups.iter().enumerate() {
        println!(
            "group {index} {:<24} tune={:+.4} volume={:+.4} pan={:+.4} key_tracking={}",
            group.name, group.tune, group.volume, group.pan, group.key_tracking
        );
    }
}
