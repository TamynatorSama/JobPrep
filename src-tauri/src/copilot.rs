//! Stealth copilot overlay — a separate always-on-top window that is excluded
//! from screen capture via `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`.
//!
//! The overlay loads the same frontend bundle as the main window; the React
//! entry (`main.tsx`) branches on the window label (`"copilot"`) to render the
//! compact Copilot UI instead of the full app.
//!
//! Improvement over the Electron `setContentProtection` approach: we call the
//! Win32 API directly with the modern `WDA_EXCLUDEFROMCAPTURE` flag (window is
//! fully removed from capture, not rendered black like the older `WDA_MONITOR`),
//! and we re-apply it on every `open` so a hide/show cycle can't silently
//! re-expose the window.

use tauri::{AppHandle, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder};

#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, HWND};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowDisplayAffinity, GetWindowLongPtrW, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, LWA_ALPHA,
    SET_WINDOW_POS_FLAGS, SM_REMOTESESSION, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

pub const COPILOT_LABEL: &str = "copilot";

const COPILOT_W: f64 = 384.0;
const COPILOT_H: f64 = 640.0;

// ─── Active-job context bridge ──────────────────────────────────────────────
//
// The copilot overlay is a SEPARATE window with its own React tree — it shares
// no state with the main app. So when the user opens the overlay (button or the
// global hotkey, which fires entirely in Rust), it has no idea which job is
// active. The main window mirrors the selected job's research dossier into this
// shared state on every selection/research change; the overlay reads the latest
// snapshot right before each chat stream and passes it as `job_context`. Because
// Rust always holds the freshest copy, both open paths (button + hotkey) ground
// the copilot on the same job the user is looking at.

/// The active job's context for the copilot overlay. `label` is a short
/// "Role · Company" line for the UI; `context` is the system-prompt block
/// (company/role/JD + company-research dossier + resume); `cheatsheet` is the
/// structured interview cheatsheet (stories/facts/questions) the Cheatsheet tab
/// renders, opaque here (`null` when the job has none yet).
#[derive(Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotContext {
    pub label: String,
    pub context: String,
    pub cheatsheet: serde_json::Value,
}

#[derive(Default)]
pub struct CopilotContextState {
    inner: std::sync::Mutex<CopilotContext>,
}

/// Mirror the active job's research context + cheatsheet from the main window.
/// Called whenever the selected job (or its research / resume / cheatsheet)
/// changes.
#[tauri::command]
pub fn set_copilot_context(
    state: State<CopilotContextState>,
    label: String,
    context: String,
    cheatsheet: Option<serde_json::Value>,
) {
    let mut g = state.inner.lock().unwrap();
    g.label = label;
    g.context = context;
    g.cheatsheet = cheatsheet.unwrap_or(serde_json::Value::Null);
}

/// Read the latest active-job context. Called by the overlay right before each
/// chat stream so the answer is grounded on whatever job is active *now*.
#[tauri::command]
pub fn get_copilot_context(state: State<CopilotContextState>) -> CopilotContext {
    state.inner.lock().unwrap().clone()
}

/// What level of capture-hiding actually took effect on the overlay.
/// `excluded` — `WDA_EXCLUDEFROMCAPTURE` confirmed live (window absent from
///   every DWM-mediated capture). `monitor` — old Windows fallback: the window
///   shows as a black box in captures (content hidden, outline visible).
///   `none` — not hidden (call failed, or debug build with the escape hatch).
#[cfg(windows)]
fn apply_cloak(hwnd: HWND) -> &'static str {
    // Debug-only escape hatch. Release builds IGNORE the env var so a stray
    // INTERPREP_NO_CLOAK can never silently expose a shipped overlay.
    if cfg!(debug_assertions) && std::env::var_os("INTERPREP_NO_CLOAK").is_some() {
        unsafe { let _ = SetWindowDisplayAffinity(hwnd, WDA_NONE); }
        return "none";
    }
    unsafe {
        // Preferred: full exclusion (Win10 2004+). VERIFY by reading it back —
        // a successful call that didn't stick should still report honestly.
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok() {
            let mut a: u32 = 0;
            if GetWindowDisplayAffinity(hwnd, &mut a).is_ok() && a == WDA_EXCLUDEFROMCAPTURE.0 {
                return "excluded";
            }
        }
        // Fallback for older Windows: black-box the window in captures.
        if SetWindowDisplayAffinity(hwnd, WDA_MONITOR).is_ok() {
            let mut a: u32 = 0;
            if GetWindowDisplayAffinity(hwnd, &mut a).is_ok() && a == WDA_MONITOR.0 {
                return "monitor";
            }
        }
    }
    "none"
}

