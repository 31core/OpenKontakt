//! Compare two renders of the same MIDI: where, and by how much, our output differs from Kontakt's.
//!
//! ```text
//! openkontakt-diff --reference kontakt.wav --candidate ours.wav
//! ```
//!
//! The report answers the questions that matter when chasing a reference by ear:
//!
//! * is it late or early (alignment lag),
//! * is it too loud or too quiet (gain match),
//! * how much is left after removing both (null residual), and
//! * *which* part of the spectrum carries the difference (per-octave residual).

use rustfft::{FftPlanner, num_complex::Complex32};
use std::path::PathBuf;

struct Signal {
    left: Vec<f32>,
    right: Vec<f32>,
    sample_rate: f64,
}

impl Signal {
    fn load(path: &PathBuf) -> Result<Self, String> {
        let sample = openkontakt::wav::map_wav(path, 0)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mut left = Vec::with_capacity(sample.frames);
        let mut right = Vec::with_capacity(sample.frames);
        for index in 0..sample.frames {
            let (l, r) = sample.frame(index);
            left.push(l);
            right.push(r);
        }
        Ok(Self {
            left,
            right,
            sample_rate: sample.sample_rate as f64,
        })
    }

    fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    fn mono(&self) -> Vec<f32> {
        (0..self.frames())
            .map(|index| 0.5 * (self.left[index] + self.right[index]))
            .collect()
    }

    fn rms(&self) -> f64 {
        let frames = self.frames();
        if frames == 0 {
            return 0.0;
        }
        let mut sum = 0.0f64;
        for index in 0..frames {
            sum += (self.left[index] as f64).powi(2) + (self.right[index] as f64).powi(2);
        }
        (sum / (frames as f64 * 2.0)).sqrt()
    }

    fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(self.right.iter())
            .fold(0.0f32, |peak, value| peak.max(value.abs()))
    }
}

fn db(value: f64) -> String {
    if value <= 1e-12 {
        "-inf".to_string()
    } else {
        format!("{:+.1}", 20.0 * value.log10())
    }
}

/// Lag, in samples, that best lines `candidate` up with `reference` (positive: candidate is late).
///
/// Two stages, because neither alone works on musical material:
///
/// * **onset.** Raw cross-correlation cannot locate a sustained tone: a piano note correlates about
///   as well with itself shifted by 300 samples as it does at the true offset, and its envelope is
///   flat enough over the first second to give the same ambiguity. The first sample that crosses a
///   fraction of the peak is unambiguous.
/// * **refinement.** The onset is only accurate to the shape of the attack, and our attack is not
///   Kontakt's, so the exact offset is found by maximising the normalised correlation within a few
///   milliseconds of it.
fn alignment(
    reference: &[f32],
    candidate: &[f32],
    search: usize,
    sample_rate: f64,
    refine: bool,
) -> isize {
    let length = reference.len().min(candidate.len());
    if length == 0 {
        return 0;
    }
    // Onset on a short RMS envelope, relative to the level of the first second rather than to the
    // peak: a peak-relative threshold makes the answer depend on how loud the file happens to be,
    // and two renders of the same music can differ by a decibel or so.
    let onset = |data: &[f32]| -> usize {
        let window = length.min((1.0 * sample_rate) as usize);
        if window == 0 {
            return 0;
        }
        let reference_level = (data[..window]
            .iter()
            .map(|value| (*value as f64).powi(2))
            .sum::<f64>()
            / window as f64)
            .sqrt() as f32;
        let threshold = (reference_level * 0.5).max(1e-5);
        let width = ((0.001 * sample_rate) as usize).max(1);
        let mut sum = 0.0f64;
        for index in 0..length {
            sum += data[index].abs() as f64;
            if index >= width {
                sum -= data[index - width].abs() as f64;
            }
            let mean = (sum / width.min(index + 1) as f64) as f32;
            if mean >= threshold {
                return index;
            }
        }
        0
    };
    // Positive lag means the candidate's content sits later in time than the reference's.
    let coarse = onset(candidate) as isize - onset(reference) as isize;
    let coarse = coarse.clamp(-(search as isize), search as isize);

    if !refine {
        // The candidate was resampled to the reference rate. Waveform correlation is no longer a
        // reliable tie-breaker there (the interpolation moves every sample a little), so the
        // onset — which is what the alignment means for this comparison — stands.
        return coarse;
    }
    let window = (0.010 * sample_rate) as isize;
    let normalised = |lag: isize| -> f64 {
        let start = (search as isize + window).max(0) as usize;
        let end = length.saturating_sub(search + window as usize);
        let mut dot = 0.0f64;
        let mut energy = 0.0f64;
        let mut index = start;
        while index < end {
            let other = index as isize + lag;
            if other >= 0 && (other as usize) < length {
                dot += reference[index] as f64 * candidate[other as usize] as f64;
                energy += (candidate[other as usize] as f64).powi(2);
            }
            index += 1;
        }
        if energy > 0.0 {
            dot / energy.sqrt()
        } else {
            0.0
        }
    };
    let mut best = (f64::NEG_INFINITY, coarse);
    let mut lag = coarse - window;
    while lag <= coarse + window {
        let value = normalised(lag);
        if value > best.0 {
            best = (value, lag);
        }
        lag += 1;
    }
    best.1
}

