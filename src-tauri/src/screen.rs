//! Screenshot → answer for the copilot (Phase 4).
//!
//! The overlay's Capture button (or Ctrl+Shift+\) grabs the window the user is
//! working in — the meeting, the coding test — reads its text with Windows'
//! built-in OCR, and sends it to the model with a picture of the screen:
//!
//!   * capture: GDI BitBlt of the target window's on-screen rectangle. The
//!     overlay carries `WDA_EXCLUDEFROMCAPTURE`, so DWM leaves it out of our
//!     own screenshot just like it does for a screen share. GDI, not Windows
//!     Graphics Capture, which can draw a yellow border around the window.
//!   * target: the foreground window, or — when the overlay itself has focus
//!     (the user is typing an instruction) — the last foreground window that
//!     wasn't the overlay, tracked by a small polling thread.
//!   * OCR: `Windows.Media.Ocr`, a few hundred ms, no tokens.
//!   * image: text-heavy screens (a coding prompt, a quiz) send the OCR text
//!     plus a 768 px thumbnail; anything else (a diagram, a UI) sends a 1,568 px
//!     image. Scaling and JPEG encoding go through WinRT's BitmapEncoder, which
//!     is native code — fast even in a debug build.
//!
//! Everything stays in memory: up to three captures wait in `ScreenState` until
//! the user sends them (stacked captures for problems that scroll), then they
//! are dropped. Nothing is written to disk.

use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

/// At most this many captures wait to be sent together.
pub const MAX_SHOTS: usize = 3;
/// Long edge of the image sent with a text-heavy screen (the text rides as OCR).
const THUMB_EDGE: u32 = 768;
/// Long edge of the image sent when the screen is mostly not text.
const FULL_EDGE: u32 = 1568;
/// Long edge of the chip preview shown in the overlay.
const PREVIEW_EDGE: u32 = 240;
/// A screen counts as text-heavy with at least this much OCR text over at
/// least TEXT_HEAVY_LINES lines.
const TEXT_HEAVY_CHARS: usize = 200;
const TEXT_HEAVY_LINES: usize = 4;

/// One capture, ready to send.
pub struct Shot {
    pub id: u64,
    /// What OCR read, one screen line per line.
    pub text: String,
    /// JPEG for the model: a thumbnail when text-heavy, else the full image.
    pub image: Vec<u8>,
    pub text_heavy: bool,
    pub info: ShotInfo,
}

/// What the overlay gets back: a small preview, never the full image.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotInfo {
    pub id: u64,
    pub preview: String,
    pub width: u32,
    pub height: u32,
    pub chars: usize,
    pub text_heavy: bool,
    pub ms: u64,
}

#[derive(Default)]
pub struct ScreenState {
    shots: Mutex<Vec<Shot>>,
    /// A hotkey capture taken while the overlay wasn't open yet: answer it as
    /// soon as the overlay is listening (`copilot_screen_ready`).
    autostart: Mutex<bool>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// The last foreground window that wasn't the overlay (raw HWND).
static LAST_TARGET: AtomicIsize = AtomicIsize::new(0);

fn overlay_hwnd(app: &AppHandle) -> isize {
    app.get_webview_window(crate::copilot::COPILOT_LABEL)
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .unwrap_or(0)
}

/// Remember the user's working window, so a capture taken while the overlay
/// has focus still grabs the meeting or the coding test behind it.
pub fn start_foreground_tracker(app: AppHandle) {
    std::thread::spawn(move || loop {
        let fg = win::foreground();
        if fg != 0 && fg != overlay_hwnd(&app) {
            LAST_TARGET.store(fg, Ordering::Relaxed);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    });
}

fn target(app: &AppHandle) -> isize {
    let fg = win::foreground();
    if fg != 0 && fg != overlay_hwnd(app) {
        fg
    } else {
        LAST_TARGET.load(Ordering::Relaxed)
    }
}

fn data_url(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg))
}