/// Drop the overlay from Alt+Tab and Task View via `WS_EX_TOOLWINDOW`
/// (− `WS_EX_APPWINDOW`). `skip_taskbar` only kills the taskbar button, not the
/// switcher. NOTE: `WS_EX_NOACTIVATE` is applied separately and *deferred* (see
/// `harden`) — setting it before WebView2's first composition leaves the page
/// blank, so it must wait until after the first paint.
#[cfg(windows)]
fn set_overlay_styles(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new = (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        // FRAMECHANGED so the switcher re-reads the style immediately.
        let _ = SetWindowPos(
            hwnd, HWND::default(), 0, 0, 0, 0,
            SET_WINDOW_POS_FLAGS(SWP_NOMOVE.0 | SWP_NOSIZE.0 | SWP_NOZORDER.0 | SWP_FRAMECHANGED.0),
        );
    }
}

/// Flip `WS_EX_NOACTIVATE` on/off. With it off the overlay can take keyboard
/// focus (so the text box accepts typing); with it on the overlay never steals
/// focus. The frontend turns it off on input focus and back on when the box
/// blurs, so typing works without ever changing focus the rest of the time.
#[cfg(windows)]
fn set_noactivate(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let na = WS_EX_NOACTIVATE.0 as isize;
        let new = if on { ex | na } else { ex & !na };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
    }
}

/// Set the overlay's whole-window opacity via a layered window (OS compositor),
/// which is independent of WebView2's own compositing — so we get real
/// see-through without the blank-page failure that true window transparency
/// causes on Windows. `alpha` is 0..=255. At full opacity we REMOVE
/// `WS_EX_LAYERED` so the default render path (and stealth cloak) are untouched;
/// only a dialed-down overlay takes the layered path.
#[cfg(windows)]
fn set_overlay_opacity(hwnd: HWND, alpha: u8) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        if alpha >= 255 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
        } else {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
            // crkey unused (LWA_ALPHA only) → COLORREF(0).
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}

/// Set overlay opacity from the frontend (0.0..1.0). Clamped so the window can't
/// be made invisible/unclickable. No-op if the overlay isn't open.
#[tauri::command]
pub fn copilot_set_opacity(app: AppHandle, opacity: f64) {
    #[cfg(windows)]
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        if let Ok(raw) = win.hwnd() {
            let alpha = (opacity.clamp(0.30, 1.0) * 255.0).round() as u8;
            set_overlay_opacity(HWND(raw.0 as _), alpha);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, opacity);
    }
}

/// First-time hardening for a freshly built overlay. `WS_EX_TOOLWINDOW` is safe
/// to apply at creation, but BOTH `WS_EX_NOACTIVATE` *and* the capture cloak
/// (`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`) blank the page when set
/// before WebView2's first composition — each one switches the window onto a
/// different activation/compositing path and the initial paint never fires. So
/// both are deferred to a spawned thread that waits out the first paint. The
/// overlay is briefly activatable AND capturable (~1.2s right at open); that's
/// the price of a reliable first paint. The raw HWND pointer is `Send` as isize.
#[cfg(windows)]
fn harden_new(window: &tauri::WebviewWindow) {
    let Ok(raw) = window.hwnd() else { return };
    // Reconstruct HWND from the raw handle so this compiles regardless of which
    // `windows` crate version Tauri itself pins — `.0` is just the pointer.
    let hwnd = HWND(raw.0 as _);
    set_overlay_styles(hwnd);
    let ptr = hwnd.0 as isize;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let h = HWND(ptr as _);
        let mode = apply_cloak(h);
        set_noactivate(h, true);
        if mode != "excluded" {
            eprintln!("[copilot] cloak degraded: mode={mode}");
        }
    });
}

