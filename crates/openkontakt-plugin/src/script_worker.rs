//! Runs the KSP interpreter off the audio thread.
//!
//! The interpreter locks script state, can allocate, and can run thousands of instructions; none of
//! that belongs in a realtime callback. This worker owns the dispatch path instead:
//!
//! ```text
//! audio thread --ScriptCommand--> worker --ScriptResult--> audio thread
//! ```
//!
//! The audio thread pops results once per sample, so a note starts a few samples after the worker
//! answers instead of waiting for the next block. A dropped or late result degrades to the
//! instrument's default routing rather than blocking the callback.

use openkontakt::KspRuntime;
use openkontakt::ksp::{Dispatch, EventContext};
use std::sync::Arc;
use std::sync::mpsc;

#[derive(Clone, Copy, Debug)]
pub enum ScriptCommand {
    NoteOn {
        key: u8,
        velocity: u8,
        id: i64,
    },
    /// `velocity` is the velocity the note was struck with: `$EVENT_VELOCITY` is still readable in
    /// `on release`, and release-trigger scripts pass it straight to `play_note`.
    NoteOff {
        key: u8,
        velocity: u8,
        id: i64,
    },
    Controller {
        number: i64,
        value: i64,
    },
}

pub struct ScriptResult {
    pub command: ScriptCommand,
    pub dispatch: Dispatch,
}

pub struct ScriptWorker {
    commands: rtrb::Producer<ScriptCommand>,
    results: rtrb::Consumer<ScriptResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl ScriptWorker {
    pub fn spawn(runtime: Arc<KspRuntime>) -> Self {
        let (commands_tx, mut commands_rx) = rtrb::RingBuffer::<ScriptCommand>::new(1024);
        let (mut results_tx, results_rx) = rtrb::RingBuffer::<ScriptResult>::new(1024);

        let thread = std::thread::Builder::new()
            .name("openkontakt-ksp".into())
            .spawn(move || {
                loop {
                    let Ok(command) = commands_rx.pop() else {
                        // Idle: yield rather than sleep, so a burst of notes is answered within
                        // microseconds. This thread is cheap when nothing is playing.
                        std::thread::yield_now();
                        continue;
                    };
                    let dispatch = dispatch(&runtime, command);
                    let mut pending = ScriptResult { command, dispatch };
                    // Push until there is room; if the audio side is not draining, drop it.
                    let mut spins = 0;
                    while let Err(rtrb::PushError::Full(returned)) = results_tx.push(pending) {
                        pending = returned;
                        spins += 1;
                        if spins > 100_000 {
                            break;
                        }
                        std::thread::yield_now();
                    }
                }
            })
            .expect("spawn KSP worker");

        Self {
            commands: commands_tx,
            results: results_rx,
            _thread: thread,
        }
    }

    pub fn submit(&mut self, command: ScriptCommand) {
        let _ = self.commands.push(command);
    }

    pub fn poll(&mut self) -> Option<ScriptResult> {
        self.results.pop().ok()
    }
}

fn dispatch(runtime: &KspRuntime, command: ScriptCommand) -> Dispatch {
    match command {
        ScriptCommand::NoteOn { key, velocity, id } => runtime.dispatch(
            "note",
            EventContext {
                note: key as i64,
                velocity: velocity as i64,
                id,
                status: 1,
            },
        ),
        ScriptCommand::NoteOff { key, velocity, id } => runtime.dispatch(
            "release",
            EventContext {
                note: key as i64,
                velocity: velocity as i64,
                id,
                status: 0,
            },
        ),
        ScriptCommand::Controller { number, value } => runtime.controller(number, value),
    }
}

/// Where an impulse response has to be read from.
///
/// A packed library keeps its impulse responses inside the same `.nkr` as its artwork and names
/// them `<container>.nkr/<path inside>`; the `.nkc` catalogue turns that path into an offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrSource {
    File(String),
    Packed {
        container: std::path::PathBuf,
        start: usize,
        size: usize,
    },
}

