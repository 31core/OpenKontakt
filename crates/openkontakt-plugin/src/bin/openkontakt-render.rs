//! Render an instrument and a MIDI file to a WAV, offline.
//!
//! ```text
//! openkontakt-render --instrument Library.nki --midi take.mid --out ours.wav
//! ```
//!
//! The reference side is rendered once from Kontakt in any host; after that this is the only
//! command in the loop.

use openkontakt_plugin::offline::{RenderOptions, render, write_wav};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "usage: openkontakt-render --instrument <file.nki> --midi <file.mid> --out <file.wav>\n\
         \x20                        [--rate 48000] [--tail 3.0] [--gain 1.0]"
    );
    std::process::exit(2)
}

fn main() {
    let mut instrument: Option<PathBuf> = None;
    let mut midi: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut options = RenderOptions::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--instrument" => instrument = Some(PathBuf::from(value())),
            "--midi" => midi = Some(PathBuf::from(value())),
            "--out" => out = Some(PathBuf::from(value())),
            "--rate" => options.sample_rate = value().parse().unwrap_or_else(|_| usage()),
            "--tail" => options.tail_seconds = value().parse().unwrap_or_else(|_| usage()),
            "--gain" => options.gain = value().parse().unwrap_or_else(|_| usage()),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument {other}");
                usage()
            }
        }
    }
    let (Some(instrument), Some(midi), Some(out)) = (instrument, midi, out) else {
        usage()
    };

    let started = std::time::Instant::now();
    let render = match render(&instrument, &midi, &options) {
        Ok(render) => render,
        Err(error) => {
            eprintln!("render failed: {error}");
            std::process::exit(1);
        }
    };
    for diagnostic in &render.diagnostics {
        eprintln!("  {diagnostic}");
    }
    if let Err(error) = write_wav(&out, &render) {
        eprintln!("{}: {error}", out.display());
        std::process::exit(1);
    }
    println!(
        "{}: {} frames at {} Hz ({:.2} s), peak {:.4}, rendered in {:.2} s",
        out.display(),
        render.frames(),
        render.sample_rate,
        render.frames() as f32 / render.sample_rate,
        render.peak(),
        started.elapsed().as_secs_f32()
    );
    if render.peak() <= 0.0 {
        eprintln!("warning: the render is silent");
        std::process::exit(3);
    }
}
