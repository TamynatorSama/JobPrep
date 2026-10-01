//! Copilot live listening: one continuous system-audio capture per Rec session.
//!
//! Pipeline, all on the PC:
//!   WASAPI loopback → `Resampler16k` → 32 ms frames → Silero VAD (`vad.rs`)
//!   → `Tracker` (per-question endpointing) → `Controller` thread, which
//!   streams the question's audio into a faster-whisper session in the sidecar
//!   (`/voice/asr/*`) and starts the copilot answer itself.
//!
//! Why it's shaped this way (Phase 2 of the copilot upgrade blueprint): the old
//! loop captured one question, waited 650–1,100 ms of silence, sent the WAV to
//! `/voice/stt` (~375 ms), then handed the text to the overlay, which started
//! the LLM. Here the transcript is decoded while the interviewer pauses
//! (`Pause`, 160 ms of silence), so at `Spec` (320 ms) it is already there and
//! the answer starts immediately. If they keep talking, the speculative answer
//! is cancelled (`Resumed`) and the next endpoint waits for `Final` (900 ms) —
//! one cheap restart instead of a slow endpoint on every question. Capture
//! never stops between questions, so one asked while an answer streams is
//! heard from its first word.
//!
//! Events to the overlay (all carry `qid`):
//!   `copilot:speech`   {qid}                        interviewer started a question
//!   `copilot:partial`  {qid, text}                  live transcript
//!   `copilot:question` {qid, text, streamId, attempt, speculative, asrMs}
//!                      the answer is streaming as `chat:*` under `streamId`
//!   `copilot:resume`   {qid, streamId}              they kept talking; answer cancelled
//!   `copilot:commit`   {qid, answered}              question over
//!   `copilot:error`    {message}                    listener died

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::vad::{Resampler16k, Silero, FRAME, FRAME_MS, SR};

// ── Endpointing constants (16 kHz samples / milliseconds) ────────────────────
// Silero hysteresis: a frame starts speech at SPEECH_TH, ends it below SILENCE_TH.
const SPEECH_TH: f32 = 0.5;
const SILENCE_TH: f32 = 0.35;
/// Consecutive speech frames that open a question (96 ms).
const ONSET_FRAMES: u32 = 3;
/// Consecutive speech frames inside a pause that count as "still talking".
/// Shorter blips (a cough, a click) leave the pause running.
const RESUME_FRAMES: u32 = 4;
/// Audio kept from before the onset so the first word isn't clipped.
const PREROLL_MS: f64 = 500.0;
/// A question needs this much voice; less is a blip and is discarded.
const MIN_VOICED_MS: f64 = 350.0;
/// Silence milestones. PAUSE asks the sidecar to decode now; SPEC starts the
/// answer speculatively (first attempt only); FINAL starts it after a restart
/// or when the transcript ends mid-clause; LONG_FINAL starts it no matter what;
/// COMMIT ends the question (later speech is a new one).
const PAUSE_MS: f64 = 160.0;
const SPEC_MS: f64 = 320.0;
const FINAL_MS: f64 = 900.0;
const LONG_FINAL_MS: f64 = 1_800.0;
const COMMIT_MS: f64 = 2_200.0;
/// Longest question; past it the answer starts on what was heard (a source
/// that never pauses — a video, back-to-back speakers).
const MAX_QUESTION_MS: f64 = 28_000.0;
/// Audio is shipped to the sidecar in ~128 ms chunks.
const CHUNK_SAMPLES: usize = FRAME * 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Spec,
    Final,
    LongFinal,
    /// Manual "answer now" (hotkey / button) or the question hit the length cap.
    Forced,
}

