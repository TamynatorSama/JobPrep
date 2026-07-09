//! WASAPI playback + voice-activity capture for the live mock interview.
//!
//! Two primitives, both shared-mode + COM-per-thread like `recorder.rs`:
//!   * `play_wav`  — render a WAV buffer (the TTS reply) to the default output,
//!     abortable via a shared `interrupt` flag (barge-in / manual stop).
//!   * `record_answer` — capture the mic with simple RMS voice-activity
//!     detection: wait for speech, record until trailing silence, return a
//!     16-bit mono WAV for the STT endpoint.
//!   * `wait_for_speech` — lightweight monitor used while the interviewer is
//!     talking; trips `detected` as soon as the candidate starts speaking.
//!
//! Endpointing is model-first: while capturing, a rolling 16 kHz tail is sent
//! ~4×/s to the sidecar's Silero-VAD endpoint (`/voice/vad`, see `VadClient`),
//! and the turn ends on the NEURAL speech/no-speech verdict — energy heuristics
//! can't tell a quiet-voiced speaker from silence or transmitted room tone from
//! speech, which caused both premature cuts and never-ending captures. The
//! adaptive energy logic below remains as the fallback when the voice stack is
//! missing or the VAD endpoint is unreachable.

use std::io::Cursor;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use wasapi::{
    Direction, SampleType, StreamMode, WasapiError, WaveFormat,
    DeviceEnumerator,
};

const BUFFER_DURATION_HNS: i64 = 200_000;
const POLL_MS: u32 = 100;

// Streaming-TTS pre-roll: how many seconds of audio to buffer before playback
// starts. A slower-than-real-time engine (e.g. VibeVoice on CPU) drains
// `pending` faster than it fills mid-sentence; this head start absorbs that
// deficit and prevents underrun crackle. Bigger = fewer cracks on long
// sentences but slower to first sound. Faster-than-real-time engines (Piper)
// and short sentences finish synthesizing before this fills and play at once.
const PREROLL_SECS: f64 = 1.0;

// VAD tuning. RMS is over mono samples normalized to [-1, 1]. Hysteresis: it
// takes SPEECH_RMS to *start*, but only CONTINUE_RMS (lower) to keep the turn
// alive — so a mid-sentence pause / breath doesn't get cut as "done".
const SPEECH_RMS: f32 = 0.02;        // above this = speech onset
const CONTINUE_RMS: f32 = 0.011;     // above this during an answer = still talking
const START_SPEECH_MS: f64 = 160.0;  // sustained voice to count as speech start
const END_SILENCE_MS: f64 = 1400.0;  // trailing silence that ends an answer — generous
                                     // (answers have longer think-pauses than
                                     // questions, so this stays above Q_END_SILENCE_MS)
const NO_SPEECH_TIMEOUT_S: f64 = 10.0; // give up if the user never speaks
const MAX_ANSWER_S: f64 = 120.0;      // hard cap on a single answer
const BARGE_SPEECH_MS: f64 = 240.0;   // sustained voice to trigger barge-in

// ── System-audio (interviewer) question capture ────────────────────────────
// Capturing the *interviewer's* voice off the system loopback is a different
// problem from the mic: the meeting volume is unknown and varies wildly, so a
// fixed RMS threshold either misses quiet audio or false-triggers on hum. So we
// calibrate the noise floor for the first few hundred ms and set the thresholds
// RELATIVE to it. The whole design is biased AGAINST false silence detection
// (answering before the question finishes):
//   * generous trailing-silence window — a mid-question pause won't end the turn
//   * a minimum total-voiced guard — a click/notification can't be mistaken for
//     a complete question; if a transient trips onset and then goes silent, we
//     discard it and keep listening instead of "answering" garbage.
const Q_CALIBRATION_MS: f64 = 250.0;   // measure ambient noise floor at start
const Q_ONSET_MULT: f32 = 3.0;         // onset threshold = noise_floor * this
const Q_CONT_MULT: f32 = 1.8;          // continue threshold = noise_floor * this
const Q_FLOOR_MIN: f32 = 0.006;        // floor so thresholds don't collapse in dead silence
const Q_START_SPEECH_MS: f64 = 120.0;  // sustained voice to count as question onset
const Q_END_SILENCE_MS: f64 = 1100.0;  // sustained trailing silence = question complete
                                       // (mid-question think-pauses are typically
                                       // <1s; the adaptive floor below is what
                                       // makes a tighter window safe)
// The one-shot calibration above is taken while the call is often near-silent,
// so its floor is ~0 — but once someone talks, the meeting app transmits their
// room tone / comfort noise CONTINUOUSLY at a level far above digital silence.
// A cont threshold frozen at calibration time then never sees "silence" and the
// capture runs to the hard cap (observed: 50s+ over-captures → 8s transcribes →
// the app "not noticing the question ended"). So during capture the floor is
// re-learned every block: it snaps DOWN to any quieter block instantly and
// drifts UP by Q_FLOOR_DRIFT per block (~+18%/s at ~30ms blocks), folding the
// transmitted background into "silence". It is capped at Q_FLOOR_PEAK_CAP of
// the speaker's running peak so nonstop talking can never drag the floor up to
// speech level and cause a false end.
const Q_FLOOR_DRIFT: f32 = 1.005;      // per-block upward re-learn rate
const Q_FLOOR_PEAK_CAP: f32 = 0.15;    // floor may never exceed this × speech peak
const Q_PEAK_FRACTION: f32 = 0.10;     // silence must also sit below this × speech peak.
                                       // Middle ground for the FALLBACK path: when the
                                       // model verdict is unavailable, a too-loose
                                       // fraction re-creates never-ending captures on
                                       // noisy calls (observed in the field), which is
                                       // worse than an occasional early cut.
