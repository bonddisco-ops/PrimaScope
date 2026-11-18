use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
pub struct CompSettings {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
}

impl CompSettings {
    /// Static curve gain reduction (dB, >= 0) for an input level in dB, soft knee per Giannoulis et al.
    fn static_gr(&self, xg: f32) -> f32 {
        let t = self.threshold_db;
        let r = self.ratio.max(1.0);
        let w = self.knee_db.max(0.0);
        let d = xg - t;
        let yg = if 2.0 * d < -w {
            xg
        } else if w > 0.0 && 2.0 * d.abs() <= w {
            xg + (1.0 / r - 1.0) * (d + w / 2.0).powi(2) / (2.0 * w)
        } else {
            t + d / r
        };
        xg - yg
    }

    /// Fills `gain` (linear, includes makeup) and `gr_db` (positive dB of reduction) for `n` samples.
    pub fn run(
        &self,
        n: usize,
        sample_rate: f32,
        detector: impl Fn(usize) -> f32,
        gain: &mut Vec<f32>,
        gr_db: &mut Vec<f32>,
    ) {
        gain.clear();
        gr_db.clear();
        gain.reserve(n);
        gr_db.reserve(n);
        let coef = |ms: f32| (-1.0 / (ms.max(0.01) * 0.001 * sample_rate)).exp();
        let a_att = coef(self.attack_ms);
        let a_rel = coef(self.release_ms);
        let makeup = self.makeup_db;
        let mut y = 0.0f32;
        for i in 0..n {
            let x = detector(i).abs();
            let xg = 20.0 * (x + 1e-9).log10();
            let xl = self.static_gr(xg);
            y = if xl > y { a_att * y + (1.0 - a_att) * xl } else { a_rel * y + (1.0 - a_rel) * xl };
            gr_db.push(y);
            gain.push(10f32.powf((makeup - y) / 20.0));
        }
    }
}

pub struct FreqAnalyzer {
    size: usize,
    sample_rate: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
}

impl FreqAnalyzer {
    pub fn new(sample_rate: f32) -> Self {
        // ~170 ms window: enough resolution for kick/bass fundamentals.
        let size = ((sample_rate * 0.17) as usize).next_power_of_two().clamp(2048, 32768);
        let fft = FftPlanner::new().plan_fft_forward(size);
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / size as f32).cos())
            .collect();
        Self { size, sample_rate, fft, window, buf: vec![Complex::default(); size] }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Dominant frequency (Hz) of the signal around `center`. `get` returns None outside the data.
    pub fn dominant(&mut self, center: isize, get: impl Fn(isize) -> Option<f32>) -> Option<f32> {
        let half = (self.size / 2) as isize;
        let mut energy = 0.0f32;
        for i in 0..self.size {
            let v = get(center - half + i as isize).unwrap_or(0.0);
            energy += v * v;
            self.buf[i] = Complex::new(v * self.window[i], 0.0);
        }
        if energy / (self.size as f32) < 1e-8 {
            return None;
        }
        self.fft.process(&mut self.buf);
        let bin_hz = self.sample_rate / self.size as f32;
        let lo = ((20.0 / bin_hz).ceil() as usize).max(1);
        let hi = (self.size / 2 - 2).min((20000.0 / bin_hz) as usize);
        let mag = |k: usize| self.buf[k].norm_sqr().max(1e-20).ln();
        let mut best = lo;
        let mut best_v = f32::MIN;
        for k in lo..=hi {
            let v = self.buf[k].norm_sqr();
            if v > best_v {
                best_v = v;
                best = k;
            }
        }
        let (a, b, c) = (mag(best - 1), mag(best), mag(best + 1));
        let denom = a - 2.0 * b + c;
        let p = if denom.abs() > 1e-12 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
        Some((best as f32 + p) * bin_hz)
    }
}

/// Human-friendly note-length label for a duration in beats (quarter notes).
pub fn beat_label(beats: f64) -> Option<&'static str> {
    const TABLE: &[(f64, &str)] = &[
        (4.0, "1 bar"),
        (2.0, "1/2"),
        (1.0, "1/4"),
        (0.5, "1/8"),
        (0.25, "1/16"),
        (0.125, "1/32"),
        (3.0, "1/2."),
        (1.5, "1/4."),
        (0.75, "1/8."),
        (0.375, "1/16."),
        (4.0 / 3.0, "1/2T"),
        (2.0 / 3.0, "1/4T"),
        (1.0 / 3.0, "1/8T"),
        (1.0 / 6.0, "1/16T"),
    ];
    TABLE
        .iter()
        .find(|(b, _)| ((beats - b) / b).abs() < 0.03)
        .map(|(_, l)| *l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressor_reduces_above_threshold() {
        let s = CompSettings {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 0.1,
            release_ms: 50.0,
            knee_db: 0.0,
            makeup_db: 0.0,
        };
        let (mut g, mut gr) = (Vec::new(), Vec::new());
        s.run(48000, 48000.0, |_| 1.0, &mut g, &mut gr);
        // 0 dB in, -20 thr, 4:1 -> out -15 dB -> 15 dB GR
        assert!((gr[47999] - 15.0).abs() < 0.1, "{}", gr[47999]);
        s.run(1000, 48000.0, |_| 0.01, &mut g, &mut gr);
        assert!(gr[999] < 0.01);
    }

    #[test]
    fn detects_sine_frequency() {
        let sr = 48000.0;
        let mut fa = FreqAnalyzer::new(sr);
        for f in [45.0f32, 60.0, 110.0, 1000.0] {
            let est = fa
                .dominant(20000, |i| Some((2.0 * std::f32::consts::PI * f * i as f32 / sr).sin()))
                .unwrap();
            assert!((est - f).abs() / f < 0.01, "{f} -> {est}");
        }
    }

    #[test]
    fn labels() {
        assert_eq!(beat_label(0.5), Some("1/8"));
        assert_eq!(beat_label(0.66), Some("1/4T"));
    }
}