impl Mark {
    fn bit(self) -> u8 {
        match self {
            Mark::Spec => 1,
            Mark::Final => 2,
            Mark::LongFinal => 4,
            Mark::Forced => 8,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Ev {
    Onset { qid: u64 },
    /// 16 kHz PCM16 for the current question, in order.
    Audio(Vec<i16>),
    /// Short pause: decode what we have. `speech_end` = samples up to the
    /// last voiced frame, counted from the question's first sample.
    Pause { speech_end: usize },
    Silence { mark: Mark, speech_end: usize },
    /// Speech came back after a pause long enough to have started an answer.
    Resumed,
    Commit { voiced_ok: bool },
    /// Capture ended (Rec off, or the device/VAD failed with `error`).
    Stopped { error: Option<String> },
}

struct Question {
    voiced_ms: f64,
    silence_ms: f64,
    speaking: bool,
    resume_run: u32,
    sent: usize,
    speech_end: usize,
    pending: Vec<i16>,
    elapsed_ms: f64,
    pause_sent: bool,
    marks: u8,
}

/// Per-question endpointing over VAD frames. Pure: frames + probabilities in,
/// `Ev`s out, so it is unit-tested without audio devices or a sidecar.
pub struct Tracker {
    ring: VecDeque<i16>,
    onset_run: u32,
    q: Option<Question>,
    next_qid: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self { ring: VecDeque::new(), onset_run: 0, q: None, next_qid: 0 }
    }

    pub fn in_question(&self) -> bool {
        self.q.is_some()
    }

    /// Feed one `FRAME`-sample 16 kHz frame and its speech probability.
    /// `force` = the user asked for an answer now.
    pub fn frame(&mut self, pcm: &[f32], prob: f32, force: bool, out: &mut Vec<Ev>) {
        let pcm16: Vec<i16> = pcm.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
        let preroll_cap = (SR as f64 * PREROLL_MS / 1000.0) as usize;
        self.ring.extend(pcm16.iter().copied());
        while self.ring.len() > preroll_cap {
            self.ring.pop_front();
        }

        let Some(q) = self.q.as_mut() else {
            self.onset_run = if prob >= SPEECH_TH { self.onset_run + 1 } else { 0 };
            if self.onset_run >= ONSET_FRAMES {
                self.next_qid += 1;
                self.onset_run = 0;
                let pre: Vec<i16> = self.ring.iter().copied().collect();
                let n = pre.len();
                out.push(Ev::Onset { qid: self.next_qid });
                out.push(Ev::Audio(pre));
                self.q = Some(Question {
                    voiced_ms: ONSET_FRAMES as f64 * FRAME_MS,
                    silence_ms: 0.0,
                    speaking: true,
                    resume_run: 0,
                    sent: n,
                    speech_end: n,
                    pending: Vec::new(),
                    elapsed_ms: 0.0,
                    pause_sent: false,
                    marks: 0,
                });
            }
            return;
        };

        q.pending.extend_from_slice(&pcm16);
        q.sent += pcm16.len();
        q.elapsed_ms += FRAME_MS;
        if prob >= SPEECH_TH {
            q.speaking = true;
        } else if prob < SILENCE_TH {
            q.speaking = false;
        }

        if q.speaking && q.silence_ms == 0.0 {
            q.voiced_ms += FRAME_MS;
            q.speech_end = q.sent;
        } else if q.speaking {
            // Speech inside a pause: only RESUME_FRAMES in a row reopen the turn.
            q.resume_run += 1;
            if q.resume_run >= RESUME_FRAMES {
                if q.marks & Mark::Spec.bit() != 0 {
                    out.push(Ev::Resumed);
                }
                q.voiced_ms += RESUME_FRAMES as f64 * FRAME_MS;
                q.silence_ms = 0.0;
                q.resume_run = 0;
                q.pause_sent = false;
                q.marks &= Mark::Forced.bit();
                q.speech_end = q.sent;
            } else {
                q.silence_ms += FRAME_MS;
            }
        } else {
            q.resume_run = 0;
            q.silence_ms += FRAME_MS;
        }

        fn flush(q: &mut Question, out: &mut Vec<Ev>) {
            if !q.pending.is_empty() {
                out.push(Ev::Audio(std::mem::take(&mut q.pending)));
            }
        }

        let voiced_ok = q.voiced_ms >= MIN_VOICED_MS;
        if q.silence_ms > 0.0 && q.resume_run == 0 && voiced_ok {
            if !q.pause_sent && q.silence_ms >= PAUSE_MS {
                flush(q, out);
                out.push(Ev::Pause { speech_end: q.speech_end });
                q.pause_sent = true;
            }
            for (mark, ms) in [(Mark::Spec, SPEC_MS), (Mark::Final, FINAL_MS), (Mark::LongFinal, LONG_FINAL_MS)] {
                if q.marks & mark.bit() == 0 && q.silence_ms >= ms {
                    flush(q, out);
                    out.push(Ev::Silence { mark, speech_end: q.speech_end });
                    q.marks |= mark.bit();
                }
            }
        }
        let capped = q.elapsed_ms >= MAX_QUESTION_MS;
        if (force || capped) && q.marks & Mark::Forced.bit() == 0 && voiced_ok {
            flush(q, out);
            out.push(Ev::Silence { mark: Mark::Forced, speech_end: q.sent });
            q.marks |= Mark::Forced.bit();
        }
        if q.pending.len() >= CHUNK_SAMPLES {
            flush(q, out);
        }
        if q.silence_ms >= COMMIT_MS || capped {
            flush(q, out);
            out.push(Ev::Commit { voiced_ok });
            self.q = None;
        }
    }
}

/// Strong "the speaker was cut off mid-sentence" heuristic. Conservative:
/// a false positive delays the answer to the FINAL endpoint (~0.6 s), a false
/// negative answers a question from its first clause — so only a trailing
/// comma or an obviously clause-opening final word counts.
pub fn looks_unfinished(text: &str) -> bool {
    let t = text.trim_end();
    if t.is_empty() {
        return false;
    }
    // Trust explicit terminal punctuation before inspecting the last word;
    // "What are you passionate about?" ends in a clause-opener.
    if t.ends_with('.') || t.ends_with('?') || t.ends_with('!') {
        return false;
    }
    if t.ends_with(',') || t.ends_with(';') || t.ends_with(':') || t.ends_with('-') {
        return true;
    }
    let last_word = t
        .rsplit(|c: char| !c.is_alphanumeric() && c != '\'')
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        last_word.as_str(),
        "and" | "or" | "but" | "so" | "because" | "if" | "when" | "while"
            | "with" | "to" | "of" | "for" | "about" | "the" | "a" | "an"
            | "your" | "how" | "what" | "which" | "that" | "into" | "on" | "in"
    )
}

// ── Controller: ASR session + answer lifecycle ────────────────────────────────

/// Streaming speech recognition for one question at a time.
pub trait Asr: Send {
    fn start(&mut self) -> Result<(), String>;
    /// Append audio; `decode` asks for a decode now. Returns the newest partial.
    fn push(&mut self, pcm: &[i16], decode: bool) -> Result<Option<String>, String>;
    /// Transcript covering at least `upto` samples.
    fn snapshot(&mut self, upto: usize) -> Result<String, String>;
    fn close(&mut self);
    /// One-shot transcription of a whole question (used when streaming failed).
    fn transcribe_all(&mut self, pcm: &[i16]) -> Result<String, String>;
}

/// Where the controller's output goes: the overlay + chat streams in the app,
/// a recorder in the latency harness.
pub trait Sink: Send {
    fn emit(&self, event: &str, payload: Value);
    /// Called at question onset: build what `start_answer` needs (LLM config,
    /// job context) off the critical path.
    fn prepare_answer(&self) {}
    fn start_answer(&self, question: &str, stream_id: &str);
    fn cancel_answer(&self, stream_id: &str);
}

struct ActiveQ {
    qid: u64,
    pcm: Vec<i16>,
    asr_ok: bool,
    fired: Option<String>,
    attempts: u32,
    restarts: u32,
    last_partial: String,
}

pub struct Controller<S: Sink, A: Asr> {
    sink: S,
    asr: A,
    tag: String,
    q: Option<ActiveQ>,
    /// Newest answer stream, possibly still running after its question ended;
    /// cancelled when the next answer starts.
    last_stream: Option<String>,
}

impl<S: Sink, A: Asr> Controller<S, A> {
    pub fn new(sink: S, asr: A) -> Self {
        let tag = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        Self { sink, asr, tag, q: None, last_stream: None }
    }

