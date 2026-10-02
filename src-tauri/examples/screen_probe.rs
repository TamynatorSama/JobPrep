//! Phase 4 gate: screenshot → answer, through the app's real capture path
//! (`screen::capture_window`: GDI grab, Windows OCR, WinRT JPEG).
//!
//! Opens two windows: a "target" showing a coding problem in black on white,
//! and a magenta "overlay" on top of it with WDA_EXCLUDEFROMCAPTURE — the same
//! cloak the copilot overlay uses. Then:
//!   1. captures the target: the overlay must not appear (no magenta pixels,
//!      none of its words in the OCR text);
//!   2. control: drops the cloak and captures again — the magenta must show,
//!      so check 1 really covered the overlay's area;
//!   3. with a config on stdin ({"url", "llm", "job_context"}, as
//!      `backend/bench/screen_gate.py` sends), answers the capture in "screen"
//!      mode and times keypress → first answer token (capture + request).
//! Prints one JSON object. Run alone (`cargo run --example screen_probe <
//! NUL`) for checks 1–2 only.

use std::io::{BufRead, BufReader, Read};
use std::time::Instant;

use interprep_lib::screen;
use serde_json::{json, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DrawTextW, EndPaint, FillRect, SelectObject, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_LEFT, DT_WORDBREAK,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, PostQuitMessage, HMENU,
    RegisterClassW, SetWindowDisplayAffinity, ShowWindow, TranslateMessage, MSG, SW_SHOW,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_EX_STYLE, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP,
};

const PROBLEM: &str = "Longest Goal Streak\n\n\
You are given an array steps where steps[i] is the number of steps walked on day i, \
and an integer goal. A streak is a run of consecutive days where every day meets the goal. \
Return the length of the longest streak. If no day meets the goal, return 0.\n\n\
Example 1:\nInput: steps = [8000, 12000, 10500, 3000, 11000], goal = 10000\nOutput: 2\n\n\
Example 2:\nInput: steps = [500, 700], goal = 1000\nOutput: 0\n\n\
Constraints:\n1 <= steps.length <= 100000\n0 <= steps[i], goal <= 1000000\n\n\
Write a function longest_streak(steps, goal) that runs in linear time.";
const OVERLAY_TEXT: &str = "SECRET OVERLAY ZEBRA";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

unsafe fn paint(hwnd: HWND, bg: u32, fg: u32, text: &str, size: i32) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    FillRect(hdc, &rc, CreateSolidBrush(COLORREF(bg)));
    let font = CreateFontW(
        size, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, w!("Segoe UI"),
    );
    SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, COLORREF(fg));
    let mut r = RECT { left: rc.left + 24, top: rc.top + 20, right: rc.right - 24, bottom: rc.bottom - 20 };
    let mut t = wide(text);
    DrawTextW(hdc, &mut t, &mut r, DT_LEFT | DT_WORDBREAK);
    let _ = EndPaint(hwnd, &ps);
}

unsafe extern "system" fn target_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd, 0x00FF_FFFF, 0x0000_0000, PROBLEM, 22);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        paint(hwnd, 0x00FF_00FF, 0x0000_0000, OVERLAY_TEXT, 30); // magenta (0x00BBGGRR)
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Create both windows on this thread and pump messages forever.
fn ui_thread(tx: std::sync::mpsc::Sender<(isize, isize)>) {
    unsafe {
        let inst = GetModuleHandleW(None).expect("module handle");
        for (name, proc_) in [
            (w!("ProbeTarget"), target_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (w!("ProbeOverlay"), overlay_proc),
        ] {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: inst.into(),
                lpszClassName: name,
                ..Default::default()
            });
        }
        let target = CreateWindowExW(
            WINDOW_EX_STYLE(0), w!("ProbeTarget"), w!("Assessment"), WS_OVERLAPPEDWINDOW,
            120, 120, 1100, 720, HWND::default(), HMENU::default(), inst, None,
        )
        .expect("target window");
        let overlay = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW, w!("ProbeOverlay"), PCWSTR::null(), WS_POPUP,
            700, 200, 420, 360, HWND::default(), HMENU::default(), inst, None,
        )
        .expect("overlay window");
        for h in [target, overlay] {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = UpdateWindow(h);
        }
        SetWindowDisplayAffinity(overlay, WDA_EXCLUDEFROMCAPTURE).expect("cloak");
        tx.send((target.0 as isize, overlay.0 as isize)).unwrap();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Magenta pixels in a fresh grab of `hwnd`'s rectangle.