/// Magnitude spectrum averaged over Hann-windowed segments (Welch).
fn average_spectrum(samples: &[f32], size: usize) -> Vec<f64> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(size);
    let window: Vec<f32> = (0..size)
        .map(|index| 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / size as f32).cos())
        .collect();
    let mut accumulated = vec![0.0f64; size / 2];
    let mut segments = 0usize;
    let hop = size / 2;
    let mut start = 0usize;
    while start + size <= samples.len() {
        let mut buffer: Vec<Complex32> = (0..size)
            .map(|index| Complex32::new(samples[start + index] * window[index], 0.0))
            .collect();
        fft.process(&mut buffer);
        for (index, bin) in buffer.iter().take(size / 2).enumerate() {
            accumulated[index] += bin.norm_sqr() as f64;
        }
        segments += 1;
        start += hop;
    }
    if segments > 1 {
        for value in &mut accumulated {
            *value /= segments as f64;
        }
    }
    accumulated
}

const BANDS: [(&str, f64, f64); 10] = [
    ("  31 Hz", 22.0, 44.0),
    ("  63 Hz", 44.0, 88.0),
    (" 125 Hz", 88.0, 177.0),
    (" 250 Hz", 177.0, 354.0),
    (" 500 Hz", 354.0, 707.0),
    ("   1 kHz", 707.0, 1414.0),
    ("   2 kHz", 1414.0, 2828.0),
    ("   4 kHz", 2828.0, 5657.0),
    ("   8 kHz", 5657.0, 11314.0),
    ("  16 kHz", 11314.0, 22627.0),
];

fn band_levels(spectrum: &[f64], sample_rate: f64, size: usize) -> Vec<f64> {
    let bin_hz = sample_rate / size as f64;
    BANDS
        .iter()
        .map(|(_, low, high)| {
            let from = ((low / bin_hz) as usize).min(spectrum.len());
            let to = ((high / bin_hz) as usize).min(spectrum.len());
            spectrum[from..to].iter().sum::<f64>()
        })
        .collect()
}