const Q_PEAK_DECAY: f32 = 0.998;       // running speech-peak decay (~6.5%/s) so the
                                       // reference adapts down to a trailing-off voice
const Q_MIN_VOICED_MS: f64 = 350.0;    // a "question" must contain at least this much voice
pub const Q_NO_SPEECH_TIMEOUT_S: f64 = 30.0; // interviewer may take a while — wait longer than the mic
/// No-speech window for the semantic CONTINUATION capture (see lib.rs): how
/// long to wait for the interviewer to resume after a transcript that ended
/// mid-sentence. Short — a genuine end just costs this much extra once.
pub const Q_CONTINUATION_TIMEOUT_S: f64 = 2.5;
const Q_MAX_QUESTION_S: f64 = 28.0;    // hard cap on one captured question. Real interview
                                       // questions rarely exceed ~25s of continuous speech;
                                       // when the source NEVER pauses (video voiceover,
                                       // back-to-back speakers — observed in the field as
                                       // 40s+ captures with Silero reporting nonstop
                                       // speech), this flushes what we have so the user
                                       // gets a transcript + answer for the first chunk
                                       // instead of waiting on silence that never comes.
const PREROLL_MS: f64 = 700.0;         // audio kept before confirmed onset so we don't clip the start

// ─── Silero-VAD client (model-based endpointing) ─────────────────────────────

/// Latest verdict from the sidecar's Silero VAD, tagged with the capture-clock
/// time of the tail it was computed on so the capture loop can extrapolate:
/// `silence_now ≈ trailing_silence_ms + (elapsed_now − snapshot_elapsed_ms)`.
#[derive(Clone, Copy, Default)]
pub struct VadVerdict {
    pub snapshot_elapsed_ms: f64,
    pub trailing_silence_ms: f64,
    pub healthy: bool,
}

/// Posts rolling 16 kHz PCM tails to the sidecar's `/voice/vad` from a worker
/// thread and shares the newest verdict with the capture loop. Bounded(1)
/// channel + `try_send`: the audio loop never blocks on HTTP — if the worker is
/// mid-request a snapshot is simply skipped and the next one lands ~250 ms
/// later. The worker exits when the client is dropped.
pub struct VadClient {
    tx: std::sync::mpsc::SyncSender<(f64, Vec<i16>)>,
    verdict: Arc<Mutex<VadVerdict>>,
}

impl VadClient {
    pub fn start(base_url: String) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(f64, Vec<i16>)>(1);
        let verdict = Arc::new(Mutex::new(VadVerdict::default()));
        let shared = Arc::clone(&verdict);
        std::thread::spawn(move || {
            use base64::Engine;
            let client = reqwest::blocking::Client::new();
            let url = format!("{base_url}/voice/vad");
            while let Ok((snap_ms, tail)) = rx.recv() {
                let mut bytes = Vec::with_capacity(tail.len() * 2);
                for s in &tail {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let resp = client
                    .post(&url)
                    .json(&serde_json::json!({ "audio_b64": b64 }))
                    // Generous: a busy box mid-transcribe can stall the verdict;
                    // a late verdict is still better than dropping to the energy
                    // fallback for the rest of the turn.
                    .timeout(std::time::Duration::from_millis(3000))
                    .send()
                    .and_then(|r| r.json::<serde_json::Value>());
                let mut v = shared.lock().unwrap();
                match resp {
                    Ok(j) if j.get("trailing_silence_ms").is_some() => {
                        v.snapshot_elapsed_ms = snap_ms;
                        v.trailing_silence_ms =
                            j["trailing_silence_ms"].as_f64().unwrap_or(0.0);
                        v.healthy = true;
                    }
                    Ok(j) => {
                        eprintln!("[vad-client] endpoint answered without verdict: {j}");
                        v.healthy = false;
                    }
                    Err(e) => {
                        eprintln!("[vad-client] request failed: {e}");
                        v.healthy = false;
                    }
                }
            }
        });
        Self { tx, verdict }
    }

    pub fn submit(&self, snapshot_elapsed_ms: f64, tail: Vec<i16>) {
        let _ = self.tx.try_send((snapshot_elapsed_ms, tail));
    }

    pub fn latest(&self) -> VadVerdict {
        *self.verdict.lock().unwrap()
    }
}

// How the model verdict is applied while capturing:
const VAD_SUBMIT_EVERY_MS: f64 = 250.0;  // tail snapshot cadence
const VAD_TAIL_MS: f64 = 2000.0;         // rolling window sent per snapshot
// A verdict older than this is stale (worker wedged / endpoint down) — fall
// back to energy until a fresh one lands. Verdicts land every ~280 ms, so
// this only trips when something is actually wrong.
const VAD_MAX_AGE_MS: f64 = 900.0;