/// Re-cloak an EXISTING overlay on re-show. It has already completed its first
/// paint, so the cloak and no-activate can be re-applied immediately — no blank,
/// no exposure gap. Re-applying guards against a hide/show cycle silently
/// clearing the display affinity.
#[cfg(windows)]
fn recloak_now(window: &tauri::WebviewWindow) {
    let Ok(raw) = window.hwnd() else { return };
    let hwnd = HWND(raw.0 as _);
    set_overlay_styles(hwnd);
    let mode = apply_cloak(hwnd);
    set_noactivate(hwnd, true);
    if mode != "excluded" {
        eprintln!("[copilot] cloak degraded: mode={mode}");
    }
}

/// Briefly let the overlay accept keyboard focus so the user can type, then
/// restore no-activate. Called by the frontend on input focus/blur.
#[tauri::command]
pub fn copilot_typing(app: AppHandle, enabled: bool) {
    #[cfg(windows)]
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        if let Ok(raw) = win.hwnd() {
            let hwnd = HWND(raw.0 as _);
            set_noactivate(hwnd, !enabled);
            if enabled {
                let _ = win.set_focus();
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, enabled);
    }
}

/// Read the overlay's CURRENT capture-hiding state straight from the OS (ground
/// truth, not a cached value), plus whether we're in a remote session.
#[cfg(windows)]
fn cloak_report(app: &AppHandle) -> (&'static str, bool) {
    let mode = app
        .get_webview_window(COPILOT_LABEL)
        .and_then(|w| w.hwnd().ok())
        .map(|raw| unsafe {
            let hwnd = HWND(raw.0 as _);
            let mut a: u32 = 0;
            if GetWindowDisplayAffinity(hwnd, &mut a).is_ok() {
                if a == WDA_EXCLUDEFROMCAPTURE.0 { "excluded" }
                else if a == WDA_MONITOR.0 { "monitor" }
                else { "none" }
            } else { "none" }
        })
        .unwrap_or("closed");
    // SM_REMOTESESSION: an RDP/remote host streams the framebuffer on its side,
    // which can bypass per-window display affinity entirely.
    let remote = unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 };
    (mode, remote)
}

#[cfg(not(windows))]
fn harden_new(_window: &tauri::WebviewWindow) {}
#[cfg(not(windows))]
fn recloak_now(_window: &tauri::WebviewWindow) {}

/// Capture-hiding status for the Settings badge. `mode`: excluded | monitor |
/// none | closed. `remote`: true under RDP (stealth may not hold).
#[tauri::command]
pub fn copilot_cloak_status(app: AppHandle) -> serde_json::Value {
    #[cfg(windows)]
    {
        let (mode, remote) = cloak_report(&app);
        serde_json::json!({ "mode": mode, "remote": remote })
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        serde_json::json!({ "mode": "none", "remote": false })
    }
}

/// Park the overlay in the top-right corner of the main window's monitor with a
/// small margin, so it floats over a meeting window like a HUD. Anchoring to the
/// main window (rather than the new window's own size, which isn't realized yet)
/// keeps it on the right screen in multi-monitor setups and dodges a race where
/// the fresh window reports a stale size.
fn position_overlay(win: &tauri::WebviewWindow, main: Option<&tauri::WebviewWindow>) {
    if let Some(m) = main {
        if let (Ok(pos), Ok(size), Ok(scale)) = (m.outer_position(), m.outer_size(), m.scale_factor()) {
            let cw = (COPILOT_W * scale) as i32;
            let margin = (24.0 * scale) as i32;
            let x = pos.x + size.width as i32 - cw - margin;
            let y = pos.y + margin;
            let _ = win.set_position(PhysicalPosition::new(x.max(pos.x), y));
            return;
        }
    }
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let screen = monitor.size();
        let cw = (COPILOT_W * monitor.scale_factor()) as i32;
        let x = screen.width as i32 - cw - 24;
        let _ = win.set_position(PhysicalPosition::new(x.max(0), 24));
    }
}