impl IrSource {
    /// Describe an impulse response from an instrument's `other` reference.
    pub fn from_other(resource: &openkontakt::OtherResource) -> Self {
        match resource {
            openkontakt::OtherResource::File(path) => {
                IrSource::File(path.display().to_string())
            }
            openkontakt::OtherResource::Packed {
                container,
                start,
                size,
            } => IrSource::Packed {
                container: container.clone(),
                start: *start,
                size: *size,
            },
        }
    }
}

/// Loader for impulse responses, also off the audio thread.
///
/// The request carries the rate the engine is running at: an impulse response recorded at a
/// different rate has to be resampled, otherwise its tail is stretched or shortened by the ratio
/// (a 44.1 kHz IR in a 48 kHz engine convolves as if it were ~9 % shorter than it is).
pub type IrRequest = Option<(IrSource, f32)>;
pub type IrResponse = Option<openkontakt::dsp::Convolver>;

pub struct IrLoader {
    request: mpsc::Sender<IrRequest>,
    response: mpsc::Receiver<IrResponse>,
}

impl IrLoader {
    pub fn spawn(block_size: usize) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<IrRequest>();
        let (response_tx, response_rx) = mpsc::channel::<IrResponse>();
        std::thread::Builder::new()
            .name("openkontakt-ir".into())
            .spawn(move || {
                while let Ok(Some((source, sample_rate))) = request_rx.recv() {
                    let convolver = build(&source, block_size, sample_rate);
                    if response_tx.send(convolver).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn IR loader");
        Self {
            request: request_tx,
            response: response_rx,
        }
    }

    pub fn request(&self, source: &IrSource, sample_rate: f32) {
        let _ = self.request.send(Some((source.clone(), sample_rate)));
    }

    pub fn poll(&self) -> Option<IrResponse> {
        self.response.try_recv().ok()
    }
}

pub(crate) fn build(
    source: &IrSource,
    block_size: usize,
    target_rate: f32,
) -> Option<openkontakt::dsp::Convolver> {
    // An impulse response is read once, so it is decoded whole: going through the streaming path
    // would decode its blocks again on every frame.
    let sample = match source {
        IrSource::File(path) => {
            let path = std::path::Path::new(path);
            let is_ncw = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ncw"));
            if is_ncw {
                openkontakt::wav::load_ncw(path, usize::MAX).ok()?
            } else {
                openkontakt::wav::load_wav(path).ok()?
            }
        }
        IrSource::Packed {
            container,
            start,
            size,
        } => openkontakt::wav::load_ncw_range(container, *start, *size).ok()?,
    };
    let frames = sample.frames;
    if frames == 0 {
        return None;
    }
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    for (index, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let (sl, sr) = sample.frame(index);
        *l = sl;
        *r = sr;
    }
    let (left, right) = resample_ir(&left, &right, sample.sample_rate as f32, target_rate);
    let (left, right) = normalise_ir(left, right);
    Some(openkontakt::dsp::Convolver::new(&left, &right, block_size))
}

/// Scale an impulse response to unit energy, so the convolution preserves the level of whatever is
/// sent into it.
///
/// A room IR is a recording: its own level says nothing about how loud the reverb should be, and
/// convolving at the recorded level made the return roughly 10 dB louder than the reference
/// implementation, almost all of it below 500 Hz. Normalising is the convention every convolution
/// reverb follows for exactly this reason.
fn normalise_ir(mut left: Vec<f32>, mut right: Vec<f32>) -> (Vec<f32>, Vec<f32>) {
    let energy: f64 = left
        .iter()
        .chain(right.iter())
        .map(|value| (*value as f64).powi(2))
        .sum::<f64>()
        / 2.0;
    if energy <= 1e-12 {
        return (left, right);
    }
    let scale = (1.0 / energy.sqrt()) as f32;
    for value in left.iter_mut().chain(right.iter_mut()) {
        *value *= scale;
    }
    (left, right)
}

/// Bring an impulse response to the engine's sample rate.
///
/// Linear interpolation is enough here: an impulse response is dense broadband material whose
/// *length* is what a convolution gets wrong when the rate is ignored, and re-timing it correctly
/// removes the error that is audible (a tail 9 % short or long). The interpolation only adds a mild
/// high-frequency roll-off above the source Nyquist.
fn resample_ir(
    left: &[f32],
    right: &[f32],
    source_rate: f32,
    target_rate: f32,
) -> (Vec<f32>, Vec<f32>) {
    if source_rate <= 0.0 || target_rate <= 0.0 || (source_rate - target_rate).abs() < 0.5 {
        return (left.to_vec(), right.to_vec());
    }
    let step = source_rate as f64 / target_rate as f64;
    let frames = (left.len() as f64 / step).ceil() as usize;
    let sample_at = |data: &[f32], index: usize| -> f32 {
        let position = index as f64 * step;
        let whole = position.floor() as usize;
        if whole >= data.len() {
            return 0.0;
        }
        let fraction = (position - whole as f64) as f32;
        let a = data[whole];
        let b = if whole + 1 < data.len() {
            data[whole + 1]
        } else {
            0.0
        };
        a + (b - a) * fraction
    };
    let mut left_out = Vec::with_capacity(frames);
    let mut right_out = Vec::with_capacity(frames);
    for index in 0..frames {
        left_out.push(sample_at(left, index));
        right_out.push(sample_at(right, index));
    }
    (left_out, right_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 44.1 kHz impulse response must arrive at 48 kHz with the same duration. Ignoring the rate
    /// convolves it as if it were 9 % shorter, which is what made the reverb tail wrong.
    #[test]
    fn impulse_responses_are_retimed_to_the_engine_rate() {
        // One second of samples: an impulse at 0.5 s followed by a decaying tail.
        let source_rate = 44_100usize;
        let mut left = vec![0.0f32; source_rate];
        let mut right = vec![0.0f32; source_rate];
        left[source_rate / 2] = 1.0;
        right[source_rate / 2] = 1.0;
        for index in source_rate / 2 + 1..source_rate {
            let decay = 1.0 - (index - source_rate / 2) as f32 / (source_rate / 2) as f32;
            left[index] = decay * 0.5;
            right[index] = decay * 0.5;
        }

        let (resampled, _) = resample_ir(&left, &right, 44_100.0, 48_000.0);
        assert_eq!(resampled.len(), 48_000);
        let peak = resampled
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .map(|(index, _)| index)
            .unwrap();
        assert!(
            peak.abs_diff(24_000) <= 1,
            "the impulse should stay at 0.5 s, landed at sample {peak}"
        );
        assert!(
            resampled[47_999].abs() < 0.05,
            "the tail should reach the end"
        );

        // Matching rates are passed through untouched.
        let (same, _) = resample_ir(&left, &right, 48_000.0, 48_000.0);
        assert_eq!(same.len(), left.len());
        assert_eq!(same[source_rate / 2], 1.0);
    }
    use openkontakt::ksp::GroupInfo;

    fn runtime() -> Arc<KspRuntime> {
        let text = r#"
        on init
          declare $x
        end on

        on note
          disallow_group($ALL_GROUPS)
          allow_group(1)
        end on
        "#;
        let groups = vec![
            GroupInfo {
                release_trigger: false,
                envelope: None,
            },
            GroupInfo {
                release_trigger: false,
                envelope: None,
            },
            GroupInfo {
                release_trigger: false,
                envelope: None,
            },
        ];
        let runtime = Arc::new(KspRuntime::compile_with_groups(
            &[Some(text.to_string())],
            "",
            groups,
        ));
        runtime.initialize();
        runtime
    }

    #[test]
    fn worker_answers_a_note_on_with_the_script_routing() {
        let mut worker = ScriptWorker::spawn(runtime());
        worker.submit(ScriptCommand::NoteOn {
            key: 60,
            velocity: 100,
            id: 7,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut result = None;
        while std::time::Instant::now() < deadline {
            if let Some(value) = worker.poll() {
                result = Some(value);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let result = result.expect("worker answered");
        let mask = result.dispatch.allowed_groups.expect("routing");
        assert_eq!(mask, vec![false, true, false]);
    }
}