fn magenta_px(hwnd: isize) -> usize {
    let frame = screen::win::grab_window(hwnd).expect("grab");
    frame.bgra.chunks_exact(4).filter(|p| p[0] > 230 && p[1] < 30 && p[2] > 230).count()
}

/// Stream the answer; (ms to first token, total ms, text, error).
fn answer(conf: &Value, shot: screen::Shot) -> (Option<u128>, u128, String, String) {
    let t0 = Instant::now();
    let body = screen::request_body(&[shot], "", conf["job_context"].as_str().unwrap_or("").into(), conf["llm"].clone());
    let url = format!("{}/chat/stream", conf["url"].as_str().unwrap_or(""));
    let resp = match reqwest::blocking::Client::new().post(url).json(&body).timeout(std::time::Duration::from_secs(120)).send() {
        Ok(r) => r,
        Err(e) => return (None, t0.elapsed().as_millis(), String::new(), e.to_string()),
    };
    let (mut first, mut text, mut error) = (None, String::new(), String::new());
    for line in BufReader::new(resp).lines().map_while(Result::ok) {
        let Some(data) = line.strip_prefix("data:") else { continue };
        let ev: Value = serde_json::from_str(data.trim()).unwrap_or(Value::Null);
        match ev["type"].as_str() {
            Some("token") => {
                first.get_or_insert_with(|| t0.elapsed().as_millis());
                text.push_str(ev["content"].as_str().unwrap_or(""));
            }
            Some("error") => {
                error = ev["content"].as_str().unwrap_or("").into();
                break;
            }
            Some("done") => break,
            _ => {}
        }
    }
    (first, t0.elapsed().as_millis(), text, error)
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let conf: Value = serde_json::from_str(&stdin).unwrap_or(Value::Null);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || ui_thread(tx));
    let (target, overlay) = rx.recv().expect("windows");
    std::thread::sleep(std::time::Duration::from_millis(900)); // let both paint

    // The app's path, timed as the keypress would be.
    let t_key = Instant::now();
    let shot = screen::capture_window(target).expect("capture");
    let capture_ms = t_key.elapsed().as_millis();
    let ocr = shot.text.clone();
    let info = shot.info.clone();
    let cloaked_magenta = magenta_px(target);

    unsafe {
        SetWindowDisplayAffinity(HWND(overlay as _), WDA_NONE).expect("uncloak");
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    let control_magenta = magenta_px(target);

    let ocr_lower = ocr.to_lowercase();
    let keywords = ["longest goal streak", "consecutive days", "longest_streak", "linear time"];
    let found = keywords.iter().filter(|k| ocr_lower.contains(*k)).count();
    let mut out = json!({
        "overlay_excluded": cloaked_magenta == 0 && !ocr.contains("ZEBRA"),
        "cloaked_magenta_px": cloaked_magenta,
        "control_magenta_px": control_magenta,
        "overlay_words_in_ocr": ocr.contains("ZEBRA"),
        "ocr_keywords": format!("{found}/{}", keywords.len()),
        "ocr_chars": info.chars,
        "text_heavy": info.text_heavy,
        "image_kb": shot.image.len() / 1024,
        "width": info.width,
        "height": info.height,
        "capture_ms": capture_ms,
        "ocr_text": ocr,
    });
    if conf["url"].is_string() {
        let (first, total, text, error) = answer(&conf, shot);
        out["request_to_first_token_ms"] = json!(first);
        out["keypress_to_first_token_ms"] = json!(first.map(|f| f + capture_ms));
        out["total_ms"] = json!(total + capture_ms);
        out["answer"] = json!(text);
        out["error"] = json!(error);
    }
    println!("{out}");
}
