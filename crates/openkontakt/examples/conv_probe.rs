//! Feed an impulse through a real impulse response and report the tail energy.
use openkontakt::dsp::Convolver;
use openkontakt::wav::load_wav;

fn main() {
    let path = std::env::args().nth(1).expect("usage: conv_probe <ir.wav>");
    let sample = load_wav(std::path::Path::new(&path)).expect("ir");
    let mut left = vec![0.0f32; sample.frames];
    let mut right = vec![0.0f32; sample.frames];
    for (index, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let (sl, sr) = sample.frame(index);
        *l = sl;
        *r = sr;
    }
    println!("ir: {} frames at {} Hz", sample.frames, sample.sample_rate);
    let block = 512usize;
    let mut conv = Convolver::new(&left, &right, block);
    let mut in_l = vec![0.0f32; block];
    let mut in_r = vec![0.0f32; block];
    in_l[0] = 1.0;
    in_r[0] = 1.0;
    let mut out_l = vec![0.0f32; block];
    let mut out_r = vec![0.0f32; block];
    let mut peak = 0.0f32;
    let mut energy = 0.0f64;
    for block_index in 0..40 {
        out_l.iter_mut().for_each(|v| *v = 0.0);
        out_r.iter_mut().for_each(|v| *v = 0.0);
        conv.process(&in_l, &in_r, &mut out_l, &mut out_r);
        let block_peak = out_l.iter().fold(0.0f32, |p, v| p.max(v.abs()));
        if block_index < 4 {
            println!("  block {block_index}: peak {block_peak:.6}");
        }
        peak = peak.max(block_peak);
        energy += out_l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        in_l.iter_mut().for_each(|v| *v = 0.0);
        in_r.iter_mut().for_each(|v| *v = 0.0);
    }
    println!("peak {peak:.6}, energy {energy:.6}");
}
