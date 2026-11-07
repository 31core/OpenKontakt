//! Uniform partitioned overlap-save convolution for stereo impulse responses.
//!
//! [`Convolver`] splits the IR into `block_size`-tap partitions and keeps one Hamming-window-free
//! (rectangular) FFT spectrum per partition and per channel. Each call to
//! [`Convolver::process`] consumes exactly one block for each channel, performs one forward FFT
//! and one inverse FFT, and adds the result into the output slices.
//!
//! The internal FFT size is `2 * block_size`, which is the smallest power-of-two-free size that
//! makes the overlap-save valid region contain a complete block for a `block_size`-tap partition.
//! All FFT plans, spectra and scratch buffers are allocated in [`Convolver::new`]; `process` does
//! not allocate.
//!
//! Empty IRs fall back to a dry pass-through (the input block is added to the output), and a
//! zero `block_size` is treated the same way.
#![allow(dead_code)]

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

fn zero_complex(len: usize) -> Vec<Complex<f32>> {
    vec![Complex::new(0.0, 0.0); len]
}

/// One-channel uniform partitioned overlap-save convolver.
struct ChannelConvolver {
    block_size: usize,
    fft_size: usize,
    partitions: usize,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    forward_scratch: Vec<Complex<f32>>,
    inverse_scratch: Vec<Complex<f32>>,
    /// `partitions * fft_size` spectra, one per IR partition.
    ir_spectra: Vec<Complex<f32>>,
    /// Ring of the last `partitions` input spectra.
    history: Vec<Complex<f32>>,
    /// Scratch for the current input window, laid out as `[previous block, current block]`.
    time_buffer: Vec<Complex<f32>>,
    /// Frequency-domain accumulation buffer.
    accum: Vec<Complex<f32>>,
    /// Previous input block, needed to form the overlap-save window.
    previous_input: Vec<f32>,
    /// Ring slot that receives the current input spectrum.
    write_index: usize,
}

impl ChannelConvolver {
    /// Builds a convolver for `ir`, treating it as zero-padded to `padded_len` taps.
    fn new(ir: &[f32], padded_len: usize, block_size: usize) -> Option<Self> {
        if ir.is_empty() || block_size == 0 {
            return None;
        }
        let padded_len = padded_len.max(ir.len());
        let fft_size = block_size.checked_mul(2)?;
        let partitions = padded_len.div_ceil(block_size);

        let mut planner = FftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(fft_size);
        let inverse = planner.plan_fft_inverse(fft_size);
        let mut forward_scratch = zero_complex(forward.get_inplace_scratch_len());
        let inverse_scratch = zero_complex(inverse.get_inplace_scratch_len());

        let mut ir_spectra = zero_complex(partitions.checked_mul(fft_size)?);
        let mut work = zero_complex(fft_size);
        for partition in 0..partitions {
            work.fill(Complex::new(0.0, 0.0));
            let base = partition * block_size;
            for tap in 0..block_size {
                let index = base + tap;
                if index < ir.len() {
                    work[tap] = Complex::new(ir[index], 0.0);
                }
            }
            forward.process_with_scratch(&mut work, &mut forward_scratch);
            let start = partition * fft_size;
            ir_spectra[start..start + fft_size].copy_from_slice(&work);
        }

        Some(Self {
            block_size,
            fft_size,
            partitions,
            forward,
            inverse,
            forward_scratch,
            inverse_scratch,
            ir_spectra,
            history: zero_complex(partitions * fft_size),
            time_buffer: zero_complex(fft_size),
            accum: zero_complex(fft_size),
            previous_input: vec![0.0; block_size],
            write_index: 0,
        })
    }

    /// Convolves one block, adding the wet signal into `output`.
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.block_size);
        debug_assert_eq!(output.len(), self.block_size);

        // Overlap-save window: [previous block, current block].
        for (index, sample) in self.previous_input.iter().enumerate() {
            self.time_buffer[index] = Complex::new(*sample, 0.0);
        }
        for (index, sample) in input.iter().enumerate() {
            self.time_buffer[self.block_size + index] = Complex::new(*sample, 0.0);
        }
        self.forward
            .process_with_scratch(&mut self.time_buffer, &mut self.forward_scratch);

        // Store X_m in the ring, then accumulate sum_p X_(m-p) * H_p.
        let slot = self.write_index;
        let slot_start = slot * self.fft_size;
        self.history[slot_start..slot_start + self.fft_size].copy_from_slice(&self.time_buffer);
        self.accum.fill(Complex::new(0.0, 0.0));
        for partition in 0..self.partitions {
            let history_slot = (slot + self.partitions - partition) % self.partitions;
            let history_start = history_slot * self.fft_size;
            let ir_start = partition * self.fft_size;
            for bin in 0..self.fft_size {
                let x = self.history[history_start + bin];
                let h = self.ir_spectra[ir_start + bin];
                let product = x * h;
                self.accum[bin].re += product.re;
                self.accum[bin].im += product.im;
            }
        }

        self.inverse
            .process_with_scratch(&mut self.accum, &mut self.inverse_scratch);
        let scale = 1.0 / self.fft_size as f32;
        for index in 0..self.block_size {
            output[index] += self.accum[self.block_size + index].re * scale;
        }

        self.previous_input.copy_from_slice(input);
        self.write_index = (slot + 1) % self.partitions;
    }
}