/// Grab, read and encode the user's working window. Blocking (~0.3–0.6 s).
pub fn capture(app: &AppHandle) -> Result<Shot, String> {
    capture_window(target(app))
}

/// `capture` for a given window (raw HWND; 0 = its monitor) — also what
/// `examples/screen_probe.rs` drives.
pub fn capture_window(hwnd: isize) -> Result<Shot, String> {
    let t0 = Instant::now();
    let frame = win::grab_window(hwnd)?;
    let t_grab = t0.elapsed().as_millis();
    let _com = win::ComInit::mta();
    let text = win::ocr(&frame).unwrap_or_else(|e| {
        eprintln!("[screen] OCR failed: {e}");
        String::new()
    });
    let t_ocr = t0.elapsed().as_millis();
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let text_heavy = text.chars().count() >= TEXT_HEAVY_CHARS && lines >= TEXT_HEAVY_LINES;
    let image = win::jpeg(&frame, if text_heavy { THUMB_EDGE } else { FULL_EDGE })?;
    let preview = win::jpeg(&frame, PREVIEW_EDGE)?;
    let ms = t0.elapsed().as_millis() as u64;
    eprintln!(
        "[screen] {}x{} grab {t_grab} ms, ocr {} ms ({} chars), encode {} ms, {} KB sent",
        frame.width, frame.height, t_ocr - t_grab, text.len(), ms as u128 - t_ocr, image.len() / 1024
    );
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let info = ShotInfo {
        id,
        preview: data_url(&preview),
        width: frame.width,
        height: frame.height,
        chars: text.chars().count(),
        text_heavy,
        ms,
    };
    Ok(Shot { id, text, image, text_heavy, info })
}

/// The `/chat/stream` body for answering `shots` (mode "screen").
pub fn request_body(shots: &[Shot], instruction: &str, job_context: String, llm: Value) -> Value {
    let several = shots.len() > 1;
    let screen_text = shots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.text.trim().is_empty())
        .map(|(i, s)| if several { format!("--- capture {} ---\n{}", i + 1, s.text) } else { s.text.clone() })
        .collect::<Vec<_>>()
        .join("\n\n");
    let images: Vec<Value> = shots
        .iter()
        .map(|s| json!({
            "mime": "image/jpeg",
            "data": base64::engine::general_purpose::STANDARD.encode(&s.image),
        }))
        .collect();
    json!({
        "message": instruction,
        "job_context": job_context,
        "history": [],
        "mode": "screen",
        "llm": llm,
        "documents": [],
        "images": images,
        "screen_text": screen_text,
    })
}

/// Send every waiting capture to the model; tokens arrive as `chat:*` events
/// under `stream_id`. Clears the stack.
fn answer(app: &AppHandle, instruction: &str, stream_id: &str) -> Result<(), String> {
    let url = crate::sidecar_url(app).ok_or("Backend is not ready yet")?;
    let shots: Vec<Shot> = std::mem::take(&mut *app.state::<ScreenState>().shots.lock().unwrap());
    if shots.is_empty() {
        return Err("Nothing captured yet".into());
    }
    let context = app.state::<crate::copilot::CopilotContextState>().context();
    let body = request_body(&shots, instruction, context, crate::ai_routing::llm_for("copilot"));
    crate::backend_client::stream_json(app.clone(), format!("{url}/chat/stream"), body, stream_id.to_string());
    Ok(())
}

fn push(state: &ScreenState, shot: Shot) -> ShotInfo {
    let info = shot.info.clone();
    let mut shots = state.shots.lock().unwrap();
    shots.push(shot);
    let excess = shots.len().saturating_sub(MAX_SHOTS);
    shots.drain(..excess);
    info
}

