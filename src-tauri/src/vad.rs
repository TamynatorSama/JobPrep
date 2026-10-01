//! In-process Silero VAD + 16 kHz resampling for the copilot's live capture.
//!
//! The model is Silero v6 as faster-whisper ships it (`silero_vad_v6.onnx`):
//! each call takes one 32 ms frame (512 samples at 16 kHz) plus the previous
//! frame's last 64 samples, and carries an LSTM state (h, c) between calls.
//! Run frame by frame it matches faster-whisper's batched call to within 0.002
//! and costs ~0.1 ms on one CPU thread. This replaces POSTing a 2 s audio tail
//! to the sidecar's /voice/vad every 160 ms: verdicts are per frame, never
//! stale, and keep coming while the sidecar is busy decoding.

use anyhow::{anyhow, Result};
use ort::session::Session;
use ort::value::Tensor;

/// Sample rate the VAD and the sidecar's streaming ASR both expect.
pub const SR: u32 = 16_000;
/// One VAD frame: 512 samples = 32 ms at 16 kHz.
pub const FRAME: usize = 512;
pub const FRAME_MS: f64 = FRAME as f64 * 1000.0 / SR as f64;
const CONTEXT: usize = 64;
const STATE: usize = 128;

static MODEL: &[u8] = include_bytes!("../models/silero_vad_v6.onnx");

fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("silero vad: {e}")
}

pub struct Silero {
    session: Session,
    h: Vec<f32>,
    c: Vec<f32>,
    ctx: [f32; CONTEXT],
}

impl Silero {
    pub fn new() -> Result<Self> {
        let session = Session::builder()
            .map_err(ort_err)?
            .with_intra_threads(1)
            .map_err(ort_err)?
            .with_inter_threads(1)
            .map_err(ort_err)?
            .commit_from_memory(MODEL)
            .map_err(ort_err)?;
        Ok(Self { session, h: vec![0.0; STATE], c: vec![0.0; STATE], ctx: [0.0; CONTEXT] })
    }

    /// Speech probability (0..1) for one `FRAME`-sample block of 16 kHz mono
    /// audio in [-1, 1]. Frames must be fed in order: the model is recurrent.
    pub fn prob(&mut self, frame: &[f32]) -> Result<f32> {
        if frame.len() != FRAME {
            return Err(anyhow!("silero vad: frame must be {FRAME} samples, got {}", frame.len()));
        }
        let mut x = Vec::with_capacity(CONTEXT + FRAME);
        x.extend_from_slice(&self.ctx);
        x.extend_from_slice(frame);
        let input = Tensor::from_array(([1usize, CONTEXT + FRAME], x)).map_err(ort_err)?;
        let h = Tensor::from_array(([1usize, 1, STATE], self.h.clone())).map_err(ort_err)?;
        let c = Tensor::from_array(([1usize, 1, STATE], self.c.clone())).map_err(ort_err)?;
        let out = self
            .session
            .run(ort::inputs!["input" => input, "h" => h, "c" => c])
            .map_err(ort_err)?;
        let (_, p) = out["speech_probs"].try_extract_tensor::<f32>().map_err(ort_err)?;
        let p = p.first().copied().unwrap_or(0.0);
        let (_, hn) = out["hn"].try_extract_tensor::<f32>().map_err(ort_err)?;
        let (_, cn) = out["cn"].try_extract_tensor::<f32>().map_err(ort_err)?;
        self.h.copy_from_slice(&hn[..STATE]);
        self.c.copy_from_slice(&cn[..STATE]);
        self.ctx.copy_from_slice(&frame[FRAME - CONTEXT..]);
        Ok(p)
    }
}

