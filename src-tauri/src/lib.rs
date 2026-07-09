//! InterPrep — Tauri runtime entry point.

mod backend_client;
mod copilot;
mod credentials;
mod jobs_store;
mod recorder;
mod resume_store;
mod sidecar;
mod types;
mod voice_audio;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use credentials::Credentials;
use resume_store::Resume;
use sidecar::PythonSidecar;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use types::Job;

// ─── State ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct SidecarState {
    inner: Mutex<SidecarInner>,
}

#[derive(Default)]
struct SidecarInner {
    sidecar:  Option<PythonSidecar>,
    base_url: Option<String>,
    last_err: Option<String>,
    /// Bridge shared secret + port, mirrored from the sidecar so commands can
    /// hand a pairing code to the frontend without re-reading the process.
    token:    Option<String>,
    port:     Option<u16>,
}

/// Generate a 256-bit hex token from the OS CSPRNG. `getrandom` is backed by
/// `BCryptGenRandom` on Windows, so the 32 bytes are full-entropy and mutually
/// independent. (Chaining `RandomState::new().finish()` does NOT give 256 bits:
/// std advances a thread's hasher seed by 1 between calls, so successive outputs
/// are correlated and the real entropy is only the single ~128-bit seed.)
fn gen_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS CSPRNG unavailable");
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[derive(Default)]
struct RecorderState {
    inner: Mutex<Option<recorder::RecordingSession>>,
}

/// Queue item for the serialized TTS playback worker.
enum TtsMsg {
    /// One chunk (typically a sentence) plus the panelist voice to speak it in
    /// (`speaker` is used only by the "vibe-rt" engine; Piper ignores it).
    Speak { text: String, speaker: String },
    Flush, // end of one utterance → emit `voice:speak_done`
}

/// Voice turn state. A single worker thread plays queued TTS chunks in order
/// (so streamed sentences don't overlap). `interrupt` aborts the current
/// chunk; `abort` drains the rest of the queue until the next `Flush`;
/// `listen_stop` ends mic capture early.
#[derive(Default)]
struct VoiceState {
    speak_tx: Mutex<Option<std::sync::mpsc::Sender<TtsMsg>>>,
    interrupt: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    abort: Arc<AtomicBool>,
    /// Barge-in (interrupt the AI by talking) only works with headphones —
    /// on speakers the mic hears the AI's own voice. Off by default.
    barge_enabled: Arc<AtomicBool>,
    /// TTS engine for the next utterance: "vibe-rt" = VibeVoice (humanlike),
    /// anything else (incl. empty) = Piper (fast default).
    engine: Arc<Mutex<String>>,
    listen_stop: Mutex<Option<Arc<AtomicBool>>>,
    /// Manual "transcribe now" trigger for the active capture. Distinct from
    /// `listen_stop` (which cancels/discards): tripping this ends capture AND
    /// keeps the audio for transcription. Set by `voice_finish_listening` (button)
    /// and the finish hotkey.
    listen_finish: Mutex<Option<Arc<AtomicBool>>>,
}

// ─── Jobs ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn list_jobs() -> Vec<Job> {
    jobs_store::load()
}

#[tauri::command]
fn save_jobs(jobs: Vec<Job>) -> Result<(), String> {
    jobs_store::save(&jobs)
}

// ─── Credentials ──────────────────────────────────────────────────────────

#[tauri::command]
fn load_credentials() -> Credentials {
    Credentials::load()
}

#[tauri::command]
fn save_credentials(credentials: Credentials) -> Result<(), String> {
    credentials.save()
}

// ─── Resumes ──────────────────────────────────────────────────────────────

#[tauri::command]
fn list_resumes() -> Vec<Resume> {
    resume_store::load()
}

#[tauri::command]
fn save_resumes(resumes: Vec<Resume>) -> Result<(), String> {
    resume_store::save(&resumes)
}

// ─── Backend lifecycle ────────────────────────────────────────────────────

#[tauri::command]
fn backend_status(state: State<SidecarState>) -> serde_json::Value {
    let inner = state.inner.lock().unwrap();
    if inner.base_url.is_some() {
        serde_json::json!({ "status": "ready", "url": inner.base_url })
    } else if let Some(err) = &inner.last_err {
        serde_json::json!({ "status": "failed", "error": err })
    } else {
        serde_json::json!({ "status": "starting" })
    }
}

// ─── Extension bridge ──────────────────────────────────────────────────────

/// Pairing info for the browser extension: the bridge port + shared secret, and
/// a single copy-paste `pairingCode` (base64 of "port:token"). Returns
/// `ready: false` until the sidecar is up. The token is a localhost-only secret
/// — surfacing it to our own frontend is fine; it never leaves the machine.
#[tauri::command]
fn bridge_info(state: State<SidecarState>) -> serde_json::Value {
    use base64::Engine;
    let inner = state.inner.lock().unwrap();
    match (&inner.port, &inner.token) {
        (Some(port), Some(token)) => {
            let code = base64::engine::general_purpose::STANDARD
                .encode(format!("{port}:{token}"));
            serde_json::json!({
                "ready": true,
                "port": port,
                "token": token,
                "pairingCode": code,
            })
        }
        _ => serde_json::json!({ "ready": false }),
    }
}