fn new_stream_id() -> String {
    format!("screen-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// Ctrl+Shift+\ — capture and answer in one go. If the overlay isn't open yet,
/// the capture waits until it is listening (`copilot_screen_ready`), so no
/// streamed token is lost before its listeners exist.
pub fn hotkey(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Capture BEFORE showing the overlay: a freshly built overlay is
        // capturable for its first ~1.2 s (copilot.rs defers the cloak).
        let shot = match capture(&app) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[screen] hotkey capture failed: {e}");
                return;
            }
        };
        let state = app.state::<ScreenState>();
        let info = push(&state, shot);
        match crate::copilot::show(&app) {
            Ok(true) => *state.autostart.lock().unwrap() = true,
            Ok(false) => {
                let previews: Vec<ShotInfo> = vec![info];
                let sid = new_stream_id();
                let _ = app.emit("copilot:screen", json!({ "streamId": sid, "shots": previews }));
                if let Err(e) = answer(&app, "", &sid) {
                    eprintln!("[screen] hotkey answer failed: {e}");
                }
            }
            Err(e) => eprintln!("[screen] could not show the overlay: {e}"),
        }
    });
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Capture the working window and add it to the stack (oldest dropped past
/// three). Returns the chip preview.
#[tauri::command]
pub async fn copilot_capture(app: AppHandle) -> Result<ShotInfo, String> {
    let shot = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || capture(&app))
            .await
            .map_err(|e| e.to_string())??
    };
    Ok(push(&app.state::<ScreenState>(), shot))
}

#[tauri::command]
pub fn copilot_capture_remove(state: State<ScreenState>, id: u64) {
    state.shots.lock().unwrap().retain(|s| s.id != id);
}

/// The captures waiting to be sent (the overlay re-reads them on mount).
#[tauri::command]
pub fn copilot_capture_list(state: State<ScreenState>) -> Vec<ShotInfo> {
    state.shots.lock().unwrap().iter().map(|s| s.info.clone()).collect()
}

/// Answer the waiting captures, with the user's optional instruction.
#[tauri::command]
pub fn copilot_screen_answer(app: AppHandle, instruction: String, stream_id: String) -> Result<(), String> {
    answer(&app, instruction.trim(), &stream_id)
}

/// Called by the overlay once its listeners are registered. If a hotkey
/// capture opened it, start that answer now and hand back its stream id.
#[tauri::command]
pub fn copilot_screen_ready(app: AppHandle) -> Option<Value> {
    let state = app.state::<ScreenState>();
    let pending = std::mem::take(&mut *state.autostart.lock().unwrap());
    if !pending {
        return None;
    }
    let shots: Vec<ShotInfo> = state.shots.lock().unwrap().iter().map(|s| s.info.clone()).collect();
    let sid = new_stream_id();
    answer(&app, "", &sid).ok()?;
    Some(json!({ "streamId": sid, "shots": shots }))
}

// ── Win32 / WinRT ────────────────────────────────────────────────────────────