/// Box-averaging decimator to 16 kHz for the VAD tail — each output sample is
/// the mean of the input samples it spans, a crude low-pass that avoids the
/// aliasing a nearest-sample pick would fold into Silero's input.
struct Tail16k {
    buf: std::collections::VecDeque<i16>,
    cap: usize,
    phase: f64,
    acc: f32,
    acc_n: u32,
}

impl Tail16k {
    fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            cap: (16000.0 * VAD_TAIL_MS / 1000.0) as usize,
            phase: 0.0,
            acc: 0.0,
            acc_n: 0,
        }
    }
    fn push(&mut self, mono: &[f32], sr: u32) {
        let step = 16000.0 / sr.max(1) as f64;
        for &s in mono {
            self.acc += s;
            self.acc_n += 1;
            self.phase += step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                let v = self.acc / self.acc_n as f32;
                self.acc = 0.0;
                self.acc_n = 0;
                if self.buf.len() == self.cap {
                    self.buf.pop_front();
                }
                self.buf.push_back((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
    }
    fn snapshot(&self) -> Vec<i16> {
        self.buf.iter().copied().collect()
    }
}

/// Model-measured trailing silence, or `None` when the verdict is missing or
/// stale (caller falls back to the energy estimate). Deliberately NOT
/// extrapolated past the snapshot: projecting silence forward assumes the
/// speaker didn't resume, and that assumption cut questions short in the field
/// — the end threshold must be met by silence Silero actually saw.
fn model_silence_ms(vad: Option<&VadClient>, elapsed_ms: f64) -> Option<f64> {
    let v = vad?.latest();
    if !v.healthy {
        return None;
    }
    let age = elapsed_ms - v.snapshot_elapsed_ms;
    if !(0.0..=VAD_MAX_AGE_MS).contains(&age) {
        return None;
    }
    Some(v.trailing_silence_ms)
}

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

fn init_mta() -> Result<ComGuard> {
    wasapi::initialize_mta()
        .ok()
        .context("failed to initialize COM (MTA) on this thread")?;
    Ok(ComGuard)
}

// ─── Playback ────────────────────────────────────────────────────────────────

/// Render `wav_bytes` to the default output device. `on_level(rms, zcr)` is
/// called per written chunk so the UI orb can react to the voice. Returns
/// `Ok(true)` if it played to the end, `Ok(false)` if `interrupt` cut it short.
///
/// Currently unused: live TTS playback goes through the streaming PCM path in
/// `lib.rs` (chunks play as they arrive). Kept as the whole-buffer fallback —
/// it's the only complete WASAPI render reference in the crate.
#[allow(dead_code)]
pub fn play_wav(
    wav_bytes: &[u8],
    interrupt: &Arc<AtomicBool>,
    on_level: &mut dyn FnMut(f32, f32),
) -> Result<bool> {
    let reader = hound::WavReader::new(Cursor::new(wav_bytes))
        .context("failed to parse TTS WAV")?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    // Re-pack samples into raw little-endian bytes matching `spec`, build a
    // matching WaveFormat (autoconvert resamples to the mixer), and a mono
    // `meter` (one f32/frame) for the orb level readout.
    let (pcm, wave_format, meter): (Vec<u8>, WaveFormat, Vec<f32>) = match spec.sample_format {
        WavSampleFormat::Int if spec.bits_per_sample == 16 => {
            let samples: Vec<i16> = reader
                .into_samples::<i16>()
                .collect::<std::result::Result<_, _>>()
                .context("failed to read 16-bit samples")?;
            let mut bytes = Vec::with_capacity(samples.len() * 2);
            for s in &samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let meter = samples
                .chunks(channels)
                .map(|f| f.iter().map(|&x| x as f32 / 32768.0).sum::<f32>() / channels as f32)
                .collect();
            (bytes, WaveFormat::new(16, 16, &SampleType::Int, spec.sample_rate as usize, channels, None), meter)
        }
        WavSampleFormat::Float if spec.bits_per_sample == 32 => {
            let samples: Vec<f32> = reader
                .into_samples::<f32>()
                .collect::<std::result::Result<_, _>>()
                .context("failed to read 32-bit float samples")?;
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for s in &samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let meter = samples
                .chunks(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32)
                .collect();
            (bytes, WaveFormat::new(32, 32, &SampleType::Float, spec.sample_rate as usize, channels, None), meter)
        }
        _ => return Err(anyhow!(
            "unsupported TTS WAV format: {} bit {:?}",
            spec.bits_per_sample, spec.sample_format
        )),
    };

    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create device enumerator")?;
    let device = enumerator
        .get_default_device(&Direction::Render)
        .context("no default output device")?;
    let mut client = device.get_iaudioclient().context("failed to create render audio client")?;

    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: BUFFER_DURATION_HNS };
    client
        .initialize_client(&wave_format, &Direction::Render, &mode)
        .context("failed to initialize render stream")?;
    let event = client.set_get_eventhandle().context("failed to get render event handle")?;
    let render = client.get_audiorenderclient().context("failed to get render client")?;
    let buffer_frames = client.get_buffer_size().context("failed to read render buffer size")?;
    let block_align = wave_format.get_blockalign() as usize;

    client.start_stream().context("failed to start render stream")?;

    let mut pos = 0usize;
    let total = pcm.len();
    let mut completed = true;
    while pos < total {
        if interrupt.load(Ordering::SeqCst) {
            completed = false;
            break;
        }
        match event.wait_for_event(POLL_MS) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(e) => return Err(e).context("render event wait failed"),
        }
        let padding = client.get_current_padding().context("failed to read padding")?;
        let avail = buffer_frames.saturating_sub(padding) as usize;
        if avail == 0 {
            continue;
        }
        let want_bytes = (avail * block_align).min(total - pos);
        let frames = want_bytes / block_align;
        if frames == 0 {
            break;
        }
        render
            .write_to_device(frames, &pcm[pos..pos + frames * block_align], None)
            .context("render write failed")?;
        // Report the level of the chunk just queued so the orb tracks speech.
        let fstart = pos / block_align;
        let fend = (fstart + frames).min(meter.len());
        if fstart < fend {
            let (r, z) = level_of(&meter[fstart..fend]);
            on_level(r, z);
        }
        pos += frames * block_align;
    }

    // Let the queued tail drain (unless interrupted).
    if completed {
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < 3.0 {
            if interrupt.load(Ordering::SeqCst) {
                completed = false;
                break;
            }
            let padding = client.get_current_padding().unwrap_or(0);
            if padding == 0 {
                break;
            }
            let _ = event.wait_for_event(POLL_MS);
        }
    }

    client.stop_stream().ok();
    Ok(completed)
}

