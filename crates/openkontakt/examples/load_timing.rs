//! Where the time goes when an instrument is loaded.
use openkontakt::{Instrument, load_samples};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let started = Instant::now();
    let instrument = Instrument::load(&path).unwrap();
    println!(
        "parse        {:>8.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );

    let stage = Instant::now();
    let loaded = load_samples(&instrument);
    println!(
        "load samples {:>8.1} ms  ({} files, {} B resident)",
        stage.elapsed().as_secs_f64() * 1000.0,
        loaded.samples.len(),
        loaded.bytes_loaded
    );

    let archive_started = Instant::now();
    let archive = instrument.resource_archive();
    println!(
        "archive      {:>8.1} ms  ({} entries)",
        archive_started.elapsed().as_secs_f64() * 1000.0,
        archive.as_ref().map_or(0, |archive| archive.entries.len())
    );

    let stage = Instant::now();
    let assets = instrument.picture_assets();
    let bytes: usize = assets.iter().map(|asset| asset.image.len()).sum();
    println!(
        "pictures     {:>8.1} ms  ({} assets, {} MB of PNG)",
        stage.elapsed().as_secs_f64() * 1000.0,
        assets.len(),
        bytes / 1_000_000
    );
}
