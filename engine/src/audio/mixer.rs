//! Mixing N decoded streams with per-peer gain and a soft limiter (SPEC §9).

/// Adds `input` (interleaved, `in_ch` channels) into `out` (interleaved, `out_ch`
/// channels) with `gain`, converting mono↔stereo as needed. Frame counts must match.
pub fn mix_into(out: &mut [f32], out_ch: usize, input: &[f32], in_ch: usize, gain: f32) {
    let frames = out.len() / out_ch.max(1);
    let in_frames = input.len() / in_ch.max(1);
    let n = frames.min(in_frames);
    match (in_ch, out_ch) {
        (1, 2) => {
            for i in 0..n {
                let s = input[i] * gain;
                out[2 * i] += s;
                out[2 * i + 1] += s;
            }
        }
        (2, 1) => {
            for i in 0..n {
                out[i] += (input[2 * i] + input[2 * i + 1]) * 0.5 * gain;
            }
        }
        (a, b) if a == b => {
            for i in 0..n * a {
                out[i] += input[i] * gain;
            }
        }
        _ => {
            // Anything exotic: take the first channel of each.
            for i in 0..n {
                out[i * out_ch] += input[i * in_ch] * gain;
            }
        }
    }
}

/// Soft knee above `threshold`: loud sums bend towards ±1 instead of clipping.
pub fn soft_limit(samples: &mut [f32]) {
    const THRESHOLD: f32 = 0.85;
    for s in samples.iter_mut() {
        let a = s.abs();
        if a > THRESHOLD {
            let over = a - THRESHOLD;
            let compressed = THRESHOLD + (1.0 - THRESHOLD) * (over / (over + (1.0 - THRESHOLD)));
            *s = compressed.copysign(*s);
        }
    }
}

/// Peak absolute level of a buffer, 0.0 .. 1.0, for the mic meter.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs())).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_conversion_and_gain() {
        let mut out = vec![0.0; 4];
        mix_into(&mut out, 2, &[0.5, -0.5], 1, 2.0);
        assert_eq!(out, vec![1.0, 1.0, -1.0, -1.0]);
        let mut mono = vec![0.0; 2];
        mix_into(&mut mono, 1, &[0.2, 0.4, 1.0, 0.0], 2, 1.0);
        assert!((mono[0] - 0.3).abs() < 1e-6 && (mono[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn limiter_never_exceeds_one_and_leaves_quiet_alone() {
        let mut loud = vec![3.0, -2.0, 0.9, 0.5, -0.1];
        soft_limit(&mut loud);
        assert!(loud.iter().all(|s| s.abs() <= 1.0));
        assert!(loud[0] > 0.85 && loud[1] < -0.85);
        assert_eq!(loud[3], 0.5);
        assert_eq!(loud[4], -0.1);
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
    }
}