pub mod win {
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapEncoder, BitmapInterpolationMode, BitmapPixelFormat, SoftwareBitmap,
    };
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
        GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        CAPTUREBLT, DIB_RGB_COLORS, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, IsIconic};

    /// A captured window: top-down BGRA, 4 bytes per pixel.
    pub struct Frame {
        pub width: u32,
        pub height: u32,
        pub bgra: Vec<u8>,
    }

    /// COM (MTA) for the WinRT calls on this thread; undone on drop only if
    /// this call is what initialized it.
    pub struct ComInit(bool);
    impl ComInit {
        pub fn mta() -> Self {
            Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }
    impl Drop for ComInit {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub fn foreground() -> isize {
        unsafe { GetForegroundWindow() }.0 as isize
    }

    /// The window's visible bounds (no invisible resize border), clipped to
    /// its monitor. Without a usable window: that monitor whole.
    fn bounds(hwnd: HWND) -> Result<RECT, String> {
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return Err("no monitor for the target window".into());
            }
            let screen = mi.rcMonitor;
            if hwnd.0.is_null() || IsIconic(hwnd).as_bool() {
                return Ok(screen);
            }
            let mut r = RECT::default();
            let dwm = DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut r as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            );
            if dwm.is_err() {
                GetWindowRect(hwnd, &mut r).map_err(|e| e.to_string())?;
            }
            let clipped = RECT {
                left: r.left.max(screen.left),
                top: r.top.max(screen.top),
                right: r.right.min(screen.right),
                bottom: r.bottom.min(screen.bottom),
            };
            if clipped.right - clipped.left < 32 || clipped.bottom - clipped.top < 32 {
                return Ok(screen);
            }
            Ok(clipped)
        }
    }

    /// What's on screen inside `hwnd`'s rectangle — including any window over
    /// it, except those excluded from capture (the overlay).
    pub fn grab_window(hwnd: isize) -> Result<Frame, String> {
        let r = bounds(HWND(hwnd as _))?;
        grab_rect(r.left, r.top, (r.right - r.left) as u32, (r.bottom - r.top) as u32)
    }

    pub fn grab_rect(x: i32, y: i32, width: u32, height: u32) -> Result<Frame, String> {
        unsafe {
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                return Err("GetDC failed".into());
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, width as i32, height as i32);
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            let blt = BitBlt(mem, 0, 0, width as i32, height as i32, screen, x, y, SRCCOPY | CAPTUREBLT);
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; width as usize * height as usize * 4];
            SelectObject(mem, old);
            let rows = GetDIBits(mem, bmp, 0, height, Some(bgra.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            ReleaseDC(HWND::default(), screen);
            blt.map_err(|e| format!("BitBlt failed: {e}"))?;
            if rows != height as i32 {
                return Err(format!("GetDIBits read {rows} of {height} rows"));
            }
            Ok(Frame { width, height, bgra })
        }
    }

    fn bitmap(frame: &Frame) -> windows::core::Result<SoftwareBitmap> {
        let buf = CryptographicBuffer::CreateFromByteArray(&frame.bgra)?;
        SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buf,
            BitmapPixelFormat::Bgra8,
            frame.width as i32,
            frame.height as i32,
            BitmapAlphaMode::Ignore,
        )
    }

    /// The screen's text, one recognized line per line, top to bottom.
    pub fn ocr(frame: &Frame) -> Result<String, String> {
        let run = || -> windows::core::Result<String> {
            let max = OcrEngine::MaxImageDimension()?;
            if frame.width > max || frame.height > max {
                return Ok(String::new());
            }
            let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
            let result = engine.RecognizeAsync(&bitmap(frame)?)?.get()?;
            let mut out = Vec::new();
            for line in result.Lines()? {
                out.push(line.Text()?.to_string_lossy());
            }
            Ok(out.join("\n"))
        };
        run().map_err(|e| e.to_string())
    }

    /// JPEG (quality 0.9, the encoder's default) scaled so the long edge is at
    /// most `long_edge`.
    pub fn jpeg(frame: &Frame, long_edge: u32) -> Result<Vec<u8>, String> {
        let run = || -> windows::core::Result<Vec<u8>> {
            let scale = (long_edge as f64 / frame.width.max(frame.height) as f64).min(1.0);
            let (w, h) = (
                ((frame.width as f64 * scale).round() as u32).max(1),
                ((frame.height as f64 * scale).round() as u32).max(1),
            );
            let stream = InMemoryRandomAccessStream::new()?;
            let encoder = BitmapEncoder::CreateAsync(BitmapEncoder::JpegEncoderId()?, &stream)?.get()?;
            encoder.SetSoftwareBitmap(&bitmap(frame)?)?;
            let t = encoder.BitmapTransform()?;
            t.SetScaledWidth(w)?;
            t.SetScaledHeight(h)?;
            t.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
            encoder.FlushAsync()?.get()?;
            let size = stream.Size()? as u32;
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
            reader.LoadAsync(size)?.get()?;
            let mut bytes = vec![0u8; size as usize];
            reader.ReadBytes(&mut bytes)?;
            Ok(bytes)
        };
        run().map_err(|e| format!("JPEG encode failed: {e}"))
    }
}