/// Stereo uniform partitioned convolution with an impulse response.
///
/// The left and right channels are processed by independent internal convolvers. If the two IRs
/// differ in length, the shorter one is treated as zero-padded to the longer length. An empty IR
/// (on either side) disables convolution for that channel: its input is added directly to the
/// output instead.
///
/// Construction allocates and plans FFTs; [`process`](Convolver::process) is allocation-free.
pub struct Convolver {
    block_size: usize,
    left: Option<ChannelConvolver>,
    right: Option<ChannelConvolver>,
}

impl Convolver {
    /// Creates a stereo convolver from a left and a right impulse response.
    ///
    /// `block_size` is the number of frames consumed per [`process`](Convolver::process) call
    /// (typically 512 or 1024). A `block_size` of zero or empty IRs produce a dry pass-through.
    pub fn new(ir_left: &[f32], ir_right: &[f32], block_size: usize) -> Self {
        if block_size == 0 {
            return Self {
                block_size,
                left: None,
                right: None,
            };
        }
        let padded_len = ir_left.len().max(ir_right.len());
        Self {
            block_size,
            left: ChannelConvolver::new(ir_left, padded_len, block_size),
            right: ChannelConvolver::new(ir_right, padded_len, block_size),
        }
    }

    /// The number of frames consumed by one [`process`](Convolver::process) call.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Convolves one block and adds the result into `left_out` and `right_out`.
    ///
    /// The four slices must all be `block_size` frames long. As a defensive measure, if the
    /// common length is not `block_size` the call falls back to a dry pass-through for that
    /// common length instead of panicking. The output slices are accumulated into, not
    /// overwritten.
    pub fn process(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        let frames = left_in
            .len()
            .min(right_in.len())
            .min(left_out.len())
            .min(right_out.len());
        if frames == 0 {
            return;
        }
        if self.block_size == 0 || frames != self.block_size {
            for index in 0..frames {
                left_out[index] += left_in[index];
                right_out[index] += right_in[index];
            }
            return;
        }

        let block = self.block_size;
        match self.left.as_mut() {
            Some(convolver) => convolver.process(&left_in[..block], &mut left_out[..block]),
            None => {
                for index in 0..block {
                    left_out[index] += left_in[index];
                }
            }
        }
        match self.right.as_mut() {
            Some(convolver) => convolver.process(&right_in[..block], &mut right_out[..block]),
            None => {
                for index in 0..block {
                    right_out[index] += right_in[index];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends a unit impulse at `impulse_block`/`impulse_offset` and returns the stereo output.
    fn run_impulse(
        ir_left: &[f32],
        ir_right: &[f32],
        block_size: usize,
        impulse_block: usize,
        impulse_offset: usize,
        blocks: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut convolver = Convolver::new(ir_left, ir_right, block_size);
        let mut left_out = vec![0.0; blocks * block_size];
        let mut right_out = vec![0.0; blocks * block_size];
        let mut left_in = vec![0.0; block_size];
        let mut right_in = vec![0.0; block_size];

        for block in 0..blocks {
            if block == impulse_block {
                left_in[impulse_offset] = 1.0;
                right_in[impulse_offset] = 1.0;
            }
            let mut left_block = vec![0.0; block_size];
            let mut right_block = vec![0.0; block_size];
            convolver.process(&left_in, &right_in, &mut left_block, &mut right_block);
            left_out[block * block_size..(block + 1) * block_size].copy_from_slice(&left_block);
            right_out[block * block_size..(block + 1) * block_size].copy_from_slice(&right_block);
            left_in.fill(0.0);
            right_in.fill(0.0);
        }

        (left_out, right_out)
    }

    fn make_ir(length: usize) -> Vec<f32> {
        (0..length)
            .map(|index| ((index as f32) * 0.37).sin() * 0.5)
            .collect()
    }

    fn assert_matches_ir(output: &[f32], ir: &[f32]) {
        for (index, expected) in ir.iter().enumerate() {
            assert!(
                (output[index] - expected).abs() < 1e-4,
                "tap {index}: expected {expected}, got {}",
                output[index]
            );
        }
        for (index, sample) in output.iter().enumerate().skip(ir.len()) {
            assert!(
                sample.abs() < 1e-4,
                "tail tap {index} should be silent, got {sample}"
            );
        }
    }

    #[test]
    fn impulse_reproduces_ir() {
        let block_size = 64;
        let ir = make_ir(300);
        let blocks = ir.len().div_ceil(block_size) + 2;
        let (left, right) = run_impulse(&ir, &ir, block_size, 0, 0, blocks);

        assert_matches_ir(&left, &ir);
        assert_matches_ir(&right, &ir);
    }

    #[test]
    fn impulse_reproduces_short_and_exact_multiple_irs() {
        let block_size = 64;
        for &ir_len in &[1usize, 17, 63, 64, 65, 192] {
            let ir = make_ir(ir_len);
            let blocks = ir_len.div_ceil(block_size) + 2;
            let (left, _) = run_impulse(&ir, &ir, block_size, 0, 0, blocks);
            assert_matches_ir(&left, &ir);
        }
    }

    #[test]
    fn shorter_ir_is_zero_padded_when_stereo_lengths_differ() {
        let block_size = 64;
        let left_ir = make_ir(37);
        let right_ir = make_ir(100);
        let blocks = right_ir.len().div_ceil(block_size) + 2;
        let (left, right) = run_impulse(&left_ir, &right_ir, block_size, 0, 0, blocks);

        assert_matches_ir(&left, &left_ir);
        assert_matches_ir(&right, &right_ir);
    }

    #[test]
    fn silence_produces_silence_and_output_is_accumulated() {
        let block_size = 32;
        let ir = make_ir(80);
        let mut convolver = Convolver::new(&ir, &ir, block_size);
        let silence = vec![0.0; block_size];
        let mut left = vec![0.0; block_size];
        let mut right = vec![0.0; block_size];

        for _ in 0..16 {
            convolver.process(&silence, &silence, &mut left, &mut right);
        }
        assert!(left.iter().all(|sample| sample.abs() < 1e-6));
        assert!(right.iter().all(|sample| sample.abs() < 1e-6));

        // Existing output content must be preserved: process adds into the outputs.
        left.fill(1.0);
        right.fill(-1.0);
        convolver.process(&silence, &silence, &mut left, &mut right);
        assert!(left.iter().all(|sample| (*sample - 1.0).abs() < 1e-6));
        assert!(right.iter().all(|sample| (*sample + 1.0).abs() < 1e-6));
    }

    #[test]
    fn dc_through_all_ones_ir_produces_expected_ramp() {
        let block_size = 8;
        let ir = vec![1.0f32; 20];
        let blocks = 5;
        let mut convolver = Convolver::new(&ir, &ir, block_size);
        let input = vec![1.0f32; block_size];
        let mut left_out = vec![0.0; blocks * block_size];

        for block in 0..blocks {
            let mut left = vec![0.0; block_size];
            let mut right = vec![0.0; block_size];
            convolver.process(&input, &input, &mut left, &mut right);
            left_out[block * block_size..(block + 1) * block_size].copy_from_slice(&left);
        }

        for (index, sample) in left_out.iter().enumerate() {
            let expected = ((index + 1).min(ir.len())) as f32;
            assert!(
                (sample - expected).abs() < 1e-3,
                "sample {index}: expected {expected}, got {sample}"
            );
        }
    }

    #[test]
    fn block_bookkeeping_is_correct_for_a_delayed_impulse() {
        let block_size = 16;
        let ir = make_ir(40);
        let impulse_block = 7;
        let impulse_offset = 3;
        let blocks = 32;
        let (left, right) =
            run_impulse(&ir, &ir, block_size, impulse_block, impulse_offset, blocks);

        let start = impulse_block * block_size + impulse_offset;
        for (index, sample) in left.iter().enumerate() {
            if index < start || index >= start + ir.len() {
                assert!(
                    sample.abs() < 1e-4,
                    "unexpected output at {index}: {sample}"
                );
            } else {
                let tap = index - start;
                assert!(
                    (sample - ir[tap]).abs() < 1e-4,
                    "tap {tap} at {index}: expected {}, got {sample}",
                    ir[tap]
                );
            }
        }
        assert_eq!(left, right);
    }

    #[test]
    fn empty_ir_falls_back_to_dry_passthrough() {
        let block_size = 16;
        let mut convolver = Convolver::new(&[], &[], block_size);
        assert_eq!(convolver.block_size(), block_size);
        let left_in = vec![0.25; block_size];
        let right_in = vec![-0.5; block_size];
        let mut left_out = vec![0.0; block_size];
        let mut right_out = vec![0.0; block_size];
        convolver.process(&left_in, &right_in, &mut left_out, &mut right_out);
        assert_eq!(left_out, left_in);
        assert_eq!(right_out, right_in);

        // An empty left IR only bypasses the left channel: the dry impulse passes through.
        let ir = make_ir(12);
        let blocks = 3;
        let (left, right) = run_impulse(&[], &ir, block_size, 0, 0, blocks);
        assert!((left[0] - 1.0).abs() < 1e-6);
        assert!(left[1..].iter().all(|sample| sample.abs() < 1e-6));
        for (index, expected) in ir.iter().enumerate() {
            assert!((right[index] - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn zero_block_size_does_not_panic() {
        let mut convolver = Convolver::new(&make_ir(8), &make_ir(8), 0);
        let input = vec![0.1; 4];
        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        convolver.process(&input, &input, &mut left, &mut right);
        assert_eq!(left, input);
        assert_eq!(right, input);
    }
}