fn main() {
    let mut reference: Option<PathBuf> = None;
    let mut candidate: Option<PathBuf> = None;
    let mut max_seconds: Option<f64> = None;
    let mut search_ms: f64 = 500.0;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--reference" => reference = args.next().map(PathBuf::from),
            "--candidate" => candidate = args.next().map(PathBuf::from),
            "--seconds" => max_seconds = args.next().and_then(|value| value.parse().ok()),
            "--search-ms" => {
                search_ms = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(500.0)
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let (Some(reference_path), Some(candidate_path)) = (reference, candidate) else {
        eprintln!(
            "usage: openkontakt-diff --reference <ref.wav> --candidate <ours.wav>\n\
             \x20                        [--seconds N] [--search-ms 500]"
        );
        std::process::exit(2);
    };

    let reference_signal = match Signal::load(&reference_path) {
        Ok(signal) => signal,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let candidate_signal = match Signal::load(&candidate_path) {
        Ok(signal) => signal,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let rates_equal = (reference_signal.sample_rate - candidate_signal.sample_rate).abs() <= 0.5;
    let mut candidate_signal = candidate_signal;
    if !rates_equal {
        // Bring the candidate to the reference rate so a 44.1 kHz export can be compared with a
        // 48 kHz render. Linear interpolation is honest enough: the residual it adds is around
        // -60 dB, well under the differences this tool is used to measure.
        eprintln!(
            "note: resampling the candidate from {:.0} Hz to {:.0} Hz",
            candidate_signal.sample_rate, reference_signal.sample_rate
        );
        let source = candidate_signal.sample_rate;
        let target = reference_signal.sample_rate;
        let step = source / target;
        let resample = |data: &[f32]| -> Vec<f32> {
            let frames = (data.len() as f64 / step).ceil() as usize;
            (0..frames)
                .map(|index| {
                    let position = index as f64 * step;
                    let whole = position.floor() as usize;
                    if whole + 1 >= data.len() {
                        return *data.last().unwrap_or(&0.0);
                    }
                    let fraction = (position - whole as f64) as f32;
                    data[whole] + (data[whole + 1] - data[whole]) * fraction
                })
                .collect()
        };
        candidate_signal = Signal {
            left: resample(&candidate_signal.left),
            right: resample(&candidate_signal.right),
            sample_rate: target,
        };
    }

    let frames = reference_signal.frames().min(candidate_signal.frames());
    let frames = match max_seconds {
        Some(seconds) => frames.min((seconds * reference_signal.sample_rate) as usize),
        None => frames,
    };
    println!(
        "reference  {}  {} frames ({:.2} s)  peak {:.4}  rms {} dBFS",
        reference_path.display(),
        reference_signal.frames(),
        reference_signal.frames() as f64 / reference_signal.sample_rate,
        reference_signal.peak(),
        db(reference_signal.rms())
    );
    println!(
        "candidate  {}  {} frames ({:.2} s)  peak {:.4}  rms {} dBFS",
        candidate_path.display(),
        candidate_signal.frames(),
        candidate_signal.frames() as f64 / candidate_signal.sample_rate,
        candidate_signal.peak(),
        db(candidate_signal.rms())
    );

    let reference_mono = reference_signal.mono();
    let candidate_mono = candidate_signal.mono();
    let search = ((search_ms / 1000.0 * reference_signal.sample_rate) as usize)
        .min(frames / 2)
        .max(1);
    let lag = alignment(
        &reference_mono[..frames],
        &candidate_mono[..frames],
        search,
        reference_signal.sample_rate,
        rates_equal,
    );
    println!(
        "alignment  {lag:+} samples ({:+.2} ms)",
        1000.0 * lag as f64 / reference_signal.sample_rate
    );

    // Best scalar gain, then the residual that survives it.
    let mut dot = 0.0f64;
    let mut candidate_energy = 0.0f64;
    for index in 0..frames {
        let other = index as isize + lag;
        if other < 0 || other as usize >= candidate_signal.frames() {
            continue;
        }
        let other = other as usize;
        for (a, b) in [
            (reference_signal.left[index], candidate_signal.left[other]),
            (reference_signal.right[index], candidate_signal.right[other]),
        ] {
            dot += a as f64 * b as f64;
            candidate_energy += (b as f64).powi(2);
        }
    }
    let gain = if candidate_energy > 0.0 {
        dot / candidate_energy
    } else {
        1.0
    };
    println!(
        "gain match {:+.2} dB (candidate × {:.4})",
        20.0 * gain.abs().max(1e-12).log10(),
        gain
    );

    let mut reference_len = 0usize;
    let mut residual_energy = 0.0f64;
    let mut reference_slice = Vec::with_capacity(frames);
    let mut candidate_slice = Vec::with_capacity(frames);
    for index in 0..frames {
        let other = index as isize + lag;
        if other < 0 || other as usize >= candidate_signal.frames() {
            continue;
        }
        let other = other as usize;
        let a = 0.5 * (reference_signal.left[index] + reference_signal.right[index]);
        let b = 0.5 * (candidate_signal.left[other] + candidate_signal.right[other]);
        let scaled = b as f64 * gain;
        residual_energy += (scaled - a as f64).powi(2);
        reference_len += 1;
        reference_slice.push(a);
        candidate_slice.push(scaled as f32);
    }
    let residual_rms = if reference_len > 0 {
        (residual_energy / reference_len as f64).sqrt()
    } else {
        0.0
    };
    let reference_rms_mono = {
        let sum: f64 = reference_slice
            .iter()
            .map(|value| (*value as f64).powi(2))
            .sum();
        (sum / reference_len.max(1) as f64).sqrt()
    };
    println!(
        "null residual {} dB below the reference",
        if residual_rms <= 1e-12 {
            "-inf".to_string()
        } else {
            format!(
                "{:.1}",
                20.0 * (residual_rms / reference_rms_mono.max(1e-12)).log10()
            )
        }
    );

    let size = 8192usize;
    let reference_spectrum = average_spectrum(&reference_slice, size);
    let candidate_spectrum = average_spectrum(&candidate_slice, size);
    let reference_bands = band_levels(&reference_spectrum, reference_signal.sample_rate, size);
    let candidate_bands = band_levels(&candidate_spectrum, reference_signal.sample_rate, size);
    println!("per octave band (candidate − reference):");
    for (index, (name, _, _)) in BANDS.iter().enumerate() {
        let a = reference_bands[index];
        let b = candidate_bands[index];
        let difference = if a > 1e-20 && b > 1e-20 {
            format!("{:+.1} dB", 10.0 * (b / a).log10())
        } else if b > a {
            "only in candidate".to_string()
        } else if a > b * 1e6 {
            "missing".to_string()
        } else {
            "-inf dB".to_string()
        };
        println!("  {name}  {difference:>16}");
    }
}
