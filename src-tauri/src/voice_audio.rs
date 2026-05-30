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
//! VAD here is deliberately crude (energy threshold + hangover). It's good
//! enough for turn-taking in a quiet room and easy to tune via the consts.

use std::io::Cursor;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
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

// VAD tuning. RMS is over mono samples normalized to [-1, 1]. Hysteresis: it
// takes SPEECH_RMS to *start*, but only CONTINUE_RMS (lower) to keep the turn
// alive — so a mid-sentence pause / breath doesn't get cut as "done".
const SPEECH_RMS: f32 = 0.02;        // above this = speech onset
const CONTINUE_RMS: f32 = 0.011;     // above this during an answer = still talking
const START_SPEECH_MS: f64 = 160.0;  // sustained voice to count as speech start
const END_SILENCE_MS: f64 = 1700.0;  // trailing silence that ends an answer (generous)
const NO_SPEECH_TIMEOUT_S: f64 = 10.0; // give up if the user never speaks
const MAX_ANSWER_S: f64 = 120.0;      // hard cap on a single answer
const BARGE_SPEECH_MS: f64 = 240.0;   // sustained voice to trigger barge-in

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

// ─── Capture core ────────────────────────────────────────────────────────────

/// Open the default mic and feed mono f32 blocks to `cb` until it returns
/// `false`, `stop` trips, or `max_secs` elapses. `cb(samples, sample_rate,
/// block_ms)`.
fn capture_run<F>(stop: &Arc<AtomicBool>, max_secs: f64, mut cb: F) -> Result<()>
where
    F: FnMut(&[f32], u32, f64) -> bool,
{
    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create device enumerator")?;
    let device = enumerator
        .get_default_device(&Direction::Capture)
        .context("no default microphone")?;
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
    capture_run(stop, MAX_ANSWER_S, |mono, _sr, block_ms| {
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
    on_level: &mut dyn FnMut(f32, f32),
) -> Result<Option<Vec<u8>>> {
    let mut collected: Vec<f32> = Vec::new();
    let mut sr = 16000u32;
    let mut speech_started = false;
    let mut voiced_ms = 0.0;
    let mut silence_ms = 0.0;
    let mut elapsed_ms = 0.0;

    capture_run(stop, MAX_ANSWER_S, |mono, sample_rate, block_ms| {
        sr = sample_rate;
        elapsed_ms += block_ms;
        let (level, zcr) = level_of(mono);
        on_level(level, zcr);

        if !speech_started {
            if level >= SPEECH_RMS {
                voiced_ms += block_ms;
                collected.extend_from_slice(mono); // keep the onset
                if voiced_ms >= START_SPEECH_MS {
                    speech_started = true;
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

        // Recording the answer. Use the lower CONTINUE_RMS so soft speech and
        // brief pauses don't prematurely end the turn.
        collected.extend_from_slice(mono);
        if level >= CONTINUE_RMS {
            silence_ms = 0.0;
        } else {
            silence_ms += block_ms;
            if silence_ms >= END_SILENCE_MS {
                return false; // sustained trailing silence → end of answer
            }
        }
        true
    })?;

    if !speech_started || collected.is_empty() {
        return Ok(None);
    }
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
