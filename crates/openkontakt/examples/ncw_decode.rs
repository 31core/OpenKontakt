//! `ncw_decode <file.ncw> [out.wav]` — decode one `.ncw` and report what came out.
//!
//! With an output path the decoded audio is written as a 24-bit WAV, which is the only way to
//! confirm by ear that the container was read correctly.

use openkontakt::ncw::{decode_ncw, parse_ncw_info};
use openkontakt::wav::{SampleCursor, map_ncw};
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: ncw_decode <file.ncw> [out.wav]");
        std::process::exit(2);
    };
    let output = args.next().map(PathBuf::from);

    let bytes = std::fs::read(&input).expect("read");
    let info = parse_ncw_info(&bytes).expect("header");
    let audio = decode_ncw(&bytes).expect("decode");

    let peak = audio.samples.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
    let sum: f64 = audio
        .samples
        .iter()
        .map(|v| (*v as f64) * (*v as f64))
        .sum();
    let rms = (sum / audio.samples.len().max(1) as f64).sqrt();
    let dc: f64 =
        audio.samples.iter().map(|v| *v as f64).sum::<f64>() / audio.samples.len().max(1) as f64;
    let silent = audio.samples.iter().filter(|v| **v == 0.0).count();
    let db = |value: f32| {
        if value > 0.0 {
            20.0 * (value as f64).log10()
        } else {
            f64::NEG_INFINITY
        }
    };

    println!("{input}");
    println!(
        "  {} ch, {} bits, {} Hz, {} frames ({:.3} s), format word {}",
        info.channels,
        info.bits,
        info.sample_rate,
        info.frames,
        info.frames as f64 / info.sample_rate as f64,
        info.format_word
    );
    println!(
        "  decoded {} frames x {} ch = {} values",
        audio.frames,
        audio.channels,
        audio.samples.len()
    );
    println!(
        "  peak {:.2} dBFS, rms {:.2} dBFS, dc {:+.4} dBFS, exactly-zero {:.2}%",
        db(peak),
        db(rms as f32),
        db(dc.abs() as f32),
        100.0 * silent as f64 / audio.samples.len().max(1) as f64
    );
    println!(
        "  compressed {} bytes -> decoded {} bytes ({:.2}x)",
        bytes.len(),
        audio.samples.len() * 4,
        (audio.samples.len() * 4) as f64 / bytes.len() as f64
    );

    // The player streams: it maps the file and decodes one block at a time. This has to agree
    // with the eager decode sample for sample, or a voice would hear something different.
    match map_ncw(std::path::Path::new(&input), 256) {
        Ok(sample) => {
            let mut cursor = SampleCursor::new(Arc::new(sample));
            let mut worst = 0.0f32;
            let mut at = 0usize;
            for frame in 0..audio.frames {
                let (left, right) = cursor.frame(frame);
                for (got, want) in [
                    (left, audio.samples[frame * audio.channels as usize]),
                    (right, audio.samples[frame * audio.channels as usize + 1]),
                ] {
                    if (got - want).abs() > worst {
                        worst = (got - want).abs();
                        at = frame;
                    }
                }
            }
            println!(
                "  streaming vs eager: worst difference {worst:.3e} at frame {at}{}",
                if worst == 0.0 { "" } else { "  <-- MISMATCH" }
            );
            let sample = cursor.sample();
            println!(
                "  mapped {} bytes, resident after preload {} bytes",
                sample.mapped_bytes(),
                sample.resident_bytes
            );
        }
        Err(error) => println!("  streaming FAILED: {error}"),
    }

    if let Some(path) = output {
        let spec = hound::WavSpec {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
        for value in &audio.samples {
            let scaled = (*value as f64 * 8_388_607.0).round() as i32;
            writer.write_sample(scaled).expect("write");
        }
        writer.finalize().expect("finalize");
        println!("  wrote {}", path.display());
    }
}