/// Streaming resampler to 16 kHz. Shared-mode loopback is usually 48 kHz; the
/// box average `voice_audio::Tail16k` uses for the old VAD tail folds energy
/// above 8 kHz back into the band Whisper hears, so this runs a 48-tap
/// Hamming-windowed-sinc low-pass (7 kHz cutoff, stopband from ~10 kHz) first
/// and then linearly interpolates onto the 16 kHz grid. ~2.3 M multiply-adds
/// per second at 48 kHz.
pub struct Resampler16k {
    taps: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    /// Input samples per output sample.
    step: f64,
    /// Input-sample index of the next output sample.
    next_t: f64,
    /// Index of the next input sample.
    n: u64,
    y_prev: f32,
}

impl Resampler16k {
    pub fn new(in_sr: u32) -> Self {
        let in_sr = in_sr.max(1);
        let taps = if in_sr > SR {
            const N: usize = 48;
            let fc = 7_000.0 / in_sr as f64; // cycles per input sample
            let mid = (N - 1) as f64 / 2.0;
            let mut t: Vec<f64> = (0..N)
                .map(|i| {
                    let x = i as f64 - mid;
                    let sinc = if x == 0.0 {
                        2.0 * fc
                    } else {
                        (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                    };
                    let hamming = 0.54
                        - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (N - 1) as f64).cos();
                    sinc * hamming
                })
                .collect();
            let sum: f64 = t.iter().sum();
            t.iter_mut().for_each(|v| *v /= sum);
            t.into_iter().map(|v| v as f32).collect()
        } else {
            vec![1.0]
        };
        Self {
            ring: vec![0.0; taps.len()],
            taps,
            ring_pos: 0,
            step: in_sr as f64 / SR as f64,
            next_t: 0.0,
            n: 0,
            y_prev: 0.0,
        }
    }

    /// Append the 16 kHz samples produced by `input` to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let len = self.taps.len();
        for &s in input {
            self.ring[self.ring_pos] = s;
            self.ring_pos = (self.ring_pos + 1) % len;
            // ring_pos now points at the oldest sample.
            let mut y = 0.0f32;
            for (k, &tap) in self.taps.iter().enumerate() {
                y += tap * self.ring[(self.ring_pos + k) % len];
            }
            let n = self.n as f64;
            while self.next_t <= n {
                let frac = (self.next_t - (n - 1.0)) as f32;
                out.push(self.y_prev + (y - self.y_prev) * frac);
                self.next_t += self.step;
            }
            self.y_prev = y;
            self.n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sr: u32, hz: f64, secs: f64) -> Vec<f32> {
        (0..(sr as f64 * secs) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / sr as f64).sin() as f32 * 0.5)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn resampler_keeps_rate_and_speech_band() {
        for sr in [48_000u32, 44_100, 16_000] {
            let mut r = Resampler16k::new(sr);
            let mut out = Vec::new();
            // Feed in uneven packets, the way WASAPI delivers them.
            for chunk in tone(sr, 1_000.0, 1.0).chunks(441) {
                r.push(chunk, &mut out);
            }
            assert!((out.len() as i64 - 16_000).abs() <= 2, "{sr}: {} samples", out.len());
            let level = rms(&out[1_000..]);
            assert!((level - 0.3536).abs() < 0.02, "{sr}: 1 kHz rms {level}");
        }
    }

    #[test]
    fn resampler_rejects_energy_above_nyquist() {
        let mut r = Resampler16k::new(48_000);
        let mut out = Vec::new();
        r.push(&tone(48_000, 12_000.0, 1.0), &mut out);
        assert!(rms(&out[1_000..]) < 0.02, "12 kHz leaked: {}", rms(&out[1_000..]));
    }

    #[test]
    fn silero_runs_and_hears_no_speech_in_silence() {
        let mut vad = Silero::new().expect("model loads");
        let silence = vec![0.0f32; FRAME];
        let mut last = 1.0;
        for _ in 0..20 {
            last = vad.prob(&silence).expect("inference");
        }
        assert!(last < 0.2, "silence prob {last}");
        assert!(vad.prob(&silence[..100]).is_err());
    }
}