/// Render a live PCM stream (mono f32 blocks pulled from `rx`) to the default
/// output. Used for streaming TTS: playback starts as soon as the first blocks
/// arrive instead of waiting for the whole sentence WAV. A short pre-roll
/// buffers a head start so brief synth stalls don't immediately starve the
/// device. `on_level(rms, zcr)` drives the orb. Returns `Ok(true)` if it played
/// the whole stream, `Ok(false)` if `interrupt` cut it short.
pub fn play_pcm_stream(
    rx: std::sync::mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    interrupt: &Arc<AtomicBool>,
    on_level: &mut dyn FnMut(f32, f32),
) -> Result<bool> {
    use std::collections::VecDeque;
    use std::sync::mpsc::TryRecvError;

    let sr = sample_rate.max(8000) as usize;
    let wave_format = WaveFormat::new(32, 32, &SampleType::Float, sr, 1, None);

    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create device enumerator")?;
    let device = enumerator
        .get_default_device(&Direction::Render)
        .context("no default output device")?;
    let mut client = device.get_iaudioclient().context("failed to create render audio client")?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: BUFFER_DURATION_HNS };
    client
        .initialize_client(&wave_format, &Direction::Render, &mode)
        .context("failed to initialize render stream")?;
    let event = client.set_get_eventhandle().context("failed to get render event handle")?;
    let render = client.get_audiorenderclient().context("failed to get render client")?;
    let buffer_frames = client.get_buffer_size().context("failed to read render buffer size")?;
    let block_align = wave_format.get_blockalign() as usize; // 4 bytes/frame (mono f32)

    // Samples received but not yet written to the device.
    let mut pending: VecDeque<f32> = VecDeque::new();
    let mut disconnected = false;

    // Pre-roll: buffer PREROLL_SECS of audio (or until the producer finishes)
    // before starting playback so a brief synth stall doesn't instantly
    // underrun. Matters most for slower-than-real-time engines; faster engines
    // (Piper) usually finish a short sentence before this fills.
    let preroll = (sr as f64 * PREROLL_SECS) as usize;
    while pending.len() < preroll && !disconnected {
        if interrupt.load(Ordering::SeqCst) {
            return Ok(false);
        }
        match rx.recv() {
            Ok(block) => pending.extend(block),
            Err(_) => disconnected = true,
        }
    }

    client.start_stream().context("failed to start render stream")?;
    let mut completed = true;

    loop {
        if interrupt.load(Ordering::SeqCst) {
            completed = false;
            break;
        }
        // Pull whatever the producer has ready without blocking.
        if !disconnected {
            loop {
                match rx.try_recv() {
                    Ok(block) => pending.extend(block),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => { disconnected = true; break; }
                }
            }
        }
        if pending.is_empty() {
            if disconnected {
                break; // produced everything and it's all been written
            }
            // Producer behind — block for the next block rather than spin.
            match rx.recv() {
                Ok(block) => pending.extend(block),
                Err(_) => disconnected = true,
            }
            continue;
        }
        match event.wait_for_event(POLL_MS) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(e) => return Err(e).context("render event wait failed"),
        }
        let padding = client.get_current_padding().context("failed to read padding")?;
        let avail = buffer_frames.saturating_sub(padding) as usize;
        if avail == 0 {
            continue;
        }
        let frames = avail.min(pending.len());
        if frames == 0 {
            continue;
        }
        let mut bytes = Vec::with_capacity(frames * block_align);
        let mut meter = Vec::with_capacity(frames);
        for _ in 0..frames {
            let s = pending.pop_front().unwrap();
            meter.push(s);
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        render
            .write_to_device(frames, &bytes, None)
            .context("render write failed")?;
        let (r, z) = level_of(&meter);
        on_level(r, z);
    }

    // Let the queued tail drain (unless interrupted).
    if completed {
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < 3.0 {
            if interrupt.load(Ordering::SeqCst) {
                completed = false;
                break;
            }
            let padding = client.get_current_padding().unwrap_or(0);
            if padding == 0 {
                break;
            }
            let _ = event.wait_for_event(POLL_MS);
        }
    }

    client.stop_stream().ok();
    Ok(completed)
}

