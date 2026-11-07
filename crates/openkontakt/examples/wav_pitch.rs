//! Decode a WAV through our own reader and report its fundamental.
use openkontakt::wav::map_wav;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: wav_pitch <file.wav>");
    let mapping = map_wav(std::path::Path::new(&path), 0).expect("wav");
    println!(
        "sample_rate={} channels={} frames={}",
        mapping.sample_rate, mapping.channels, mapping.frames
    );
    let mut block = Vec::new();
    for frame in 0..48_000usize.min(mapping.frames) {
        block.push(mapping.frame(frame).0);
    }
    let start = block.len() / 8;
    let window = &block[start..];
    let mut best = (0.0f64, 0.0f64);
    let mut frequency = 50.0f64;
    while frequency < 400.0 {
        let step = std::f64::consts::TAU * frequency / mapping.sample_rate as f64;
        let (mut real, mut imaginary) = (0.0f64, 0.0f64);
        for (index, value) in window.iter().enumerate() {
            real += *value as f64 * (step * index as f64).cos();
            imaginary += *value as f64 * (step * index as f64).sin();
        }
        let magnitude = (real * real + imaginary * imaginary).sqrt();
        if magnitude > best.0 {
            best = (magnitude, frequency);
        }
        frequency += 1.0;
    }
    println!("fundamental ~{:.0} Hz", best.1);
}