/// Push the currently-stored LLM config (provider toggle + keys) to the
/// running sidecar. Call after the user saves credentials so the extension
/// keeps working without an app restart.
#[tauri::command]
fn reseed_backend_key(state: State<SidecarState>) -> Result<(), String> {
    let (url, token) = {
        let inner = state.inner.lock().unwrap();
        match (inner.base_url.clone(), inner.token.clone()) {
            (Some(u), Some(t)) => (u, t),
            _ => return Err("Backend is not ready yet".to_string()),
        }
    };
    let llm = Credentials::load().llm_json();
    backend_client::seed_config(&url, &token, &llm)
}

/// Poll the extension's job-capture inbox. Returns the pending captures (JDs the
/// user grabbed from a browser); the frontend turns each into a job + research
/// run, then calls `ack_job_inbox`.
#[tauri::command]
fn poll_job_inbox(state: State<SidecarState>) -> Result<serde_json::Value, String> {
    let (url, token) = bridge_url_token(&state)?;
    backend_client::inbox_pending(&url, &token)
}

/// Acknowledge consumed inbox captures so they aren't processed again.
#[tauri::command]
fn ack_job_inbox(state: State<SidecarState>, ids: Vec<String>) -> Result<(), String> {
    let (url, token) = bridge_url_token(&state)?;
    backend_client::inbox_ack(&url, &token, ids)
}

/// Poll the extension's timeline-event queue. Returns "mark Applied + note"
/// events the extension logged after an autofill; the frontend applies each to
/// the job's stage timeline, then calls `ack_timeline_inbox`.
#[tauri::command]
fn poll_timeline_inbox(state: State<SidecarState>) -> Result<serde_json::Value, String> {
    let (url, token) = bridge_url_token(&state)?;
    backend_client::timeline_pending(&url, &token)
}

/// Acknowledge applied timeline events so they aren't applied again.
#[tauri::command]
fn ack_timeline_inbox(state: State<SidecarState>, ids: Vec<String>) -> Result<(), String> {
    let (url, token) = bridge_url_token(&state)?;
    backend_client::timeline_ack(&url, &token, ids)
}

fn bridge_url_token(state: &State<SidecarState>) -> Result<(String, String), String> {
    let inner = state.inner.lock().unwrap();
    match (inner.base_url.clone(), inner.token.clone()) {
        (Some(u), Some(t)) => Ok((u, t)),
        _ => Err("Backend is not ready yet".to_string()),
    }
}

// ─── Streams (chat / research / company research) ─────────────────────────