// ─── Capture core ────────────────────────────────────────────────────────────

/// Which endpoint to capture from. `Mic` = default capture device. `System` =
/// render-loopback (what's playing through the speakers — i.e. the interviewer's
/// voice in a meeting app). Loopback opens the default *render* device but
/// initializes the client in the Capture direction (same trick as recorder.rs).
#[derive(Clone, Copy, PartialEq)]
enum CaptureSource {
    Mic,
    System,
}

/// Open the requested endpoint and feed mono f32 blocks to `cb` until it returns
/// `false`, `stop` trips, or `max_secs` elapses. `cb(samples, sample_rate,
/// block_ms)`.
fn capture_run<F>(source: CaptureSource, stop: &Arc<AtomicBool>, max_secs: f64, mut cb: F) -> Result<()>
where
    F: FnMut(&[f32], u32, f64) -> bool,
{
    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create device enumerator")?;
    // Loopback grabs the default *render* device but still initializes a Capture
    // stream below; WASAPI shared-mode loopback then mirrors the playback into
    // the capture buffer. Mic uses the default capture device directly.
    let device = match source {
        CaptureSource::Mic => enumerator
            .get_default_device(&Direction::Capture)
            .context("no default microphone")?,
        CaptureSource::System => enumerator
            .get_default_device(&Direction::Render)
            .context("no default output device for loopback")?,
    };
    let mut client = device.get_iaudioclient().context("failed to create capture client")?;
    let format = client.get_mixformat().context("failed to get mic mix format")?;
    let sample_rate = format.get_samplespersec();
    let channels = format.get_nchannels() as usize;
    let bits = format.get_bitspersample();
    let stype = format.get_subformat().context("failed to read mic sample format")?;
    let block_align = format.get_blockalign() as usize;

    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: BUFFER_DURATION_HNS };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .context("failed to initialize capture stream")?;
    let event = client.set_get_eventhandle().context("failed to get capture event handle")?;
    let capture = client.get_audiocaptureclient().context("failed to get capture client")?;
    let buf_frames = client.get_buffer_size().context("failed to read capture buffer size")?.max(1) as usize;
    let mut scratch = vec![0u8; buf_frames * block_align];

    client.start_stream().context("failed to start capture stream")?;
    let start = Instant::now();

    'outer: loop {
        if stop.load(Ordering::SeqCst) || start.elapsed().as_secs_f64() > max_secs {
            break;
        }
        match event.wait_for_event(POLL_MS) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(e) => return Err(e).context("capture event wait failed"),
        }
        loop {
            let pf = capture
                .get_next_packet_size()
                .context("failed to read mic packet size")?
                .unwrap_or(0);
            if pf == 0 {
                break;
            }
            let pb = pf as usize * block_align;
            if scratch.len() < pb {
                scratch.resize(pb, 0);
            }
            let (frames, info) = capture
                .read_from_device(&mut scratch[..pb])
                .context("failed to read mic data")?;
            let used = frames as usize * block_align;
            let mono = packet_to_mono_f32(&scratch[..used], stype, bits, channels, info.flags.silent);
            let block_ms = if sample_rate > 0 { frames as f64 / sample_rate as f64 * 1000.0 } else { 0.0 };
            if !cb(&mono, sample_rate, block_ms) {
                break 'outer;
            }
        }
    }

    client.stop_stream().ok();
    Ok(())
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// Loudness (RMS) + a cheap pitch proxy (zero-crossing rate, ~0..1). Drives the
/// orb: RMS → size, ZCR → hue/morph. No FFT needed.
fn level_of(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sumsq = 0.0f32;
    let mut crossings = 0usize;
    let mut prev = 0.0f32;
    for (i, &s) in samples.iter().enumerate() {
        sumsq += s * s;
        if i > 0 && (s >= 0.0) != (prev >= 0.0) {
            crossings += 1;
        }
        prev = s;
    }
    let rms = (sumsq / samples.len() as f32).sqrt();
    let zcr = crossings as f32 / samples.len() as f32;
    (rms, zcr)
}

/// Watch the mic; trip `detected` and return as soon as the user sustains
/// speech for `BARGE_SPEECH_MS`. Stops when `stop` is set.
pub fn wait_for_speech(stop: &Arc<AtomicBool>, detected: &Arc<AtomicBool>) -> Result<()> {
    let mut voiced_ms = 0.0;
    capture_run(CaptureSource::Mic, stop, MAX_ANSWER_S, |mono, _sr, block_ms| {
        if rms(mono) >= SPEECH_RMS {
            voiced_ms += block_ms;
            if voiced_ms >= BARGE_SPEECH_MS {
                detected.store(true, Ordering::SeqCst);
                return false;
            }
        } else {
            voiced_ms = 0.0;
        }
        true
    })
}