    pub fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Onset { qid } => {
                self.end_question(false);
                self.sink.prepare_answer();
                let asr_ok = match self.asr.start() {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("[copilot-listen] ASR session failed to start, will batch-transcribe: {e}");
                        false
                    }
                };
                self.q = Some(ActiveQ {
                    qid,
                    pcm: Vec::new(),
                    asr_ok,
                    fired: None,
                    attempts: 0,
                    restarts: 0,
                    last_partial: String::new(),
                });
                self.sink.emit("copilot:speech", json!({ "qid": qid }));
            }
            Ev::Audio(pcm) => {
                let Some(q) = self.q.as_mut() else { return };
                q.pcm.extend_from_slice(&pcm);
                if !q.asr_ok {
                    return;
                }
                match self.asr.push(&pcm, false) {
                    Ok(Some(p)) if q.fired.is_none() && !p.is_empty() && p != q.last_partial => {
                        q.last_partial = p.clone();
                        self.sink.emit("copilot:partial", json!({ "qid": q.qid, "text": p }));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[copilot-listen] ASR chunk failed, will batch-transcribe: {e}");
                        q.asr_ok = false;
                    }
                }
            }
            Ev::Pause { .. } => {
                if let Some(q) = self.q.as_mut() {
                    if q.asr_ok {
                        if let Err(e) = self.asr.push(&[], true) {
                            eprintln!("[copilot-listen] ASR decode request failed: {e}");
                            q.asr_ok = false;
                        }
                    }
                }
            }
            Ev::Silence { mark, speech_end } => self.on_silence(mark, speech_end),
            Ev::Resumed => {
                let Some(q) = self.q.as_mut() else { return };
                if let Some(sid) = q.fired.take() {
                    self.sink.cancel_answer(&sid);
                    if self.last_stream.as_deref() == Some(sid.as_str()) {
                        self.last_stream = None;
                    }
                    q.restarts += 1;
                    eprintln!("[copilot-listen] q{} resumed — cancelled {sid}", q.qid);
                    self.sink.emit("copilot:resume", json!({ "qid": q.qid, "streamId": sid }));
                }
            }
            Ev::Commit { .. } => self.end_question(true),
            Ev::Stopped { error } => {
                self.end_question(true);
                self.sink.emit("copilot:stopped", json!({ "error": error }));
            }
        }
    }

    fn on_silence(&mut self, mark: Mark, speech_end: usize) {
        let Some(q) = self.q.as_mut() else { return };
        if q.fired.is_some() {
            return;
        }
        // The speculative endpoint is for the first attempt only; after a
        // restart the answer waits for FINAL so it isn't cancelled again.
        if mark == Mark::Spec && q.restarts > 0 {
            return;
        }
        let t0 = Instant::now();
        let mut text = Err(String::new());
        if q.asr_ok {
            text = self.asr.snapshot(speech_end);
            if let Err(e) = &text {
                eprintln!("[copilot-listen] ASR snapshot failed, batch-transcribing: {e}");
                q.asr_ok = false;
            }
        }
        if text.is_err() {
            text = self.asr.transcribe_all(&q.pcm);
        }
        let asr_ms = t0.elapsed().as_millis() as u64;
        let text = match text {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                eprintln!("[copilot-listen] transcription failed: {e}");
                if matches!(mark, Mark::LongFinal | Mark::Forced) {
                    self.sink.emit("copilot:error", json!({ "message": format!("transcription failed: {e}") }));
                }
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        if matches!(mark, Mark::Spec | Mark::Final) && looks_unfinished(&text) {
            eprintln!("[copilot-listen] q{} {mark:?}: transcript looks unfinished, waiting", q.qid);
            return;
        }
        q.attempts += 1;
        let sid = format!("copilot-{}-{}-{}", self.tag, q.qid, q.attempts);
        if let Some(prev) = self.last_stream.replace(sid.clone()) {
            self.sink.cancel_answer(&prev);
        }
        eprintln!(
            "[copilot-listen] q{} {mark:?} → answer {sid} (asr {asr_ms} ms, {} chars)",
            q.qid,
            text.len()
        );
        q.fired = Some(sid.clone());
        self.sink.emit(
            "copilot:question",
            json!({
                "qid": q.qid, "text": text, "streamId": sid, "attempt": q.attempts,
                "speculative": mark == Mark::Spec, "asrMs": asr_ms,
            }),
        );
        self.sink.start_answer(&text, &sid);
    }

    fn end_question(&mut self, emit: bool) {
        if let Some(q) = self.q.take() {
            if q.asr_ok {
                self.asr.close();
            }
            if emit {
                self.sink.emit("copilot:commit", json!({ "qid": q.qid, "answered": q.fired.is_some() }));
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.end_question(false);
    }
}

// ── App sink: overlay events + the copilot answer as a chat stream ────────────

pub struct AppSink {
    app: tauri::AppHandle,
    base_url: String,
    /// (llm config, job context), built at question onset.
    prepared: Mutex<Option<(Value, String)>>,
}

impl AppSink {
    pub fn new(app: tauri::AppHandle, base_url: String) -> Self {
        Self { app, base_url, prepared: Mutex::new(None) }
    }

    fn build(&self) -> (Value, String) {
        use tauri::Manager;
        // Same as the overlay's typed asks: AI routing for "copilot" + keys
        // from Credential Manager, and the active job's context.
        let llm = crate::ai_routing::llm_for("copilot");
        let context = self.app.state::<crate::copilot::CopilotContextState>().context();
        (llm, context)
    }
}

impl Sink for AppSink {
    fn emit(&self, event: &str, payload: Value) {
        use tauri::Emitter;
        let _ = self.app.emit(event, payload);
    }

    fn prepare_answer(&self) {
        let built = self.build();
        *self.prepared.lock().unwrap() = Some(built);
    }

    fn start_answer(&self, question: &str, stream_id: &str) {
        let prepared = self.prepared.lock().unwrap().clone();
        let (llm, context) = prepared.unwrap_or_else(|| self.build());
        crate::backend_client::stream_chat(
            self.app.clone(),
            self.base_url.clone(),
            question.to_string(),
            context,
            Vec::new(),
            "copilot".to_string(),
            llm,
            Vec::new(),
            stream_id.to_string(),
        );
    }

    fn cancel_answer(&self, stream_id: &str) {
        crate::backend_client::cancel_stream(stream_id);
    }
}

// ── Sidecar ASR client ────────────────────────────────────────────────────────

pub struct HttpAsr {
    client: reqwest::blocking::Client,
    base_url: String,
    session: Option<String>,
}

impl HttpAsr {
    pub fn new(base_url: String) -> Self {
        Self { client: reqwest::blocking::Client::new(), base_url, session: None }
    }

    fn post(&self, path: &str, body: Value, timeout_s: u64) -> Result<Value, String> {
        let v: Value = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(&body)
            .timeout(Duration::from_secs(timeout_s))
            .send()
            .and_then(|r| r.json())
            .map_err(|e| e.to_string())?;
        match v["error"].as_str() {
            Some(e) => Err(e.to_string()),
            None => Ok(v),
        }
    }
}

fn pcm16_b64(pcm: &[i16]) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

impl Asr for HttpAsr {
    fn start(&mut self) -> Result<(), String> {
        self.session = None;
        let v = self.post("/voice/asr/start", json!({}), 5)?;
        let sid = v["session_id"].as_str().filter(|s| !s.is_empty()).ok_or("no session id")?;
        self.session = Some(sid.to_string());
        Ok(())
    }

    fn push(&mut self, pcm: &[i16], decode: bool) -> Result<Option<String>, String> {
        let sid = self.session.clone().ok_or("no ASR session")?;
        let audio = if pcm.is_empty() { String::new() } else { pcm16_b64(pcm) };
        let v = self.post(
            "/voice/asr/chunk",
            json!({ "session_id": sid, "audio_b64": audio, "decode": decode }),
            5,
        )?;
        Ok(v["partial"].as_str().map(str::to_string))
    }

    fn snapshot(&mut self, upto: usize) -> Result<String, String> {
        let sid = self.session.clone().ok_or("no ASR session")?;
        // Generous: a recognizer still loading answers late rather than never.
        let v = self.post("/voice/asr/snapshot", json!({ "session_id": sid, "upto_samples": upto }), 90)?;
        Ok(v["text"].as_str().unwrap_or("").to_string())
    }

    fn close(&mut self) {
        if let Some(sid) = self.session.take() {
            let _ = self.post("/voice/asr/close", json!({ "session_id": sid }), 5);
        }
    }

    fn transcribe_all(&mut self, pcm: &[i16]) -> Result<String, String> {
        use base64::Engine;
        let mut wav = std::io::Cursor::new(Vec::<u8>::new());
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: SR,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::new(&mut wav, spec).map_err(|e| e.to_string())?;
            for &s in pcm {
                w.write_sample(s).map_err(|e| e.to_string())?;
            }
            w.finalize().map_err(|e| e.to_string())?;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(wav.into_inner());
        let v = self.post("/voice/stt", json!({ "audio_b64": b64 }), 90)?;
        Ok(v["text"].as_str().unwrap_or("").to_string())
    }
}

// ── Listener: capture thread + controller thread ──────────────────────────────

/// Runs `frames` (16 kHz mono f32, any packet size) through Silero and the
/// tracker. Shared by the live capture and the latency harness's replay.
pub struct FramePump {
    vad: Silero,
    tracker: Tracker,
    buf: Vec<f32>,
    evs: Vec<Ev>,
    /// A force request waiting for the next complete frame (packets are
    /// ~10 ms, frames 32 ms, so most pushes complete no frame).
    force: bool,
}

impl FramePump {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { vad: Silero::new()?, tracker: Tracker::new(), buf: Vec::new(), evs: Vec::new(), force: false })
    }

    pub fn push(
        &mut self,
        samples16k: &[f32],
        force: bool,
        mut send: impl FnMut(Ev),
    ) -> anyhow::Result<()> {
        self.buf.extend_from_slice(samples16k);
        self.force |= force;
        let mut off = 0;
        while self.buf.len() - off >= FRAME {
            let frame = &self.buf[off..off + FRAME];
            let prob = self.vad.prob(frame)?;
            self.tracker.frame(frame, prob, std::mem::take(&mut self.force), &mut self.evs);
            off += FRAME;
            for ev in self.evs.drain(..) {
                send(ev);
            }
        }
        self.buf.drain(..off);
        Ok(())
    }
}

