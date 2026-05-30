//! InterPrep — Tauri runtime entry point.

mod backend_client;
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
}

#[derive(Default)]
struct RecorderState {
    inner: Mutex<Option<recorder::RecordingSession>>,
}

/// Queue item for the serialized TTS playback worker.
enum TtsMsg {
    Speak(String),
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
    listen_stop: Mutex<Option<Arc<AtomicBool>>>,
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
    api_key: String,
    documents: Vec<backend_client::RagDocPayload>,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_chat(app, url, message, job_context, history, mode, api_key, documents);
    Ok(())
}

#[tauri::command]
fn start_research_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    job_description: String,
    api_key: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_research(app, url, company, role, job_description, api_key);
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
    api_key: String,
    glassdoor_email: String,
    glassdoor_password: String,
    indeed_email: String,
    indeed_password: String,
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
        api_key,
        glassdoor_email,
        glassdoor_password,
        indeed_email,
        indeed_password,
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
    api_key: String,
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
        api_key,
    );
    Ok(())
}

#[tauri::command]
fn start_knockout_screen_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    api_key: String,
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
        api_key,
    );
    Ok(())
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

    let session = recorder::RecordingSession::start(config, None)
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

/// Serialized TTS playback worker. Pulls chunks off the queue, synthesizes +
/// plays each in order (so streamed sentences never overlap), runs a barge-in
/// monitor during playback, and emits `voice:speak_done` on each `Flush`.
fn voice_worker(
    app: AppHandle,
    url: String,
    rx: std::sync::mpsc::Receiver<TtsMsg>,
    interrupt_slot: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    abort: Arc<AtomicBool>,
    barge_enabled: Arc<AtomicBool>,
) {
    while let Ok(msg) = rx.recv() {
        match msg {
            TtsMsg::Flush => {
                // `abort` is set if a barge-in/stop cut this utterance short.
                let interrupted = abort.swap(false, Ordering::SeqCst);
                let _ = app.emit("voice:speak_done", serde_json::json!({ "interrupted": interrupted }));
            }
            TtsMsg::Speak(text) => {
                if abort.load(Ordering::SeqCst) {
                    continue; // draining the rest of an aborted utterance
                }
                let wav = match backend_client::voice_tts(&url, &text) {
                    Ok(w) => w,
                    Err(e) => { let _ = app.emit("voice:error", e); continue; }
                };
                if abort.load(Ordering::SeqCst) {
                    continue;
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
                let played = voice_audio::play_wav(&wav, &interrupt, &mut on_level).unwrap_or(false);
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
    std::thread::spawn(move || voice_worker(app2, url, rx, slot, abort, barge));
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
) -> Result<(), String> {
    let url = require_url(&sidecar)?;
    // A fresh utterance is starting — clear any stale abort from a prior turn.
    voice.abort.store(false, Ordering::SeqCst);
    let tx = ensure_voice_worker(&app, url, &voice);
    let _ = tx.send(TtsMsg::Speak(text));
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
    *voice.listen_stop.lock().unwrap() = Some(Arc::clone(&stop));

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
        let result = voice_audio::record_answer(&stop, &mut on_level);
        let _ = app.emit("voice:level", serde_json::json!({ "level": 0.0, "pitch": 0.0, "mode": "listening" }));
        match result {
            Ok(Some(wav)) => {
                use base64::Engine;
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

/// Force-end mic capture early (user pressed stop / disabled voice).
#[tauri::command]
fn voice_stop_listening(voice: State<VoiceState>) {
    if let Some(flag) = voice.listen_stop.lock().unwrap().as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
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
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(SidecarState::default())
        .manage(RecorderState::default())
        .manage(VoiceState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let state = handle.state::<SidecarState>();
                match PythonSidecar::start() {
                    Ok(sidecar) => {
                        let url = sidecar.base_url();
                        {
                            let mut inner = state.inner.lock().unwrap();
                            inner.base_url = Some(url.clone());
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
            start_chat_stream,
            start_research_stream,
            start_company_research_stream,
            start_application_tailor_stream,
            start_knockout_screen_stream,
            save_resume_docx,
            open_path,
            list_audio_devices,
            start_recording,
            stop_recording,
            voice_status,
            voice_speak_chunk,
            voice_speak_flush,
            voice_set_barge,
            voice_interrupt,
            voice_listen,
            voice_stop_listening,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Drop on managed State only runs reliably during a normal exit
            // path. ExitRequested fires before the Tauri runtime tears down,
            // which is the right window to kill the Python sidecar (and its
            // Chromium grandchildren). Without this, closing the window
            // leaves python.exe + chrome.exe orphaned until reboot.
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