/// Capture one spoken answer with VAD endpointing. `on_level(rms, zcr)` feeds
/// the orb while listening. Returns 16-bit mono WAV bytes, or `None` if the
/// user never spoke.
pub fn record_answer(
    stop: &Arc<AtomicBool>,
    finish: &Arc<AtomicBool>,
    vad: Option<&VadClient>,
    on_level: &mut dyn FnMut(f32, f32),
) -> Result<Option<Vec<u8>>> {
    let mut collected: Vec<f32> = Vec::new();
    let mut sr = 16000u32;
    let mut speech_started = false;
    let mut voiced_ms = 0.0;
    let mut silence_ms = 0.0;
    let mut elapsed_ms = 0.0;
    // Adaptive-floor state (see the Q_FLOOR_* consts + record_question).
    let mut floor_est = CONTINUE_RMS * 0.5;
    let mut peak_ema = 0.0f32;
    let mut tail16 = Tail16k::new();
    let mut last_vad_submit_ms = 0.0f64;

    capture_run(CaptureSource::Mic, stop, MAX_ANSWER_S, |mono, sample_rate, block_ms| {
        // Manual "transcribe now" (button / hotkey): end capture immediately and
        // keep what we have — unlike `stop`, which the caller treats as cancel.
        if finish.load(Ordering::SeqCst) {
            return false;
        }
        sr = sample_rate;
        elapsed_ms += block_ms;
        let (level, zcr) = level_of(mono);
        on_level(level, zcr);
        tail16.push(mono, sample_rate);
        if let Some(v) = vad {
            if speech_started && elapsed_ms - last_vad_submit_ms >= VAD_SUBMIT_EVERY_MS {
                last_vad_submit_ms = elapsed_ms;
                v.submit(elapsed_ms, tail16.snapshot());
            }
        }

        if !speech_started {
            if level >= SPEECH_RMS {
                voiced_ms += block_ms;
                collected.extend_from_slice(mono); // keep the onset
                if voiced_ms >= START_SPEECH_MS {
                    speech_started = true;
                    peak_ema = level;
                }
            } else {
                voiced_ms = 0.0;
                // No speech yet — give up after the no-speech timeout.
                if elapsed_ms / 1000.0 > NO_SPEECH_TIMEOUT_S {
                    return false;
                }
            }
            return true;
        }

        // Recording the answer, with the same continuously-adaptive silence
        // threshold as the question capture: a static CONTINUE_RMS never ends
        // the turn when steady mic noise (fan, AC, breath on the capsule) sits
        // above it. Floor snaps down instantly, drifts up slowly, is capped
        // well below the speaker's running peak.
        collected.extend_from_slice(mono);
        peak_ema = (peak_ema * Q_PEAK_DECAY).max(level.min(1.0));
        if level < floor_est {
            floor_est = level;
        } else {
            floor_est = (floor_est * Q_FLOOR_DRIFT).min(peak_ema * Q_FLOOR_PEAK_CAP);
        }
        let cont_th = (floor_est * Q_CONT_MULT)
            .max(peak_ema * Q_PEAK_FRACTION)
            .max(CONTINUE_RMS * 0.5);
        if level >= cont_th {
            silence_ms = 0.0;
        } else {
            silence_ms += block_ms;
        }
        // Model-first end decision (see record_question): the fresh Silero
        // verdict overrides the energy estimate in both directions.
        if model_silence_ms(vad, elapsed_ms).unwrap_or(silence_ms) >= END_SILENCE_MS {
            return false; // sustained trailing silence → end of answer
        }
        true
    })?;

    // External stop = cancel (user hit stop / disabled voice): DISCARD what was
    // captured — the caller documented `voice_stop_listening` as discard, and
    // transcribing it would pop an unwanted answer after the user backed out.
    if stop.load(Ordering::SeqCst) {
        return Ok(None);
    }
    // Manual finish returns whatever was captured even if VAD never confirmed an
    // onset — the user explicitly ended the turn. Auto endpointing still requires
    // a real onset so a silent timeout doesn't transcribe noise.
    let manual = finish.load(Ordering::SeqCst);
    if collected.is_empty() || (!manual && !speech_started) {
        return Ok(None);
    }
    Ok(Some(encode_wav_mono16(&collected, sr)))
}