pub struct Listener {
    stop: Arc<AtomicBool>,
    /// Trip to answer the current question now (hotkey / button).
    pub force: Arc<AtomicBool>,
}

impl Listener {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Start listening to the system audio (the meeting app). Returns once the VAD
/// model is loaded and capture is running, or with the reason it can't.
pub fn spawn_system_listener<S: Sink + 'static>(
    sink: S,
    base_url: String,
    mut on_level: impl FnMut(f32, f32) + Send + 'static,
) -> Result<Listener, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let force = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Ev>();

    // Controller: owns the HTTP calls so the capture thread never blocks.
    std::thread::spawn(move || {
        let mut c = Controller::new(sink, HttpAsr::new(base_url));
        while let Ok(ev) = rx.recv() {
            c.handle(ev);
        }
        c.shutdown();
    });

    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let cap_stop = Arc::clone(&stop);
    let cap_force = Arc::clone(&force);
    std::thread::spawn(move || {
        let mut pump = match FramePump::new() {
            Ok(p) => p,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("{e:#}")));
                return;
            }
        };
        let _ = ready_tx.send(Ok(()));
        let mut resampler: Option<(u32, Resampler16k)> = None;
        let mut out16 = Vec::new();
        let mut failed: Option<String> = None;
        let result = crate::voice_audio::capture_system_continuous(&cap_stop, |mono, sr, _block_ms| {
            let (level, zcr) = crate::voice_audio::level_of(mono);
            on_level(level, zcr);
            if resampler.as_ref().map(|(r, _)| *r) != Some(sr) {
                resampler = Some((sr, Resampler16k::new(sr)));
            }
            out16.clear();
            resampler.as_mut().unwrap().1.push(mono, &mut out16);
            let force = cap_force.swap(false, Ordering::SeqCst);
            if let Err(e) = pump.push(&out16, force, |ev| {
                let _ = tx.send(ev);
            }) {
                failed = Some(format!("{e:#}"));
                return false;
            }
            true
        });
        let error = failed.or_else(|| result.err().map(|e| format!("{e:#}")));
        if let Some(e) = &error {
            eprintln!("[copilot-listen] capture stopped: {e}");
        }
        let _ = tx.send(Ev::Stopped { error });
    });

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(Listener { stop, force }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("VAD did not start in 10 s".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tr: &mut Tracker, prob: f32, frames: usize, force_at: Option<usize>) -> Vec<Ev> {
        let silence = vec![0.0f32; FRAME];
        let mut out = Vec::new();
        for i in 0..frames {
            tr.frame(&silence, prob, force_at == Some(i), &mut out);
        }
        out.retain(|e| !matches!(e, Ev::Audio(_)));
        out
    }

    fn frames(ms: f64) -> usize {
        (ms / FRAME_MS).ceil() as usize
    }

    #[test]
    fn question_fires_spec_then_commits() {
        let mut tr = Tracker::new();
        assert!(run(&mut tr, 0.0, 20, None).is_empty());
        let evs = run(&mut tr, 0.9, frames(1_500.0), None);
        assert_eq!(evs, vec![Ev::Onset { qid: 1 }]);
        let evs = run(&mut tr, 0.05, frames(COMMIT_MS) + 1, None);
        let kinds: Vec<_> = evs
            .iter()
            .map(|e| match e {
                Ev::Pause { .. } => "pause",
                Ev::Silence { mark: Mark::Spec, .. } => "spec",
                Ev::Silence { mark: Mark::Final, .. } => "final",
                Ev::Silence { mark: Mark::LongFinal, .. } => "long",
                Ev::Commit { voiced_ok: true } => "commit",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["pause", "spec", "final", "long", "commit"]);
        assert!(!tr.in_question());
    }

    #[test]
    fn speech_after_spec_resumes_but_short_blip_does_not() {
        let mut tr = Tracker::new();
        run(&mut tr, 0.9, frames(1_000.0), None);
        let evs = run(&mut tr, 0.05, frames(SPEC_MS) + 1, None);
        assert!(evs.iter().any(|e| matches!(e, Ev::Silence { mark: Mark::Spec, .. })));
        // A 2-frame blip: no resume, pause keeps counting.
        assert!(run(&mut tr, 0.9, 2, None).is_empty());
        let evs = run(&mut tr, 0.9, RESUME_FRAMES as usize, None);
        assert_eq!(evs, vec![Ev::Resumed]);
        // Next pause fires Pause + Spec again (the controller decides to skip Spec).
        let evs = run(&mut tr, 0.05, frames(SPEC_MS) + 1, None);
        assert!(evs.iter().any(|e| matches!(e, Ev::Silence { mark: Mark::Spec, .. })));
    }

    #[test]
    fn blip_below_min_voice_is_discarded() {
        let mut tr = Tracker::new();
        run(&mut tr, 0.9, ONSET_FRAMES as usize + 1, None);
        let evs = run(&mut tr, 0.0, frames(COMMIT_MS) + 1, None);
        assert_eq!(evs, vec![Ev::Commit { voiced_ok: false }]);
    }

    #[test]
    fn force_and_cap_fire_forced() {
        let mut tr = Tracker::new();
        let evs = run(&mut tr, 0.9, frames(1_000.0), Some(frames(800.0)));
        assert!(evs.iter().any(|e| matches!(e, Ev::Silence { mark: Mark::Forced, .. })));
        let mut tr = Tracker::new();
        let evs = run(&mut tr, 0.9, frames(MAX_QUESTION_MS) + ONSET_FRAMES as usize + 2, None);
        assert!(evs.iter().any(|e| matches!(e, Ev::Silence { mark: Mark::Forced, .. })));
        assert!(evs.iter().any(|e| matches!(e, Ev::Commit { voiced_ok: true })));
    }

    #[test]
    fn audio_is_preroll_plus_every_sample_until_commit() {
        let mut tr = Tracker::new();
        let mut fed: Vec<i16> = Vec::new();
        let mut audio: Vec<i16> = Vec::new();
        let (mut onset_at, mut commit_at) = (None, None);
        for i in 0..200usize {
            let frame: Vec<f32> = (0..FRAME).map(|k| ((i * FRAME + k) % 20_000) as f32 / 32_768.0).collect();
            fed.extend(frame.iter().map(|s| (s * 32767.0) as i16));
            let prob = if (20..120).contains(&i) { 0.9 } else { 0.0 };
            let mut out = Vec::new();
            tr.frame(&frame, prob, false, &mut out);
            for ev in out {
                match ev {
                    Ev::Audio(a) => audio.extend(a),
                    Ev::Onset { .. } => onset_at = Some(i),
                    Ev::Commit { .. } => commit_at = Some(i),
                    _ => {}
                }
            }
        }
        let (onset_at, commit_at) = (onset_at.unwrap(), commit_at.unwrap());
        assert_eq!(onset_at, 20 + ONSET_FRAMES as usize - 1);
        let preroll = (SR as f64 * PREROLL_MS / 1000.0) as usize;
        let start = (onset_at + 1) * FRAME - preroll;
        assert_eq!(audio, fed[start..(commit_at + 1) * FRAME]);
    }

    #[test]
    fn terminal_punctuation_never_requests_continuation() {
        assert!(!looks_unfinished("What are you passionate about?"));
        assert!(!looks_unfinished("Tell me about your last role."));
        assert!(!looks_unfinished("Why this company!"));
    }

    #[test]
    fn clause_openers_without_terminal_punctuation_request_continuation() {
        assert!(looks_unfinished("Tell me about"));
        assert!(looks_unfinished("What would you do if"));
        assert!(looks_unfinished("The main reason is,"));
    }

    #[test]
    fn complete_plain_text_does_not_request_continuation() {
        assert!(!looks_unfinished("Describe your most successful project"));
        assert!(!looks_unfinished("How did you measure success"));
    }

    // Controller with fakes: speculative answer, resume cancels, restart waits for Final.
    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<String>>>);
    impl Sink for Rec {
        fn emit(&self, event: &str, payload: Value) {
            self.0.lock().unwrap().push(format!("{event} {}", payload["text"].as_str().unwrap_or("")).trim().to_string());
        }
        fn start_answer(&self, q: &str, _sid: &str) {
            self.0.lock().unwrap().push(format!("start {q}"));
        }
        fn cancel_answer(&self, _sid: &str) {
            self.0.lock().unwrap().push("cancel".into());
        }
    }
    struct FakeAsr(Vec<&'static str>);
    impl Asr for FakeAsr {
        fn start(&mut self) -> Result<(), String> { Ok(()) }
        fn push(&mut self, _: &[i16], _: bool) -> Result<Option<String>, String> { Ok(None) }
        fn snapshot(&mut self, _: usize) -> Result<String, String> { Ok(self.0.remove(0).to_string()) }
        fn close(&mut self) {}
        fn transcribe_all(&mut self, _: &[i16]) -> Result<String, String> { Err("unused".into()) }
    }

    #[test]
    fn controller_speculates_cancels_and_restarts_on_final() {
        let rec = Rec::default();
        let mut c = Controller::new(rec.clone(), FakeAsr(vec!["Tell me about a time", "Tell me about a time you failed."]));
        c.handle(Ev::Onset { qid: 1 });
        c.handle(Ev::Silence { mark: Mark::Spec, speech_end: 100 });
        c.handle(Ev::Resumed);
        c.handle(Ev::Silence { mark: Mark::Spec, speech_end: 200 }); // skipped after a restart
        c.handle(Ev::Silence { mark: Mark::Final, speech_end: 200 });
        c.handle(Ev::Commit { voiced_ok: true });
        let log = rec.0.lock().unwrap().clone();
        assert_eq!(
            log,
            [
                "copilot:speech",
                "copilot:question Tell me about a time",
                "start Tell me about a time",
                "cancel",
                "copilot:resume",
                "copilot:question Tell me about a time you failed.",
                "start Tell me about a time you failed.",
                "copilot:commit",
            ]
        );
    }

    #[test]
    fn controller_waits_out_an_unfinished_transcript() {
        let rec = Rec::default();
        let mut c = Controller::new(rec.clone(), FakeAsr(vec!["What would you do if", "What would you do if"]));
        c.handle(Ev::Onset { qid: 1 });
        c.handle(Ev::Silence { mark: Mark::Spec, speech_end: 100 });
        c.handle(Ev::Silence { mark: Mark::LongFinal, speech_end: 100 });
        let log = rec.0.lock().unwrap().clone();
        assert_eq!(log, ["copilot:speech", "copilot:question What would you do if", "start What would you do if"]);
    }
}