/// Build the cloaked overlay window from scratch.
fn build_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    // Load exactly what the main window loaded. In `tauri dev` that's the Vite
    // dev-server URL; `WebviewUrl::App("index.html")` resolves to the wrong base
    // for a programmatically-created window and renders a blank white page.
    // Mirroring the main window's URL works in both dev and the bundled app.
    let main = app.get_webview_window("main");
    let main_url = main.as_ref().and_then(|w| w.url().ok());
    let url = main_url
        .clone()
        .map(WebviewUrl::External)
        .unwrap_or_else(|| WebviewUrl::App("index.html".into()));
    eprintln!(
        "[copilot] build_window: main_found={} mirror_url={:?} on_main_thread={}",
        main.is_some(),
        main_url.as_ref().map(|u| u.as_str()),
        // crude: window creation must happen on the UI thread on Windows
        std::thread::current().name().unwrap_or("<unnamed>").to_string(),
    );

    let win = WebviewWindowBuilder::new(app, COPILOT_LABEL, url)
        .title("Audio Service")
        .inner_size(COPILOT_W, COPILOT_H)
        .min_inner_size(320.0, 360.0)
        .decorations(false)
        // NOTE: deliberately NOT transparent. Transparent WebView2 windows on
        // Windows frequently composite to nothing (especially with CSS
        // backdrop-filter), so the overlay would open but render invisible. An
        // opaque dark panel is reliably visible and the screen-capture cloak
        // (SetWindowDisplayAffinity) is independent of window transparency.
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(true)
        .shadow(true)
        // MUST be focused(true): WebView2 won't kick its first paint on a window
        // created unfocused (→ blank page). We immediately hand focus back to the
        // prior foreground window (below) so opening still doesn't steal focus.
        .focused(true)
        .build()
        .map_err(|e| format!("build copilot window failed: {e}"))?;
    eprintln!("[copilot] build_window: window built, hwnd_ok={}", win.hwnd().is_ok());

    position_overlay(&win, main.as_ref());
    // Alt+Tab hiding now; capture-cloak + no-activate deferred until after the
    // first paint (see harden_new — applying them at creation blanks the page).
    // Best-effort: a degraded cloak (e.g. pre-2004 Windows falls back to
    // black-box) shouldn't stop the overlay from opening — the real state is
    // reported to the UI via copilot_cloak_status.
    harden_new(&win);
    Ok(win)
}

/// Open the overlay (or re-show + re-cloak it if it already exists). Re-cloaking
/// on every open guards against the documented hide/show clearing the affinity.
///
/// MUST be `async`: a sync `#[tauri::command]` runs on the main (event-loop)
/// thread, and `WebviewWindowBuilder::build()` on Windows needs that loop to keep
/// pumping while the WebView2 controller initializes. Blocking the loop inside the
/// command deadlocks build() → white, frozen window. Async commands run off the
/// main thread, so the loop stays free to pump and the build completes.
#[tauri::command]
pub async fn open_copilot(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        let _ = win.show();
        // Intentionally no set_focus — surfacing must not steal focus from the
        // meeting window. Existing window already painted, so re-cloak now.
        recloak_now(&win);
        return Ok(());
    }
    build_window(&app).map(|_| ())
}

/// Close the overlay window if open.
#[tauri::command]
pub fn close_copilot(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        win.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Toggle the overlay — used by the global hotkey and the Copilot toggle
/// command. Returns the new visibility state (`true` = now shown). Hides rather
/// than closes an existing window so its position, opacity, and React state
/// survive a hide/show cycle (the user can drag it once and it stays put). The
/// `close_copilot` command (X button) is the only path that destroys it.
pub fn toggle(app: &AppHandle) -> Result<bool, String> {
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        if win.is_visible().unwrap_or(true) {
            win.hide().map_err(|e| e.to_string())?;
            Ok(false)
        } else {
            win.show().map_err(|e| e.to_string())?;
            recloak_now(&win); // re-apply cloak/no-activate after a hide/show
            Ok(true)
        }
    } else {
        build_window(app)?;
        Ok(true)
    }
}

/// Async for the same reason as `open_copilot`: building the overlay window must
/// not run on the blocked main event-loop thread (deadlocks build() on Windows).
#[tauri::command]
pub async fn toggle_copilot(app: AppHandle) -> Result<bool, String> {
    toggle(&app)
}