/// Capture one interviewer question off the system loopback with noise-floor-
/// adaptive VAD endpointing. Returns 16-bit mono WAV bytes, or `None` if nothing
/// usable was heard before the timeout. Tuned to NOT end on a mid-question pause
/// and to discard transient blips (see the `Q_*` consts).
/// `no_speech_timeout_s`: give up if nobody talks within this window. The
/// first capture of a turn uses `Q_NO_SPEECH_TIMEOUT_S`; the semantic
/// CONTINUATION capture (the transcript looked unfinished, so the caller
/// listens again for the rest of the question) uses a couple of seconds.
pub fn record_question(
    stop: &Arc<AtomicBool>,
    finish: &Arc<AtomicBool>,
    vad: Option<&VadClient>,
    no_speech_timeout_s: f64,
    on_level: &mut dyn FnMut(f32, f32),
) -> Result<Option<Vec<u8>>> {
    use std::collections::VecDeque;

    let mut collected: Vec<f32> = Vec::new();
    let mut preroll: VecDeque<f32> = VecDeque::new();
    let mut preroll_cap = 0usize; // sized once sample rate is known
    let mut sr = 16000u32;
    let mut tail16 = Tail16k::new();
    let mut last_vad_submit_ms = 0.0f64;

    // Calibration accumulators. Floor = the QUIETEST block seen, not the mean:
    // if the interviewer is already mid-sentence when capture (re)arms — common
    // when re-arming right after an answer — a mean would fold that speech into
    // the "noise floor" and inflate the thresholds, clipping the start of the
    // next question. The min tracks the inter-word gaps (≈ true room hum) and
    // ignores the speech bursts, so onset stays sensitive.
    let mut calib_ms = 0.0;
    let mut calib_min = f32::INFINITY;
    let mut calibrated = false;
    let mut onset_th = Q_FLOOR_MIN * Q_ONSET_MULT; // provisional until calibrated
    // Adaptive-floor state for phase 3 (see the Q_FLOOR_* consts): the floor
    // estimate re-learns continuously during capture; `peak_ema` tracks how
    // loud the speaker actually is so both the floor cap and the silence
    // threshold scale with the (unknown) meeting volume.
    let mut floor_est = Q_FLOOR_MIN;
    let mut peak_ema = 0.0f32;

    let mut speech_started = false;
    let mut voiced_ms = 0.0;       // consecutive voiced run while waiting for onset
    let mut silence_ms = 0.0;      // trailing silence once recording
    let mut total_voiced_ms = 0.0; // cumulative voice in the captured turn
    let mut elapsed_ms = 0.0;

    // Keep at most `cap` most-recent samples so a confirmed onset can be
    // back-filled (we only confirm after Q_START_SPEECH_MS of sustained voice).
    fn push_preroll(buf: &mut VecDeque<f32>, mono: &[f32], cap: usize) {
        if cap == 0 {
            return;
        }
        for &s in mono {
            if buf.len() == cap {
                buf.pop_front();
            }
            buf.push_back(s);
        }
    }

    capture_run(CaptureSource::System, stop, Q_MAX_QUESTION_S, |mono, sample_rate, block_ms| {
        // Manual "transcribe now" (button / hotkey): end capture immediately,
        // bypassing the silence detector the user is overriding.
        if finish.load(Ordering::SeqCst) {
            return false;
        }
        sr = sample_rate;
        elapsed_ms += block_ms;
        if preroll_cap == 0 {
            preroll_cap = (sample_rate as f64 * PREROLL_MS / 1000.0) as usize;
        }
        let (level, zcr) = level_of(mono);
        on_level(level, zcr);
        // Feed the model tail continuously (even pre-onset, so the window has
        // context) and, while recording, ship a snapshot every ~250 ms.
        tail16.push(mono, sample_rate);
        if let Some(v) = vad {
            if speech_started && elapsed_ms - last_vad_submit_ms >= VAD_SUBMIT_EVERY_MS {
                last_vad_submit_ms = elapsed_ms;
                v.submit(elapsed_ms, tail16.snapshot());
            }
        }

        // Phase 1 — calibrate the ambient noise floor, set relative thresholds.
        if !calibrated {
            if level < calib_min {
                calib_min = level;
            }
            calib_ms += block_ms;
            push_preroll(&mut preroll, mono, preroll_cap);
            if calib_ms >= Q_CALIBRATION_MS {
                let floor = if calib_min.is_finite() { calib_min } else { 0.0 };
                onset_th = (floor * Q_ONSET_MULT).max(Q_FLOOR_MIN);
                floor_est = floor;
                calibrated = true;
                eprintln!(
                    "[vad] calibrated: floor={floor:.4} onset_th={onset_th:.4} \
                     (end_silence={Q_END_SILENCE_MS:.0}ms preroll={PREROLL_MS:.0}ms max={Q_MAX_QUESTION_S:.0}s)"
                );
            }
            return true;
        }

        // Phase 2 — wait for a sustained onset above the noise floor.
        if !speech_started {
            push_preroll(&mut preroll, mono, preroll_cap);
            if level >= onset_th {
                voiced_ms += block_ms;
                if voiced_ms >= Q_START_SPEECH_MS {
                    speech_started = true;
                    // Prepend the pre-roll so the start of the question isn't clipped.
                    collected.extend(preroll.drain(..));
                    total_voiced_ms = voiced_ms;
                    peak_ema = level; // seed the speaker-loudness estimate
                    eprintln!("[vad] onset @ {:.1}s (level={level:.4})", elapsed_ms / 1000.0);
                }
            } else {
                voiced_ms = 0.0;
                if elapsed_ms / 1000.0 > no_speech_timeout_s {
                    eprintln!("[vad] no speech within {no_speech_timeout_s:.0}s — giving up");
                    return false; // interviewer never spoke in the window
                }
            }
            return true;
        }

        // Phase 3 — recording the question, with a continuously-adaptive
        // silence threshold. The floor snaps down to any quieter block and
        // drifts back up slowly (capped well below the speaker's level), and
        // "silence" must also sit below a fraction of the speaker's running
        // peak — so both a near-silent call and one with transmitted room
        // tone / comfort noise endpoint correctly at the same tuning.
        collected.extend_from_slice(mono);
        peak_ema = (peak_ema * Q_PEAK_DECAY).max(level.min(1.0));
        if level < floor_est {
            floor_est = level;
        } else {
            floor_est = (floor_est * Q_FLOOR_DRIFT).min(peak_ema * Q_FLOOR_PEAK_CAP);
        }
        let cont_th = (floor_est * Q_CONT_MULT)
            .max(peak_ema * Q_PEAK_FRACTION)
            .max(Q_FLOOR_MIN * 0.7);
        if level >= cont_th {
            silence_ms = 0.0;
            total_voiced_ms += block_ms;
        } else {
            silence_ms += block_ms;
        }
        // End decision. When the Silero verdict is fresh it is AUTHORITATIVE —
        // for both directions: it keeps the turn alive through a quiet-voiced
        // stretch the energy path would misread as silence, and it ends the
        // turn through background noise the energy path would misread as
        // speech. Energy-only is the fallback (voice stack absent / endpoint
        // down / verdict stale).
        let effective_silence = model_silence_ms(vad, elapsed_ms).unwrap_or(silence_ms);
        if effective_silence >= Q_END_SILENCE_MS {
            if total_voiced_ms >= Q_MIN_VOICED_MS {
                eprintln!(
                    "[vad] END (silence {:.0}ms): dur={:.1}s voiced={:.0}ms \
                     floor={floor_est:.4} peak={peak_ema:.4} cont_th={cont_th:.4}",
                    effective_silence, elapsed_ms / 1000.0, total_voiced_ms
                );
                return false; // genuine question, fully ended → transcribe
            }
            // False trigger (a click / notification blip): discard and keep
            // listening rather than answering a non-question.
            eprintln!("[vad] discard blip: voiced={:.0}ms (< {:.0}ms min)", total_voiced_ms, Q_MIN_VOICED_MS);
            speech_started = false;
            voiced_ms = 0.0;
            silence_ms = 0.0;
            total_voiced_ms = 0.0;
            collected.clear();
            preroll.clear();
        }
        true
    })?;

    // Reached here without a silence-triggered end ⇒ hit the hard cap (or stop).
    // A capture that runs to Q_MAX_QUESTION_S means silence was NEVER detected —
    // i.e. continuous meeting audio kept `level` above cont_th the whole time.
    if speech_started && elapsed_ms / 1000.0 >= Q_MAX_QUESTION_S - 0.5 {
        eprintln!(
            "[vad] HARD CAP {:.0}s hit — silence never detected \
             (floor={floor_est:.4} peak={peak_ema:.4} vs continuous background?). Captured {:.1}s.",
            Q_MAX_QUESTION_S, collected.len() as f64 / sr.max(1) as f64
        );
    }

    // External stop = cancel (user hit stop / turned Rec off): DISCARD what was
    // captured — transcribing it would pop an unwanted answer after backing out.
    if stop.load(Ordering::SeqCst) {
        eprintln!("[vad] externally stopped — discarding capture");
        return Ok(None);
    }
    // Manual finish overrides the silence detector: return whatever was captured.
    // If the user forced it before a confirmed onset, fall back to the pre-roll
    // ring so we still transcribe the most recent audio rather than nothing.
    let manual = finish.load(Ordering::SeqCst);
    if manual && !speech_started {
        collected.extend(preroll.drain(..));
    }
    if collected.is_empty() || (!manual && (!speech_started || total_voiced_ms < Q_MIN_VOICED_MS)) {
        eprintln!(
            "[vad] no usable question (started={speech_started} voiced={total_voiced_ms:.0}ms \
             collected={} manual={manual})",
            collected.len()
        );
        return Ok(None);
    }
    eprintln!(
        "[vad] question captured: {:.1}s audio @ {}Hz → STT",
        collected.len() as f64 / sr.max(1) as f64, sr
    );
    Ok(Some(encode_wav_mono16(&collected, sr)))
}

