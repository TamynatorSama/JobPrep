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

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowDisplayAffinity, GetWindowLongPtrW, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SET_WINDOW_POS_FLAGS, SM_REMOTESESSION,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR,
    WDA_NONE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

pub const COPILOT_LABEL: &str = "copilot";

const COPILOT_W: f64 = 384.0;
const COPILOT_H: f64 = 640.0;

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

/// Apply the overlay's extended-window-style hardening:
///   • `WS_EX_TOOLWINDOW` (− `WS_EX_APPWINDOW`) — drops it from Alt+Tab and Task
///     View. `skip_taskbar` only removes the taskbar button, not the switcher.
///   • `WS_EX_NOACTIVATE` — clicking or showing the overlay does NOT steal focus
///     from whatever's in front (e.g. a browser meeting). Critical: a focus
///     change is a visible tell to the call / proctoring. Toggled off briefly by
///     `copilot_typing` when the user deliberately types.
#[cfg(windows)]
fn set_overlay_styles(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new = (ex | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize)
            & !(WS_EX_APPWINDOW.0 as isize);
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

/// Apply every Windows-side hardening to the overlay and return the cloak mode.
#[cfg(windows)]
fn harden(window: &tauri::WebviewWindow) -> &'static str {
    let Ok(raw) = window.hwnd() else { return "none" };
    // Reconstruct HWND from the raw handle so this compiles regardless of which
    // `windows` crate version Tauri itself pins — `.0` is just the pointer.
    let hwnd = HWND(raw.0 as _);
    set_overlay_styles(hwnd);
    apply_cloak(hwnd)
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

#[cfg(windows)]
fn harden_window(window: &tauri::WebviewWindow) {
    let mode = harden(window);
    if mode != "excluded" {
        eprintln!("[copilot] cloak degraded: mode={mode}");
    }
}

#[cfg(not(windows))]
fn harden_window(_window: &tauri::WebviewWindow) {}

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
    let url = main
        .as_ref()
        .and_then(|w| w.url().ok())
        .map(WebviewUrl::External)
        .unwrap_or_else(|| WebviewUrl::App("index.html".into()));

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
        // Do NOT grab focus on open — surfacing the overlay must not pull focus
        // off the meeting window. WS_EX_NOACTIVATE (set in set_overlay_styles)
        // keeps clicks from stealing focus too.
        .focused(false)
        .build()
        .map_err(|e| format!("build copilot window failed: {e}"))?;

    position_overlay(&win, main.as_ref());
    // Apply capture-cloak + Alt+Tab hiding. Best-effort: a degraded cloak (e.g.
    // pre-2004 Windows falls back to black-box) shouldn't stop the overlay from
    // opening — the real state is reported to the UI via copilot_cloak_status.
    harden_window(&win);
    Ok(win)
}

/// Open the overlay (or re-show + re-cloak it if it already exists). Re-cloaking
/// on every open guards against the documented hide/show clearing the affinity.
#[tauri::command]
pub fn open_copilot(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        let _ = win.show();
        // Intentionally no set_focus — surfacing must not steal focus from the
        // meeting window. re-harden re-applies cloak + no-activate after show.
        harden_window(&win);
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
/// command. Returns the new visibility state (`true` = now open).
pub fn toggle(app: &AppHandle) -> Result<bool, String> {
    if let Some(win) = app.get_webview_window(COPILOT_LABEL) {
        win.close().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        build_window(app)?;
        Ok(true)
    }
}

#[tauri::command]
pub fn toggle_copilot(app: AppHandle) -> Result<bool, String> {
    toggle(&app)
}