fn require_url(state: &State<SidecarState>) -> Result<String, String> {
    state
        .inner
        .lock()
        .unwrap()
        .base_url
        .clone()
        .ok_or_else(|| "Backend is not ready yet".to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_chat_stream(
    app: AppHandle,
    state: State<SidecarState>,
    message: String,
    job_context: String,
    history: Vec<(String, String)>,
    mode: String,
    llm: serde_json::Value,
    documents: Vec<backend_client::RagDocPayload>,
    stream_id: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_chat(app, url, message, job_context, history, mode, llm, documents, stream_id);
    Ok(())
}

#[tauri::command]
fn start_research_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    job_description: String,
    llm: serde_json::Value,
    stream_id: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_research(app, url, company, role, job_description, llm, stream_id);
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_company_research_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    llm: serde_json::Value,
    stream_id: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_company_research(
        app,
        url,
        company,
        role,
        location,
        job_description,
        tailored_resume,
        llm,
        stream_id,
    );
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_application_tailor_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    location: String,
    job_description: String,
    master_resumes: Vec<backend_client::MasterResumePayload>,
    llm: serde_json::Value,
    stream_id: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_application_tailor(
        app,
        url,
        company,
        role,
        location,
        job_description,
        master_resumes,
        llm,
        stream_id,
    );
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_knockout_screen_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    llm: serde_json::Value,
    stream_id: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_knockout_screen(
        app,
        url,
        company,
        role,
        location,
        job_description,
        tailored_resume,
        llm,
        stream_id,
    );
    Ok(())
}

// ─── Interview cheatsheet ─────────────────────────────────────────────────

/// Build/refresh a job's interview cheatsheet from its aggregated context
/// (`payload` carries company/role/JD + resume + research dossier +
/// conversation docs + the previous cheatsheet markdown + the LLM config).
/// Returns the structured cheatsheet JSON for the frontend to store on the Job.
#[tauri::command]
fn generate_cheatsheet(
    state: State<SidecarState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = require_url(&state)?;
    backend_client::build_cheatsheet(&url, payload)
}

/// Persist the cheatsheet markdown as the job's living `.md` file at
/// `%APPDATA%/InterPrep/applications/<job_id>/cheatsheet.md`. Returns the path.
#[tauri::command]
fn save_cheatsheet_md(job_id: String, markdown: String) -> Result<String, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| "no AppData directory available".to_string())?
        .join("InterPrep")
        .join("applications")
        .join(&job_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let path = dir.join("cheatsheet.md");
    std::fs::write(&path, markdown.as_bytes()).map_err(|e| format!("write failed: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

// ─── Generated docx persistence ───────────────────────────────────────────

/// Decode the base64 `.docx` bytes emitted by `chat:resume_docx` and write
/// them to `%APPDATA%/InterPrep/applications/<job_id>/resume.docx`. Returns
/// the absolute path so the frontend can persist it on the Job.
#[tauri::command]
fn save_resume_docx(job_id: String, b64: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    let dir = dirs::data_dir()
        .ok_or_else(|| "no AppData directory available".to_string())?
        .join("InterPrep")
        .join("applications")
        .join(&job_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let path = dir.join("resume.docx");
    std::fs::write(&path, &bytes).map_err(|e| format!("write failed: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Open an arbitrary path in the OS default app (Word, browser, etc.).
/// Used by the "Open Tailored Resume" button.
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    opener::open(&path).map_err(|e| format!("open failed: {e}"))
}

// ─── Audio recording ──────────────────────────────────────────────────────

#[tauri::command]
fn list_audio_devices() -> Result<serde_json::Value, String> {
    let catalog =
        recorder::query_device_catalog().map_err(|e| format!("device query failed: {e}"))?;
    let to_json = |entries: &[recorder::AudioDeviceEntry]| -> Vec<serde_json::Value> {
        entries
            .iter()
            .map(|d| serde_json::json!({ "name": d.name, "isDefault": d.is_default }))
            .collect()
    };
    Ok(serde_json::json!({
        "render":  to_json(&catalog.render_devices),
        "capture": to_json(&catalog.capture_devices),
    }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRecordingArgs {
    /// Directory where the WAV files will be written. Defaults to
    /// `%USERPROFILE%\InterPrep Recordings` if blank.
    #[serde(default)]
    out_dir: String,
    /// Override the default playback device. Blank = Windows default.
    #[serde(default)]
    system_device: String,
    /// Override the default microphone. Blank = Windows default.
    #[serde(default)]
    mic_device: String,
    /// File names. Defaults to `system_audio.wav` / `microphone.wav`.
    #[serde(default)]
    system_file: String,
    #[serde(default)]
    mic_file: String,
}

#[tauri::command]
fn start_recording(
    state: State<RecorderState>,
    args: StartRecordingArgs,
) -> Result<serde_json::Value, String> {
    let mut slot = state.inner.lock().unwrap();
    if slot.is_some() {
        return Err("A recording is already in progress".into());
    }

    let out_dir = if args.out_dir.is_empty() {
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("InterPrep Recordings")
    } else {
        PathBuf::from(&args.out_dir)
    };

    let config = recorder::RecorderConfig {
        system_device_name: opt(args.system_device),
        mic_device_name:    opt(args.mic_device),
        output_dir:         out_dir.clone(),
        system_file:        non_empty(args.system_file, "system_audio.wav"),
        mic_file:           non_empty(args.mic_file,    "microphone.wav"),
        poll_timeout_ms:    recorder::DEFAULT_POLL_TIMEOUT_MS,
    };

    let session = recorder::RecordingSession::start(config)
        .map_err(|e| format!("start failed: {e:#}"))?;
    *slot = Some(session);

    Ok(serde_json::json!({
        "outDir":     out_dir,
        "systemFile": "system_audio.wav",
        "micFile":    "microphone.wav",
    }))
}

#[tauri::command]
fn stop_recording(state: State<RecorderState>) -> Result<serde_json::Value, String> {
    let session = state
        .inner
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "No active recording".to_string())?;
    session.request_stop();
    let report = session.finish().map_err(|e| format!("stop failed: {e:#}"))?;
    Ok(serde_json::json!({
        "system": capture_to_json(&report.system),
        "mic":    capture_to_json(&report.microphone),
    }))
}

fn capture_to_json(c: &recorder::CaptureSummary) -> serde_json::Value {
    serde_json::json!({
        "path":            c.output_path,
        "deviceName":      c.device_name,
        "sampleRate":      c.sample_rate,
        "channels":        c.channels,
        "framesWritten":   c.frames_written,
        "durationSeconds": c.duration_seconds(),
    })
}

// ─── Voice (live mock interview) ──────────────────────────────────────────

/// Capability + device (cuda/cpu) report from the sidecar.
#[tauri::command]
fn voice_status(state: State<SidecarState>) -> Result<serde_json::Value, String> {
    let url = require_url(&state)?;
    backend_client::voice_status(&url)
}

/// Item handed from the synth stage to the playback stage.
enum PlayItem {
    /// A streaming sentence: the player pulls decoded mono-f32 PCM blocks off
    /// `pcm` and plays them as they arrive (synth runs slower than real-time,
    /// so streaming is what gets audio out fast).
    Stream {
        sample_rate: u32,
        pcm: std::sync::mpsc::Receiver<Vec<f32>>,
    },
    Flush, // end of one utterance → emit `voice:speak_done`
}

/// TTS synthesis stage. Pulls chunks off the queue, synthesizes each WAV, and
/// hands it to the playback stage through a capacity-1 channel. The bound means
/// we synthesize at most ONE chunk ahead of playback — so sentence N+1 is being
/// generated while sentence N plays (hiding synth latency behind audio), without
/// racing ahead and wasting work that a barge-in would throw away.
fn voice_worker(
    app: AppHandle,
    url: String,
    rx: std::sync::mpsc::Receiver<TtsMsg>,
    interrupt_slot: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    abort: Arc<AtomicBool>,
    barge_enabled: Arc<AtomicBool>,
    engine: Arc<Mutex<String>>,
) {
    let (ptx, prx) = std::sync::mpsc::sync_channel::<PlayItem>(1);
    {
        let app = app.clone();
        let slot = Arc::clone(&interrupt_slot);
        let abort = Arc::clone(&abort);
        let barge = Arc::clone(&barge_enabled);
        std::thread::spawn(move || play_worker(app, prx, slot, abort, barge));
    }

    while let Ok(msg) = rx.recv() {
        match msg {
            TtsMsg::Flush => {
                if ptx.send(PlayItem::Flush).is_err() {
                    break; // playback stage gone
                }
            }
            TtsMsg::Speak { text, speaker } => {
                if abort.load(Ordering::SeqCst) {
                    continue; // draining the rest of an aborted utterance
                }
                // Read the engine fresh each utterance so a Settings toggle
                // mid-interview applies to the next reply. `speaker` rides with
                // the chunk so a panel can switch voices turn to turn.
                let engine = engine.lock().unwrap().clone();
                let (sample_rate, mut resp) = match backend_client::voice_tts_stream(&url, &text, &engine, &speaker) {
                    Ok(v) => v,
                    Err(e) => { let _ = app.emit("voice:error", e); continue; }
                };
                if abort.load(Ordering::SeqCst) {
                    continue;
                }
                // Hand the player a receiver to start draining immediately, then
                // pump decoded PCM into it as the HTTP body streams in. The bound
                // back-pressures synth if the player ever falls behind.
                let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);
                if ptx.send(PlayItem::Stream { sample_rate, pcm: pcm_rx }).is_err() {
                    break;
                }
                use std::io::Read;
                let mut buf = [0u8; 8192];
                let mut leftover: Vec<u8> = Vec::new(); // holds a trailing odd byte
                loop {
                    if abort.load(Ordering::SeqCst) {
                        break;
                    }
                    let n = match resp.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    leftover.extend_from_slice(&buf[..n]);
                    let pairs = leftover.len() / 2;
                    if pairs == 0 {
                        continue;
                    }
                    let mut block = Vec::with_capacity(pairs);
                    for i in 0..pairs {
                        let s = i16::from_le_bytes([leftover[2 * i], leftover[2 * i + 1]]);
                        block.push(s as f32 / 32768.0);
                    }
                    // Carry a leftover odd byte into the next read.
                    if leftover.len() % 2 == 1 {
                        let last = leftover[leftover.len() - 1];
                        leftover.clear();
                        leftover.push(last);
                    } else {
                        leftover.clear();
                    }
                    // Player dropped the receiver (interrupt/stop) → stop reading.
                    if pcm_tx.send(block).is_err() {
                        break;
                    }
                }
                // Dropping pcm_tx signals end-of-sentence to the player.
            }
        }
    }
}

/// TTS playback stage. Plays each streamed sentence in order (so they never
/// overlap), runs a barge-in monitor during playback, and emits
/// `voice:speak_done` on each `Flush`.
fn play_worker(
    app: AppHandle,
    rx: std::sync::mpsc::Receiver<PlayItem>,
    interrupt_slot: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    abort: Arc<AtomicBool>,
    barge_enabled: Arc<AtomicBool>,
) {
    while let Ok(item) = rx.recv() {
        match item {
            PlayItem::Flush => {
                // `abort` is set if a barge-in/stop cut this utterance short.
                let interrupted = abort.swap(false, Ordering::SeqCst);
                let _ = app.emit("voice:speak_done", serde_json::json!({ "interrupted": interrupted }));
            }
            PlayItem::Stream { sample_rate, pcm } => {
                if abort.load(Ordering::SeqCst) {
                    continue; // draining the rest of an aborted utterance (drops pcm)
                }

                let interrupt = Arc::new(AtomicBool::new(false));
                *interrupt_slot.lock().unwrap() = Some(Arc::clone(&interrupt));

                // Barge-in monitor for this chunk — only with headphones (else
                // the mic hears the AI's own playback and false-triggers).
                let detected = Arc::new(AtomicBool::new(false));
                let monitor_stop = Arc::new(AtomicBool::new(false));
                let monitoring = barge_enabled.load(Ordering::SeqCst);
                if monitoring {
                    let interrupt = Arc::clone(&interrupt);
                    let detected = Arc::clone(&detected);
                    let monitor_stop = Arc::clone(&monitor_stop);
                    let app = app.clone();
                    std::thread::spawn(move || {
                        let _ = voice_audio::wait_for_speech(&monitor_stop, &detected);
                        if detected.load(Ordering::SeqCst) {
                            interrupt.store(true, Ordering::SeqCst);
                            let _ = app.emit("voice:barge_in", ());
                        }
                    });
                }

                let app_lvl = app.clone();
                let mut last = Instant::now() - Duration::from_millis(100);
                let mut on_level = move |rms: f32, zcr: f32| {
                    let now = Instant::now();
                    if now.duration_since(last) >= Duration::from_millis(45) {
                        last = now;
                        let _ = app_lvl.emit("voice:level", serde_json::json!({ "level": rms, "pitch": zcr, "mode": "speaking" }));
                    }
                };
                let played = voice_audio::play_pcm_stream(pcm, sample_rate, &interrupt, &mut on_level).unwrap_or(false);
                let _ = app.emit("voice:level", serde_json::json!({ "level": 0.0, "pitch": 0.0, "mode": "speaking" }));
                monitor_stop.store(true, Ordering::SeqCst);
                *interrupt_slot.lock().unwrap() = None;
                if detected.load(Ordering::SeqCst) || !played {
                    abort.store(true, Ordering::SeqCst); // drop the rest of this utterance
                }
            }
        }
    }
}

/// Lazily start the playback worker, returning its queue sender.
fn ensure_voice_worker(
    app: &AppHandle,
    url: String,
    voice: &State<VoiceState>,
) -> std::sync::mpsc::Sender<TtsMsg> {
    let mut guard = voice.speak_tx.lock().unwrap();
    if let Some(tx) = guard.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    let slot = Arc::clone(&voice.interrupt);
    let abort = Arc::clone(&voice.abort);
    let barge = Arc::clone(&voice.barge_enabled);
    let engine = Arc::clone(&voice.engine);
    std::thread::spawn(move || voice_worker(app2, url, rx, slot, abort, barge, engine));
    *guard = Some(tx.clone());
    tx
}

/// Enqueue a chunk (typically one sentence) of the interviewer's reply for
/// playback. Chunks play in order as they stream in from the model.
#[tauri::command]
fn voice_speak_chunk(
    app: AppHandle,
    sidecar: State<SidecarState>,
    voice: State<VoiceState>,
    text: String,
    speaker: Option<String>,
) -> Result<(), String> {
    let url = require_url(&sidecar)?;
    // A fresh utterance is starting — clear any stale abort from a prior turn.
    voice.abort.store(false, Ordering::SeqCst);
    let tx = ensure_voice_worker(&app, url, &voice);
    let _ = tx.send(TtsMsg::Speak { text, speaker: speaker.unwrap_or_default() });
    Ok(())
}

/// Mark the end of the interviewer's reply. The worker emits `voice:speak_done`
/// once it finishes everything queued before this.
#[tauri::command]
fn voice_speak_flush(
    app: AppHandle,
    sidecar: State<SidecarState>,
    voice: State<VoiceState>,
) -> Result<(), String> {
    let url = require_url(&sidecar)?;
    let tx = ensure_voice_worker(&app, url, &voice);
    let _ = tx.send(TtsMsg::Flush);
    Ok(())
}

/// Enable/disable barge-in (interrupt the AI by speaking). Should only be on
/// with headphones — otherwise the mic hears the AI and cuts it off.
#[tauri::command]
fn voice_set_barge(voice: State<VoiceState>, enabled: bool) {
    voice.barge_enabled.store(enabled, Ordering::SeqCst);
}

/// Select the TTS engine for the interview voice: "vibe-rt" = VibeVoice
/// (humanlike), anything else = Piper (fast default). Takes effect on the next
/// spoken utterance.
#[tauri::command]
fn voice_set_engine(voice: State<VoiceState>, engine: String) {
    *voice.engine.lock().unwrap() = engine;
}

/// Warm up an engine's cold start off the critical path. vibe-rt's first load +
/// first synth take ~2.5 min on a laptop GPU; calling this when the user enables
/// the humanlike voice moves that cost off the first interview question. Fire-
/// and-forget: spawns a thread so the UI isn't blocked, and ignores errors (the
/// sidecar may not be up yet / the engine may not be installed).
#[tauri::command]
fn voice_warm(sidecar: State<SidecarState>, engine: String, speaker: Option<String>) {
    let Ok(url) = require_url(&sidecar) else { return };
    let speaker = speaker.unwrap_or_default();
    std::thread::spawn(move || {
        let _ = backend_client::voice_warm(&url, &engine, &speaker);
    });
}

/// Warm STT + the chosen TTS engine and BLOCK until ready, returning the
/// sidecar's readiness report. Called when the user starts a mock interview so
/// the cold start lands behind the "Preparing engine…" modal instead of at app
/// startup. Async + `spawn_blocking` so the ~30s wait runs off the main thread
/// and never freezes the UI.
#[tauri::command]
async fn voice_prepare(
    sidecar: State<'_, SidecarState>,
    engine: String,
    speaker: Option<String>,
) -> Result<serde_json::Value, String> {
    let url = require_url(&sidecar)?;
    let speaker = speaker.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        backend_client::voice_prepare(&url, &engine, &speaker)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Abort current playback + drain the rest of the queued utterance.
#[tauri::command]
fn voice_interrupt(voice: State<VoiceState>) {
    voice.abort.store(true, Ordering::SeqCst);
    if let Some(flag) = voice.interrupt.lock().unwrap().as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Capture one spoken answer (VAD-endpointed), transcribe it (faster-whisper),
/// and emit `voice:transcript` with the text ("" if nothing was said). Emits
/// `voice:listening` when the mic opens.
#[tauri::command]
fn voice_listen(
    app: AppHandle,
    sidecar: State<SidecarState>,
    voice: State<VoiceState>,
) -> Result<(), String> {
    let url = require_url(&sidecar)?;
    let stop = Arc::new(AtomicBool::new(false));
    let finish = Arc::new(AtomicBool::new(false));
    *voice.listen_stop.lock().unwrap() = Some(Arc::clone(&stop));
    *voice.listen_finish.lock().unwrap() = Some(Arc::clone(&finish));

    std::thread::spawn(move || {
        let _ = app.emit("voice:listening", ());
        let app_lvl = app.clone();
        let mut last = Instant::now() - Duration::from_millis(100);
        let mut on_level = move |rms: f32, zcr: f32| {
            let now = Instant::now();
            if now.duration_since(last) >= Duration::from_millis(45) {
                last = now;
                let _ = app_lvl.emit("voice:level", serde_json::json!({ "level": rms, "pitch": zcr, "mode": "listening" }));
            }
        };
        // Model-based endpointing: verdicts come from the sidecar's Silero VAD;
        // record_answer falls back to energy heuristics if it's unreachable.
        let vad = voice_audio::VadClient::start(url.clone());
        let result = voice_audio::record_answer(&stop, &finish, Some(&vad), &mut on_level);
        let _ = app.emit("voice:level", serde_json::json!({ "level": 0.0, "pitch": 0.0, "mode": "listening" }));
        match result {
            Ok(Some(wav)) => {
                use base64::Engine;
                // Capture ended; STT can take a while on CPU. Tell the UI so it
                // can show a "transcribing" status instead of looking stuck.
                let _ = app.emit("voice:transcribing", ());
                let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
                match backend_client::voice_stt(&url, &b64) {
                    Ok(text) => { let _ = app.emit("voice:transcript", text); }
                    Err(e) => {
                        let _ = app.emit("voice:error", e);
                        let _ = app.emit("voice:transcript", String::new());
                    }
                }
            }
            Ok(None) => { let _ = app.emit("voice:transcript", String::new()); }
            Err(e) => {
                let _ = app.emit("voice:error", format!("listen failed: {e:#}"));
                let _ = app.emit("voice:transcript", String::new());
            }
        }
    });

    Ok(())
}

/// Capture one interviewer question off the SYSTEM AUDIO loopback (what's
/// playing through the speakers — the meeting app), VAD-endpointed with a
/// noise-floor-adaptive detector biased against false silence. Transcribes it
/// and emits `voice:transcript` (""=nothing heard). Same event surface as
/// `voice_listen`, so the copilot's listen loop is source-agnostic. Used by the
/// copilot Rec button to answer questions as they're asked.
#[tauri::command]
fn voice_listen_system(
    app: AppHandle,
    sidecar: State<SidecarState>,
    voice: State<VoiceState>,
) -> Result<(), String> {
    let url = require_url(&sidecar)?;
    let stop = Arc::new(AtomicBool::new(false));
    let finish = Arc::new(AtomicBool::new(false));
    *voice.listen_stop.lock().unwrap() = Some(Arc::clone(&stop));
    *voice.listen_finish.lock().unwrap() = Some(Arc::clone(&finish));

    std::thread::spawn(move || {
        let _ = app.emit("voice:listening", ());
        let app_lvl = app.clone();
        let mut last = Instant::now() - Duration::from_millis(100);
        let mut on_level = move |rms: f32, zcr: f32| {
            let now = Instant::now();
            if now.duration_since(last) >= Duration::from_millis(45) {
                last = now;
                let _ = app_lvl.emit("voice:level", serde_json::json!({ "level": rms, "pitch": zcr, "mode": "listening" }));
            }
        };
        // Model-based endpointing: verdicts come from the sidecar's Silero VAD;
        // record_question falls back to energy heuristics if it's unreachable.
        let vad = voice_audio::VadClient::start(url.clone());
        use base64::Engine;
        // Semantic continuation: whisper punctuates its transcripts, so a
        // segment ending WITHOUT terminal punctuation usually means the VAD
        // endpointed on a mid-question think-pause. In that case listen again
        // briefly for the rest and stitch the segments — the answer is only
        // drafted on a question that reads complete (or after 3 segments).
        let mut full_text = String::new();
        let mut timeout = voice_audio::Q_NO_SPEECH_TIMEOUT_S;
        for _segment in 0..3 {
            let result =
                voice_audio::record_question(&stop, &finish, Some(&vad), timeout, &mut on_level);
            match result {
                Ok(Some(wav)) => {
                    // Capture ended; STT can take a while on CPU. Tell the UI
                    // so it shows "transcribing" instead of looking stuck.
                    let _ = app.emit("voice:transcribing", ());
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
                    match backend_client::voice_stt(&url, &b64) {
                        Ok(text) => {
                            let t = text.trim().to_string();
                            if !t.is_empty() {
                                if !full_text.is_empty() {
                                    full_text.push(' ');
                                }
                                full_text.push_str(&t);
                            }
                        }
                        Err(e) => {
                            let _ = app.emit("voice:error", e);
                            break;
                        }
                    }
                    // Manual "transcribe now" — the user forced the end; never
                    // re-listen against their explicit intent.
                    if finish.load(Ordering::SeqCst) {
                        break;
                    }
                    // Only re-listen on STRONG "mid-sentence" evidence. Whisper
                    // regularly omits the trailing "?" on questions, so merely
                    // missing terminal punctuation must NOT trigger the ~2.8s
                    // continuation wait on every complete question — only a cut
                    // that ends on a comma or a clause-opening word does.
                    if !looks_unfinished(&full_text) {
                        break;
                    }
                    eprintln!("[voice] transcript looks unfinished — listening for the rest");
                    let _ = app.emit("voice:listening", ());
                    timeout = voice_audio::Q_CONTINUATION_TIMEOUT_S;
                }
                Ok(None) => break, // silence / cancel — nothing more coming
                Err(e) => {
                    let _ = app.emit("voice:error", format!("system listen failed: {e:#}"));
                    break;
                }
            }
        }
        let _ = app.emit("voice:level", serde_json::json!({ "level": 0.0, "pitch": 0.0, "mode": "listening" }));
        // A cancel (stop flag) mid-flow discards everything — the user backed out.
        if stop.load(Ordering::SeqCst) {
            full_text.clear();
        }
        let _ = app.emit("voice:transcript", full_text);
    });

    Ok(())
}

/// Force-end mic capture early (user pressed stop / disabled voice). The audio
/// captured so far is DISCARDED — use `voice_finish_listening` to end + transcribe.
#[tauri::command]
fn voice_stop_listening(voice: State<VoiceState>) {
    if let Some(flag) = voice.listen_stop.lock().unwrap().as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Manually end the active capture AND transcribe what was captured — the user's
/// override for imperfect silence detection ("transcribe now" button / hotkey).
/// No-op if nothing is listening.
#[tauri::command]
fn voice_finish_listening(voice: State<VoiceState>) {
    if let Some(flag) = voice.listen_finish.lock().unwrap().as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Strong "the speaker was cut off mid-sentence" heuristic for the semantic
/// continuation listen. Deliberately conservative: false positives cost ~2.8s
/// on every question, false negatives just mean one question gets answered
/// from its first clause — so only a trailing comma or an obviously
/// clause-opening final word counts.
fn looks_unfinished(text: &str) -> bool {
    let t = text.trim_end();
    if t.is_empty() {
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

fn opt(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn non_empty(s: String, fallback: &str) -> String {
    if s.trim().is_empty() { fallback.to_string() } else { s }
}

// ─── Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri_plugin_global_shortcut::ShortcutState;

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Global hotkey to toggle the stealth copilot overlay without touching
        // the mouse. Only one shortcut is registered (below, in `setup`), so the
        // handler doesn't need to disambiguate which fired.
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    use tauri::Manager;
                    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
                    // Ctrl+`  → manually end + transcribe the active capture (the
                    // user's override for imperfect silence detection). Anything
                    // else registered (Ctrl+\) toggles the stealth overlay.
                    let finish_sc = Shortcut::new(Some(Modifiers::CONTROL), Code::Backquote);
                    if *shortcut == finish_sc {
                        if let Some(flag) =
                            app.state::<VoiceState>().listen_finish.lock().unwrap().as_ref()
                        {
                            flag.store(true, Ordering::SeqCst);
                        }
                        return;
                    }
                    if let Err(e) = copilot::toggle(app) {
                        eprintln!("[copilot] hotkey toggle failed: {e}");
                    }
                })
                .build(),
        )
        .manage(SidecarState::default())
        .manage(RecorderState::default())
        .manage(VoiceState::default())
        .manage(copilot::CopilotContextState::default())
        .setup(|app| {
            // Register Ctrl+\ to toggle the stealth copilot overlay. Best-effort:
            // if another app already owns the chord, log and carry on rather than
            // failing app startup.
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
                let sc = Shortcut::new(Some(Modifiers::CONTROL), Code::Backslash);
                if let Err(e) = app.global_shortcut().register(sc) {
                    eprintln!("[copilot] global shortcut register failed: {e}");
                }
                // Ctrl+`  → manually end + transcribe the active capture, for when
                // the silence detector mis-judges the end of a question/answer.
                let finish_sc = Shortcut::new(Some(Modifiers::CONTROL), Code::Backquote);
                if let Err(e) = app.global_shortcut().register(finish_sc) {
                    eprintln!("[copilot] finish shortcut register failed: {e}");
                }
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let state = handle.state::<SidecarState>();
                // Load the persistent bridge token (minted on first run) and
                // seed the sidecar with the stored Gemini key so the
                // extension's autofill works without the app forwarding the
                // key on every request. Persistent + sticky port (see
                // sidecar::pick_port) keep the extension pairing valid across
                // app restarts instead of forcing a re-pair every launch.
                let token = sidecar::load_or_mint_token(gen_token);
                let creds = Credentials::load();
                match PythonSidecar::start(&token, &creds.gemini_api_key) {
                    Ok(sidecar) => {
                        let url = sidecar.base_url();
                        let port = sidecar.port();
                        // The env var only seeds the Gemini key — push the full
                        // LLM config (provider toggle + every key) now that the
                        // sidecar answers. Best-effort: a failure just means the
                        // extension endpoints run on the env-seeded default
                        // until the user next saves Settings.
                        let _ = backend_client::seed_config(&url, &token, &creds.llm_json());
                        {
                            let mut inner = state.inner.lock().unwrap();
                            inner.base_url = Some(url.clone());
                            inner.token    = Some(token);
                            inner.port     = Some(port);
                            inner.sidecar  = Some(sidecar);
                            inner.last_err = None;
                        }
                        let _ = handle.emit("sidecar:ready", url);
                    }
                    Err(e) => {
                        {
                            let mut inner = state.inner.lock().unwrap();
                            inner.last_err = Some(e.clone());
                        }
                        let _ = handle.emit("sidecar:error", e);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_jobs,
            save_jobs,
            load_credentials,
            save_credentials,
            list_resumes,
            save_resumes,
            backend_status,
            bridge_info,
            reseed_backend_key,
            poll_job_inbox,
            ack_job_inbox,
            poll_timeline_inbox,
            ack_timeline_inbox,
            start_chat_stream,
            start_research_stream,
            start_company_research_stream,
            start_application_tailor_stream,
            start_knockout_screen_stream,
            generate_cheatsheet,
            save_cheatsheet_md,
            save_resume_docx,
            open_path,
            list_audio_devices,
            start_recording,
            stop_recording,
            voice_status,
            voice_speak_chunk,
            voice_speak_flush,
            voice_set_barge,
            voice_set_engine,
            voice_warm,
            voice_prepare,
            voice_interrupt,
            voice_listen,
            voice_listen_system,
            voice_stop_listening,
            voice_finish_listening,
            copilot::open_copilot,
            copilot::close_copilot,
            copilot::toggle_copilot,
            copilot::copilot_cloak_status,
            copilot::copilot_typing,
            copilot::copilot_set_opacity,
            copilot::set_copilot_context,
            copilot::get_copilot_context,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Drop on managed State only runs reliably during a normal exit
            // path. ExitRequested fires before the Tauri runtime tears down,
            // which is the right window to kill the Python sidecar's process
            // tree. Without this, closing the window leaves python.exe (and
            // any children) orphaned until reboot.
            if let RunEvent::ExitRequested { .. } = event {
                let sidecar_state = app_handle.state::<SidecarState>();
                let mut inner = sidecar_state.inner.lock().unwrap();
                if let Some(mut sc) = inner.sidecar.take() {
                    sc.shutdown();
                }
                inner.base_url = None;
                // Also kill any lingering recording worker.
                let rec_state = app_handle.state::<RecorderState>();
                let maybe_session = rec_state.inner.lock().unwrap().take();
                if let Some(session) = maybe_session {
                    session.request_stop();
                    let _ = session.finish();
                }
            }
        });
}