fn encode_wav_mono16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: WavSampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut w = WavWriter::new(&mut buf, spec).expect("wav writer");
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            let _ = w.write_sample(v);
        }
        let _ = w.finalize();
    }
    buf.into_inner()
}

fn packet_to_mono_f32(
    bytes: &[u8],
    stype: SampleType,
    bits: u16,
    channels: usize,
    silent: bool,
) -> Vec<f32> {
    let bytes_per_sample = (bits / 8) as usize;
    if bytes_per_sample == 0 || channels == 0 {
        return Vec::new();
    }
    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 || bytes.len() < frame_bytes {
        return Vec::new();
    }
    let frames = bytes.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames);
    if silent {
        out.resize(frames, 0.0);
        return out;
    }
    for f in 0..frames {
        let base = f * frame_bytes;
        let mut acc = 0.0f32;
        for ch in 0..channels {
            let off = base + ch * bytes_per_sample;
            let chunk = &bytes[off..off + bytes_per_sample];
            acc += decode_sample(stype, bits, chunk);
        }
        out.push(acc / channels as f32);
    }
    out
}

fn decode_sample(stype: SampleType, bits: u16, chunk: &[u8]) -> f32 {
    match (stype, bits) {
        (SampleType::Float, 32) => {
            let b: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
            f32::from_le_bytes(b)
        }
        (SampleType::Int, 16) => {
            let b: [u8; 2] = chunk.try_into().unwrap_or([0; 2]);
            i16::from_le_bytes(b) as f32 / 32768.0
        }
        (SampleType::Int, 32) => {
            let b: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
            i32::from_le_bytes(b) as f32 / 2_147_483_648.0
        }
        (SampleType::Int, 24) => {
            if chunk.len() < 3 {
                return 0.0;
            }
            let raw = (chunk[0] as i32) | ((chunk[1] as i32) << 8) | ((chunk[2] as i32) << 16);
            let signed = if raw & 0x80_0000 != 0 { raw | !0xFF_FFFF } else { raw };
            signed as f32 / 8_388_608.0
        }
        _ => 0.0,
    }
}
