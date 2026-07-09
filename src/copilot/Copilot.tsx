// ─── InterPrep — Always-on-top Interview Copilot ────────────────────────────
//
// Faithful port of the claude.ai/design "Copilot.html" / copilot.jsx design,
// wired to the real Tauri backend. Rendered into the cloaked "copilot" window
// (see main.tsx label branch). A floating, stealth, glass HUD with four views:
// Live transcript · Suggested answer · Cheatsheet · Settings.
//
// Adapted for the desktop reality:
//   • The window is OPAQUE — transparent WebView2 windows render invisible on
//     Windows, so the design's `backdrop-filter` glass is faked with solid dark
//     surfaces instead of true blur.
//   • The Tweaks-panel CSS vars (--co-alpha/glass/scale) are hardcoded.
//   • QuickAsk + mic drive the real chat stream; the answer streams into the
//     Suggested-answer view. Rec toggles the mic listen loop.

import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

// ── Theme — InterPrep's dark Framer tokens ──────────────────────────────────
const T = {
  bg: "#0C0C0C", surface: "#1A1A1A", surface2: "#242424",
  border: "#1F1F1F",
  text: "#FFFFFF", textSecondary: "#9A9A9A", textTertiary: "#646464",
  accent: "#0099FF", accentSoft: "rgba(0,153,255,0.13)",
  green: "#22C55E",
  amber: "#F59E0B",
  violet: "#a855f7", violetSoft: "rgba(168,85,247,0.14)",
  red: "#EF4444",
  // Opaque dark "glass" surface (no real blur — see header note).
  glass: "#111113",
  glassBorder: "rgba(255,255,255,0.10)",
  glassEdge: "rgba(255,255,255,0.08)",
  shadowFloat: "0 1px 0 rgba(255,255,255,0.06) inset, 0 24px 70px rgba(0,0,0,0.66)",
  gradientViolet: "linear-gradient(135deg, #4a1a8a 0%, #7c3aed 42%, #a855f7 72%, #c084fc 100%)",
  fontDisplay: "'Geist','Inter',sans-serif", fontBody: "'Inter',sans-serif",
};

// ── Backend helpers (mirror App.tsx) ────────────────────────────────────────
interface Creds {
  llmProvider?: string;
  geminiApiKey?: string;
  openaiApiKey?: string;
  anthropicApiKey?: string;
}
const llmPayload = (c: Creds) => ({
  provider: c.llmProvider || "gemini",
  gemini_api_key: c.geminiApiKey ?? "",
  openai_api_key: c.openaiApiKey ?? "",
  anthropic_api_key: c.anthropicApiKey ?? "",
});
const newStreamId = () =>
  `cop-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;

// ── Cheatsheet (mirrored from the active job via the context bridge) ─────────
interface CheatStory { title: string; tag?: string; metric?: string; beats?: string[]; }
interface CheatFact { k: string; v: string; }
interface CheatQuestion { q: string; why?: string; }
interface Cheatsheet {
  summary?: string;
  stories?: CheatStory[];
  facts?: CheatFact[];
  questions?: CheatQuestion[];
  markdown?: string;
  updatedAt?: number;
}
interface CopilotCtx { label: string; context: string; cheatsheet: Cheatsheet | null; }

// ── Icons (ported from the design) ──────────────────────────────────────────
type IconName =
  | "spark" | "mic" | "micOff" | "eye" | "eyeOff" | "x" | "minus" | "check"
  | "chevDown" | "chevUp" | "chevRight" | "send" | "refresh" | "copy"
  | "search" | "monitor" | "volume" | "shield" | "sliders" | "clock" | "alert"
  | "arrowUp" | "camera" | "brief" | "image" | "lock" | "user";

const Icon = ({ name, size = 16, color = "currentColor", sw = 1.7 }: {
  name: IconName; size?: number; color?: string; sw?: number;
}) => {
  const p = {
    width: size, height: size, viewBox: "0 0 24 24", fill: "none", stroke: color,
    strokeWidth: sw, strokeLinecap: "round" as const, strokeLinejoin: "round" as const,
  };
  const g: Record<IconName, ReactNode> = {
    spark: <path d="M12 3 10.1 8.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3Z" />,
    mic: <><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" /><path d="M19 10v2a7 7 0 0 1-14 0v-2" /><line x1="12" y1="19" x2="12" y2="23" /><line x1="8" y1="23" x2="16" y2="23" /></>,
    micOff: <><line x1="1" y1="1" x2="23" y2="23" /><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" /><path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23" /><line x1="12" y1="19" x2="12" y2="23" /><line x1="8" y1="23" x2="16" y2="23" /></>,
    eyeOff: <><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" /><line x1="1" y1="1" x2="23" y2="23" /></>,
    eye: <><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></>,
    alert: <><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" /><line x1="12" y1="9" x2="12" y2="13" /><line x1="12" y1="17" x2="12.01" y2="17" /></>,
    x: <><line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" /></>,
    minus: <line x1="5" y1="12" x2="19" y2="12" />,
    check: <polyline points="20 6 9 17 4 12" />,
    chevDown: <polyline points="6 9 12 15 18 9" />,
    chevUp: <polyline points="18 15 12 9 6 15" />,
    chevRight: <polyline points="9 18 15 12 9 6" />,
    send: <><line x1="22" y1="2" x2="11" y2="13" /><polygon points="22 2 15 22 11 13 2 9 22 2" /></>,
    refresh: <><polyline points="23 4 23 10 17 10" /><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" /></>,
    copy: <><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></>,
    search: <><circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" /></>,
    monitor: <><rect x="2" y="3" width="20" height="14" rx="2" /><line x1="8" y1="21" x2="16" y2="21" /><line x1="12" y1="17" x2="12" y2="21" /></>,
    volume: <><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" /><path d="M15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14" /></>,
    shield: <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />,
    sliders: <><line x1="4" y1="21" x2="4" y2="14" /><line x1="4" y1="10" x2="4" y2="3" /><line x1="12" y1="21" x2="12" y2="12" /><line x1="12" y1="8" x2="12" y2="3" /><line x1="20" y1="21" x2="20" y2="16" /><line x1="20" y1="12" x2="20" y2="3" /><line x1="1" y1="14" x2="7" y2="14" /><line x1="9" y1="8" x2="15" y2="8" /><line x1="17" y1="16" x2="23" y2="16" /></>,
    clock: <><circle cx="12" cy="12" r="10" /><polyline points="12 6 12 12 16 14" /></>,
    arrowUp: <><line x1="12" y1="19" x2="12" y2="5" /><polyline points="5 12 12 5 19 12" /></>,
    camera: <><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" /><circle cx="12" cy="13" r="4" /></>,
    brief: <><rect x="2" y="7" width="20" height="14" rx="2" /><path d="M16 21V5a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16" /></>,
    image: <><rect x="3" y="3" width="18" height="18" rx="2" /><circle cx="8.5" cy="8.5" r="1.5" /><polyline points="21 15 16 10 5 21" /></>,
    lock: <><rect x="3" y="11" width="18" height="11" rx="2" /><path d="M7 11V7a5 5 0 0 1 10 0v4" /></>,
    user: <><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" /><circle cx="12" cy="7" r="4" /></>,
  };
  return <svg {...p}>{g[name]}</svg>;
};

// ── Bits ────────────────────────────────────────────────────────────────────
const pill = (bg: string, color: string, br?: string): CSSProperties => ({
  display: "inline-flex", alignItems: "center", gap: 5, padding: "3px 8px",
  borderRadius: 100, background: bg, color, border: br ?? "none", fontSize: 10.5,
  fontWeight: 600, letterSpacing: "-0.1px", whiteSpace: "nowrap", lineHeight: 1,
});
const Dot = ({ c, pulse }: { c: string; pulse?: boolean }) => (
  <span style={{ width: 6, height: 6, borderRadius: "50%", background: c, flexShrink: 0, animation: pulse ? "co-blink 1.3s ease-in-out infinite" : "none" }} />
);
// Live audio meter. When `levelRef` is supplied the bars are driven by the real
// capture level on a rAF loop (DOM writes only — no React re-render): bar HEIGHT
// tracks RMS loudness and bar COLOR tracks pitch (zero-crossing rate), so the
// user can see voice is actually being picked up. Without `levelRef` it falls
// back to the old decorative CSS-keyframe bars.
const Wave = ({ active, color, count = 22, h = 22, levelRef }: {
  active?: boolean; color: string; count?: number; h?: number;
  levelRef?: { current: { level: number; pitch: number } };
}) => {
  const bars = useRef<(HTMLDivElement | null)[]>([]);
  const heights = useRef<number[]>(Array(count).fill(0));

  useEffect(() => {
    if (!levelRef) return;            // decorative mode handled by CSS below
    if (!active) {                    // paused → flatten the bars
      heights.current.fill(0);
      bars.current.forEach((b) => { if (b) { b.style.height = "3px"; b.style.background = T.border; } });
      return;
    }
    let raf = 0;
    const mid = (count - 1) / 2;
    const tick = () => {
      const { level, pitch } = levelRef.current;
      // RMS of speech sits ~0..0.12; map to 0..1. Pitch (ZCR) ~0..0.4 → hue
      // sweep from violet (low) toward cyan (high) so timbre is visible.
      const amp = Math.min(1, level / 0.12);
      const hue = 270 - Math.min(1, pitch / 0.35) * 90;   // 270°→180°
      const now = performance.now();
      for (let i = 0; i < count; i++) {
        const dist = Math.abs(i - mid) / mid;             // 0 center → 1 edge
        const shape = 1 - dist * 0.55;                    // center-weighted
        const jitter = 0.78 + 0.22 * Math.sin(i * 1.7 + now / 90);
        const target = amp * shape * jitter;
        const prev = heights.current[i];
        const k = target > prev ? 0.55 : 0.16;            // fast attack, slow release
        const v = prev + (target - prev) * k;
        heights.current[i] = v;
        const b = bars.current[i];
        if (b) {
          b.style.height = `${(3 + v * (h - 3)).toFixed(1)}px`;
          b.style.background = v > 0.04 ? `hsl(${hue.toFixed(0)},90%,${(55 + amp * 12).toFixed(0)}%)` : T.border;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [active, levelRef, count, h]);

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 2.5, height: h }}>
      {Array.from({ length: count }).map((_, i) => (
        <div
          key={i}
          ref={levelRef ? (el) => { bars.current[i] = el; } : undefined}
          style={{
            width: 2.5, borderRadius: 3, transformOrigin: "center",
            height: levelRef ? 3 : (active ? h : 3),
            background: levelRef ? T.border : (active ? color : T.border),
            animation: levelRef ? "none" : (active ? `co-bar 0.5s ease-in-out ${(i % 7) * 0.08}s infinite alternate` : "none"),
          }}
        />
      ))}
    </div>
  );
};
// Cursor stays `default` on controls — stealth contract (don't telegraph clicks).
const press: CSSProperties = { cursor: "default" };

// Real capture-hiding status, read from the OS via copilot_cloak_status.
type CloakMode = "excluded" | "monitor" | "none" | "closed" | "unknown";
interface CloakStatus { mode: CloakMode; remote: boolean }

const STEALTH_UI: Record<string, { c: string; bg: string; icon: IconName; full: string; compact: string }> = {
  excluded: { c: T.green, bg: "rgba(34,197,94,0.10)", icon: "eyeOff", full: "Hidden from share", compact: "Hidden" },
  monitor: { c: T.amber, bg: "rgba(245,158,11,0.12)", icon: "eyeOff", full: "Black-box only", compact: "Black-box" },
  none: { c: T.red, bg: "rgba(239,68,68,0.12)", icon: "eye", full: "NOT hidden", compact: "Exposed" },
  unknown: { c: T.textTertiary, bg: "rgba(255,255,255,0.05)", icon: "eye", full: "Checking…", compact: "…" },
};
const stealthUi = (s: CloakStatus) => STEALTH_UI[s.mode] ?? STEALTH_UI.unknown;

const StealthPill = ({ status, compact }: { status: CloakStatus; compact?: boolean }) => {
  const u = stealthUi(status);
  return (
    <span style={{ ...pill(u.bg, u.c, `0.5px solid ${u.c}44`), padding: compact ? "3px 7px" : "3px 9px" }}>
      <Icon name={status.remote && status.mode !== "none" ? "alert" : u.icon} size={10} color={u.c} sw={2} />
      {compact ? (status.remote ? "Remote" : u.compact) : u.full}
    </span>
  );
};

// ── Shell: drag bar ─────────────────────────────────────────────────────────
const ctrlMini: CSSProperties = { width: 22, height: 22, borderRadius: 6, border: "none", background: "none", display: "flex", alignItems: "center", justifyContent: "center", ...press };

function DragBar({ state, job, cloak, onMin, onClose }: { state?: string; job?: string; cloak: CloakStatus; onMin: () => void; onClose: () => void }) {
  // The whole bar is a drag handle. Children sit on top of the parent, and a
  // child is only draggable if it ALSO carries the attribute — so tag every
  // non-interactive child (the buttons + stealth pill stay clickable).
  return (
    <div data-tauri-drag-region style={{ height: 36, flexShrink: 0, display: "flex", alignItems: "center", gap: 8, padding: "0 8px 0 11px", borderBottom: `1px solid ${T.glassEdge}`, cursor: "default" }}>
      <div data-tauri-drag-region style={{ display: "flex", flexDirection: "column", gap: 2.5, flexShrink: 0, opacity: 0.4 }}>
        {[0, 1].map((r) => <div key={r} data-tauri-drag-region style={{ display: "flex", gap: 2.5 }}>{[0, 1, 2].map((c) => <span key={c} data-tauri-drag-region style={{ width: 2, height: 2, borderRadius: "50%", background: T.text }} />)}</div>)}
      </div>
      <div data-tauri-drag-region style={{ width: 17, height: 17, borderRadius: 5, background: T.gradientViolet, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, boxShadow: "0 1px 6px rgba(124,58,237,0.5)" }}>
        <Icon name="spark" size={9.5} color="#fff" sw={2.2} />
      </div>
      <span data-tauri-drag-region style={{ fontSize: 11.5, fontWeight: 600, color: T.text, letterSpacing: "-0.2px", fontFamily: T.fontDisplay, flexShrink: 0 }}>Copilot</span>
      {/* Active job grounding takes priority over the view-state label — it tells
          the user which research the answers are drawn from. Ellipsizes so it
          never crowds the stealth pill / window controls. */}
      {job
        ? <span data-tauri-drag-region title={`Grounded on ${job}`} style={{ fontSize: 11, color: T.textSecondary, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", minWidth: 0 }}>· {job}</span>
        : state && <span data-tauri-drag-region style={{ fontSize: 11, color: T.textTertiary, flexShrink: 0 }}>· {state}</span>}
      <div data-tauri-drag-region style={{ flex: 1, minWidth: 8 }} />
      <StealthPill status={cloak} compact />
      <div style={{ display: "flex", alignItems: "center", gap: 1, flexShrink: 0 }}>
        <button onClick={onMin} style={ctrlMini} title="Minimize"><Icon name="minus" size={13} color={T.textTertiary} /></button>
        <button onClick={onClose} style={ctrlMini} title="Close (Ctrl+\\)"><Icon name="x" size={13} color={T.textTertiary} /></button>
      </div>
    </div>
  );
}

// ── Shell: nav bar (tabs + capture + rec) ───────────────────────────────────
type View = "live" | "answer" | "cheat" | "settings";
function NavBar({ view, setView, recording, onToggleRec, onCapture, time }: {
  view: View; setView: (v: View) => void; recording: boolean; onToggleRec: () => void; onCapture: () => void; time: string;
}) {
  const Tab = ({ id, icon, label }: { id: View; icon: IconName; label: string }) => {
    const on = view === id;
    return (
      <button onClick={() => setView(id)} title={label} style={{ width: 30, height: 28, borderRadius: 100, border: "none", background: on ? "#fff" : "transparent", display: "flex", alignItems: "center", justifyContent: "center", ...press, transition: "background .15s" }}>
        <Icon name={icon} size={14} color={on ? "#0C0C0C" : T.textSecondary} sw={on ? 2 : 1.8} />
      </button>
    );
  };
  return (
    <div style={{ height: 46, flexShrink: 0, display: "flex", alignItems: "center", gap: 7, padding: "0 10px", borderBottom: `1px solid ${T.glassEdge}` }}>
      <div style={{ display: "flex", gap: 2, background: "rgba(255,255,255,0.045)", border: `0.5px solid ${T.border}`, borderRadius: 100, padding: 3 }}>
        <Tab id="live" icon="volume" label="Live transcript" />
        <Tab id="answer" icon="spark" label="Suggested answer" />
        <Tab id="cheat" icon="brief" label="Cheatsheet" />
      </div>
      <button onClick={() => setView("settings")} title="Settings & privacy" style={{ width: 30, height: 30, borderRadius: 9, border: "none", background: view === "settings" ? "rgba(255,255,255,0.1)" : "rgba(255,255,255,0.04)", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, ...press }}>
        <Icon name="sliders" size={14} color={view === "settings" ? T.text : T.textSecondary} />
      </button>
      <div style={{ flex: 1 }} />
      <button onClick={onCapture} title="Capture screen — send a screenshot to the AI" style={{ height: 30, display: "flex", alignItems: "center", gap: 5, padding: "0 10px", borderRadius: 100, border: `0.5px solid ${T.border}`, background: "rgba(255,255,255,0.05)", color: T.textSecondary, fontSize: 11, fontWeight: 600, flexShrink: 0, fontFamily: T.fontBody, ...press }}>
        <Icon name="camera" size={13} color={T.textSecondary} />Capture
      </button>
      <button onClick={onToggleRec} title="Listen to the call (system audio) — auto-answers each question on silence" style={{ height: 30, display: "flex", alignItems: "center", gap: 6, padding: "0 11px", borderRadius: 100, border: `0.5px solid ${recording ? "rgba(239,68,68,0.45)" : T.border}`, background: recording ? "rgba(239,68,68,0.16)" : "rgba(255,255,255,0.05)", color: recording ? "#ff6b6b" : T.text, fontSize: 11, fontWeight: 600, flexShrink: 0, fontFamily: T.fontBody, ...press }}>
        {recording
          ? <><span style={{ width: 9, height: 9, borderRadius: 2, background: "#ff5b5b" }} /><span style={{ fontVariantNumeric: "tabular-nums" }}>{time}</span></>
          : <><span style={{ width: 8, height: 8, borderRadius: "50%", background: T.red }} />Rec audio</>}
      </button>
    </div>
  );
}

// ── Shell: quick-ask footer ─────────────────────────────────────────────────
function QuickAsk({ placeholder, value, onChange, onSend, busy, listening, onMic }: {
  placeholder: string; value: string; onChange: (v: string) => void; onSend: () => void; busy: boolean; listening: boolean; onMic: () => void;
}) {
  return (
    <div style={{ flexShrink: 0, padding: "9px 10px 10px", borderTop: `1px solid ${T.glassEdge}` }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, background: "rgba(255,255,255,0.04)", border: `0.5px solid ${T.border}`, borderRadius: 12, padding: "7px 8px 7px 12px" }}>
        <Icon name="spark" size={13} color={T.textTertiary} sw={2} />
        <input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); onSend(); } }}
          // The overlay is no-activate (never steals focus). Briefly allow
          // activation while the box is focused so keystrokes land, then restore
          // no-activate on blur — so typing works without a visible focus change
          // the rest of the time. onMouseDown fires even on a no-activate window.
          onMouseDown={() => invoke("copilot_typing", { enabled: true }).catch(() => {})}
          onFocus={() => invoke("copilot_typing", { enabled: true }).catch(() => {})}
          onBlur={() => invoke("copilot_typing", { enabled: false }).catch(() => {})}
          placeholder={placeholder}
          style={{ flex: 1, minWidth: 0, background: "transparent", border: "none", outline: "none", color: T.text, fontSize: 12, fontFamily: T.fontBody, letterSpacing: "-0.12px" }}
        />
        <button onClick={onMic} title="Hold mic — capture a spoken question" style={{ width: 26, height: 26, borderRadius: 8, border: "none", background: listening ? T.accentSoft : "rgba(255,255,255,0.06)", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, ...press }}>
          <Icon name="mic" size={13} color={listening ? T.accent : T.textSecondary} />
        </button>
        <button onClick={onSend} disabled={busy || !value.trim()} title="Send" style={{ width: 26, height: 26, borderRadius: 8, border: "none", background: value.trim() && !busy ? "#fff" : "rgba(255,255,255,0.1)", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, opacity: busy ? 0.6 : 1, ...press }}>
          <Icon name="send" size={12} color={value.trim() && !busy ? "#0C0C0C" : T.textSecondary} sw={2} />
        </button>
      </div>
    </div>
  );
}

// Pipeline status for the copilot, shown so the user always knows what's
// happening (and that a long transcribe/think isn't a hang).
type Phase = "idle" | "listening" | "transcribing" | "thinking" | "answering";
const PHASE_UI: Record<Phase, { label: string; c: string; pulse: boolean }> = {
  idle:         { label: "Paused",         c: T.textTertiary,   pulse: false },
  listening:    { label: "Listening",      c: T.accent,         pulse: true  },
  transcribing: { label: "Transcribing…",  c: T.amber,          pulse: true  },
  thinking:     { label: "Thinking…",      c: T.violet,         pulse: true  },
  answering:    { label: "Answering",      c: T.green,          pulse: true  },
};

// ── View: live transcript (real captured lines + status + streamed answer) ──
function ListenBody({ phase, time, lines, levelRef, question, answer, answering, onFinish }: {
  phase: Phase; time: string;
  lines: { speaker: string; text: string; last: boolean }[];
  levelRef: { current: { level: number; pitch: number } };
  question: string; answer: string; answering: boolean;
  onFinish: () => void;
}) {
  const u = PHASE_UI[phase];
  const active = phase !== "idle";
  const show = lines.length ? lines : [{ speaker: "—", text: "Tap Rec audio to listen in on the call — I'll transcribe each interviewer question and draft an answer when they finish. (Or use the mic to ask your own.)", last: true }];
  return (
    <div style={{ display: "flex", flexDirection: "column" }}>
      {/* Status header — the dot/label always reflect the current phase. */}
      <div style={{ flexShrink: 0, padding: "11px 13px", display: "flex", alignItems: "center", gap: 11, borderBottom: `1px solid ${T.glassEdge}` }}>
        <div style={{ display: "flex", alignItems: "center", gap: 7 }}>
          <span style={{ position: "relative", width: 8, height: 8, flexShrink: 0 }}>
            <span style={{ position: "absolute", inset: 0, borderRadius: "50%", background: u.c, animation: u.pulse ? "co-pulse 1.3s ease-in-out infinite" : "none" }} />
          </span>
          <span style={{ fontSize: 11.5, fontWeight: 600, color: active ? T.text : T.textSecondary }}>{u.label}</span>
        </div>
        {/* Wave only animates while actually capturing audio. */}
        <div style={{ flex: 1 }}><Wave active={phase === "listening"} color={T.accent} count={24} h={20} levelRef={levelRef} /></div>
        {/* Manual override for imperfect silence detection: end capture now and
            transcribe what's been heard. Only while actively listening. Also
            bound to the global Ctrl+` hotkey. */}
        {phase === "listening" && (
          <button onClick={onFinish} title="Transcribe now (Ctrl+`) — stop waiting for silence"
            style={{ height: 24, display: "flex", alignItems: "center", gap: 5, padding: "0 9px", borderRadius: 100, border: `0.5px solid ${T.accent}55`, background: T.accentSoft, color: T.accent, fontSize: 10.5, fontWeight: 600, fontFamily: T.fontBody, flexShrink: 0, ...press }}>
            <Icon name="check" size={11} color={T.accent} sw={2.4} />Transcribe
          </button>
        )}
        <span style={{ fontSize: 11, color: active ? T.textSecondary : T.textTertiary, fontVariantNumeric: "tabular-nums" }}>{active ? time : "tap Rec"}</span>
      </div>

      <div style={{ padding: "13px 14px 8px" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 7, marginBottom: 11 }}>
          <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.1em", textTransform: "uppercase", color: T.textTertiary }}>Live transcript</span>
          <div style={{ flex: 1, height: 1, background: T.border }} />
          <span style={pill("rgba(255,255,255,0.05)", T.textSecondary)}>System audio</span>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {show.map((l, i) => (
            <div key={i} style={{ display: "flex", gap: 9 }}>
              <div style={{ width: 22, height: 22, borderRadius: 6, flexShrink: 0, background: "#635BFF22", border: "0.5px solid #635BFF55", display: "flex", alignItems: "center", justifyContent: "center", fontSize: 10, fontWeight: 700, color: "#7c83ff", fontFamily: T.fontDisplay }}>{l.speaker[0] ?? "•"}</div>
              <p style={{ fontSize: 13, lineHeight: 1.5, color: l.last ? T.text : T.textSecondary, letterSpacing: "-0.13px" }}>
                {l.text}{l.last && phase === "transcribing" && <span style={{ marginLeft: 2, opacity: 0.55, animation: "co-blink 1s steps(2) infinite" }}>▌</span>}
              </p>
            </div>
          ))}
        </div>

        {/* Live answer — same data as the Answer tab, shown inline here so the
            user watches the question and the drafted reply in one place. */}
        {(question && (answer || answering)) && (
          <div style={{ marginTop: 14 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 8 }}>
              <Icon name="spark" size={12} color={T.violet} sw={2} />
              <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.1em", textTransform: "uppercase", color: T.violet }}>Suggested answer</span>
            </div>
            <div style={{ background: "rgba(168,85,247,0.06)", border: `0.5px solid ${T.border}`, borderRadius: 12, padding: "11px 12px", minHeight: 44 }}>
              <p style={{ fontSize: 13, lineHeight: 1.55, color: T.text, letterSpacing: "-0.13px", whiteSpace: "pre-wrap" }}>
                {answer || "…"}
                {answering && answer.length > 0 && <span style={{ marginLeft: 1, opacity: 0.5, animation: "co-blink 1s steps(2) infinite" }}>▌</span>}
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// ── View: suggested answer (real streamed answer) ───────────────────────────
function AnswerBody({ question, answer, answering, jobLabel, onRephrase, onDeeper, onCopy }: {
  question: string; answer: string; answering: boolean; jobLabel: string; onRephrase: () => void; onDeeper: () => void; onCopy: () => void;
}) {
  if (!question && !answer) {
    return (
      <div style={{ padding: "26px 18px", display: "flex", flexDirection: "column", alignItems: "center", gap: 12, textAlign: "center" }}>
        <div style={{ width: 44, height: 44, borderRadius: 13, background: T.violetSoft, display: "flex", alignItems: "center", justifyContent: "center" }}>
          <Icon name="spark" size={22} color={T.violet} sw={1.9} />
        </div>
        <p style={{ fontSize: 13, fontWeight: 600, color: T.text, fontFamily: T.fontDisplay }}>Suggested answers land here</p>
        <p style={{ fontSize: 11.5, lineHeight: 1.5, color: T.textTertiary, maxWidth: 240 }}>Type a question below, or hit <b style={{ color: T.textSecondary }}>Rec audio</b> to ask one out loud. I'll draft a strong spoken answer.</p>
        {jobLabel
          ? <span style={pill(T.violetSoft, "#c084fc")}><Icon name="brief" size={10} color="#c084fc" sw={2} />Grounded on {jobLabel}</span>
          : <span style={pill("rgba(255,255,255,0.05)", T.textTertiary)}>No active job — open one in the app to ground answers</span>}
      </div>
    );
  }
  return (
    <div style={{ display: "flex", flexDirection: "column" }}>
      <div style={{ flexShrink: 0, padding: "12px 14px", borderBottom: `1px solid ${T.glassEdge}`, background: "linear-gradient(180deg, rgba(0,153,255,0.07), transparent)" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 7 }}>
          <span style={pill(T.accentSoft, T.accent)}><Dot c={T.accent} pulse={answering} />{answering ? "Drafting…" : "Question"}</span>
        </div>
        <p style={{ fontSize: 13.5, fontWeight: 600, color: T.text, lineHeight: 1.45, letterSpacing: "-0.2px", fontFamily: T.fontDisplay }}>
          {question || "…"}
        </p>
      </div>
      <div style={{ padding: "13px 14px 8px" }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 10 }}>
          <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, fontWeight: 700, color: T.violet, letterSpacing: "0.02em" }}>
            <Icon name="spark" size={12} color={T.violet} sw={2} />SUGGESTED ANSWER
          </span>
        </div>
        <div style={{ background: "rgba(255,255,255,0.035)", border: `0.5px solid ${T.border}`, borderRadius: 13, padding: "12px 13px", position: "relative", overflow: "hidden", minHeight: 60 }}>
          {answering && answer.length === 0 && (
            <div style={{ position: "absolute", top: 0, left: 0, width: 60, height: "100%", background: "linear-gradient(90deg, rgba(168,85,247,0.16), transparent)", animation: "co-sheen 2.6s ease-in-out infinite", pointerEvents: "none" }} />
          )}
          <p style={{ fontSize: 13, lineHeight: 1.6, color: T.text, letterSpacing: "-0.13px", whiteSpace: "pre-wrap" }}>
            {answer || (answering ? "…" : "")}
            {answering && answer.length > 0 && <span style={{ marginLeft: 1, opacity: 0.5, animation: "co-blink 1s steps(2) infinite" }}>▌</span>}
          </p>
        </div>
        <div style={{ display: "flex", gap: 7, marginTop: 12 }}>
          {([{ i: "refresh", l: "Rephrase", on: onRephrase }, { i: "arrowUp", l: "Go deeper", on: onDeeper }, { i: "copy", l: "Copy", on: onCopy }] as const).map((a) => (
            <button key={a.l} onClick={a.on} disabled={answering || !answer} style={{ flex: 1, height: 32, borderRadius: 100, border: `0.5px solid ${T.border}`, background: "rgba(255,255,255,0.04)", color: T.textSecondary, fontSize: 11.5, fontWeight: 500, fontFamily: T.fontBody, display: "flex", alignItems: "center", justifyContent: "center", gap: 5, opacity: answering || !answer ? 0.5 : 1, ...press }}>
              <Icon name={a.i as IconName} size={12} color={T.textSecondary} />{a.l}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

// ── View: cheatsheet (live — mirrored from the active job, LLM-maintained) ───
type CheatSection = "stories" | "facts" | "questions";

function CheatBody({ cheat, busy, jobLabel, onRefresh }: {
  cheat: Cheatsheet | null; busy: boolean; jobLabel: string; onRefresh: () => void;
}) {
  const [section, setSection] = useState<CheatSection>("stories");
  const [query, setQuery] = useState("");
  const [openStory, setOpenStory] = useState(0);

  const stories = cheat?.stories ?? [];
  const facts = cheat?.facts ?? [];
  const questions = cheat?.questions ?? [];
  const q = query.trim().toLowerCase();
  const hit = (...parts: (string | undefined)[]) => !q || parts.some((p) => (p ?? "").toLowerCase().includes(q));
  const fStories = stories.filter((s) => hit(s.title, s.metric, s.tag, ...(s.beats ?? [])));
  const fFacts = facts.filter((f) => hit(f.k, f.v));
  const fQuestions = questions.filter((x) => hit(x.q, x.why));

  const isEmpty = !cheat || (stories.length === 0 && facts.length === 0 && questions.length === 0);

  // Refresh button — shared by every state.
  const refreshBtn = (
    <button onClick={onRefresh} disabled={busy} title="Rebuild from your conversations, resume & company research"
      style={{ width: 30, height: 30, borderRadius: 9, border: `0.5px solid ${T.border}`, background: "rgba(255,255,255,0.05)", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, opacity: busy ? 0.5 : 1, ...press }}>
      <Icon name="refresh" size={14} color={busy ? T.textTertiary : T.textSecondary} sw={busy ? 2 : 1.8} />
    </button>
  );

  if (isEmpty) {
    return (
      <div style={{ padding: "26px 18px", display: "flex", flexDirection: "column", alignItems: "center", gap: 12, textAlign: "center" }}>
        <div style={{ width: 44, height: 44, borderRadius: 13, background: T.violetSoft, display: "flex", alignItems: "center", justifyContent: "center" }}>
          <Icon name={busy ? "refresh" : "brief"} size={22} color={T.violet} sw={1.9} />
        </div>
        <p style={{ fontSize: 13, fontWeight: 600, color: T.text, fontFamily: T.fontDisplay }}>{busy ? "Building your cheatsheet…" : "No cheatsheet yet"}</p>
        <p style={{ fontSize: 11.5, lineHeight: 1.5, color: T.textTertiary, maxWidth: 250 }}>
          {busy
            ? "Mining your STAR stories, key facts, and questions to ask from this job's conversations, resume, and company research."
            : jobLabel
              ? "Build one from this job's conversations, resume, and company research — STAR stories, key facts, and sharp questions to ask."
              : "Open a job in the app first, then build a cheatsheet grounded on its conversations and research."}
        </p>
        {jobLabel && (
          <button onClick={onRefresh} disabled={busy}
            style={{ height: 34, padding: "0 16px", borderRadius: 100, border: "none", background: busy ? "rgba(255,255,255,0.1)" : "#fff", color: busy ? T.textSecondary : "#0C0C0C", fontSize: 12, fontWeight: 600, fontFamily: T.fontBody, display: "flex", alignItems: "center", gap: 7, ...press }}>
            <Icon name="refresh" size={13} color={busy ? T.textSecondary : "#0C0C0C"} sw={2} />{busy ? "Building…" : "Build cheatsheet"}
          </button>
        )}
      </div>
    );
  }

  const counts: Record<CheatSection, number> = { stories: stories.length, facts: facts.length, questions: questions.length };
  const SectionPill = ({ id, label }: { id: CheatSection; label: string }) => {
    const on = section === id;
    return (
      <span onClick={() => setSection(id)} style={{ ...pill(on ? "#fff" : "rgba(255,255,255,0.05)", on ? "#0C0C0C" : T.textSecondary, on ? "none" : `0.5px solid ${T.border}`), padding: "5px 11px", fontSize: 11.5, ...press }}>
        {label} · {counts[id]}
      </span>
    );
  };

  return (
    <div style={{ display: "flex", flexDirection: "column" }}>
      <div style={{ flexShrink: 0, padding: "11px 13px 0" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <div style={{ flex: 1, display: "flex", alignItems: "center", gap: 8, background: "rgba(255,255,255,0.04)", border: `0.5px solid ${T.border}`, borderRadius: 11, padding: "7px 11px", minWidth: 0 }}>
            <Icon name="search" size={13} color={T.textTertiary} />
            <input value={query} onChange={(e) => setQuery(e.target.value)}
              onMouseDown={() => invoke("copilot_typing", { enabled: true }).catch(() => {})}
              onFocus={() => invoke("copilot_typing", { enabled: true }).catch(() => {})}
              onBlur={() => invoke("copilot_typing", { enabled: false }).catch(() => {})}
              placeholder="Search stories, facts, questions…"
              style={{ flex: 1, minWidth: 0, background: "transparent", border: "none", outline: "none", color: T.text, fontSize: 12, fontFamily: T.fontBody }} />
          </div>
          {refreshBtn}
        </div>
        <div style={{ display: "flex", gap: 6 }}>
          <SectionPill id="stories" label="Stories" />
          <SectionPill id="facts" label="Facts" />
          <SectionPill id="questions" label="Questions" />
        </div>
      </div>

      <div style={{ padding: "12px 13px 10px", display: "flex", flexDirection: "column", gap: 8 }}>
        {cheat?.summary && section === "stories" && !q && (
          <div style={{ display: "flex", gap: 9, padding: "9px 12px", borderRadius: 11, background: T.violetSoft, border: `0.5px solid rgba(168,85,247,0.28)` }}>
            <Icon name="spark" size={13} color={T.violet} sw={2} />
            <p style={{ fontSize: 11.5, lineHeight: 1.45, color: T.textSecondary }}>{cheat.summary}</p>
          </div>
        )}

        {section === "stories" && (fStories.length === 0
          ? <EmptySection text={q ? "No stories match." : "No stories yet."} />
          : fStories.map((s, i) => {
            const open = openStory === i;
            return (
              <div key={i} onClick={() => setOpenStory(open ? -1 : i)} style={{ background: open ? "rgba(168,85,247,0.06)" : "rgba(255,255,255,0.03)", border: `0.5px solid ${open ? "rgba(168,85,247,0.28)" : T.border}`, borderRadius: 12, padding: "10px 12px", ...press }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <p style={{ fontSize: 12.5, fontWeight: 600, color: T.text, letterSpacing: "-0.2px", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{s.title}</p>
                    {s.metric && <p style={{ fontSize: 10.5, color: T.textTertiary, marginTop: 2 }}>{s.metric}</p>}
                  </div>
                  {s.tag && <span style={pill(T.violetSoft, "#c084fc")}>{s.tag}</span>}
                  <Icon name={open ? "chevUp" : "chevDown"} size={14} color={T.textTertiary} />
                </div>
                {open && (s.beats?.length ?? 0) > 0 && (
                  <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 6, paddingTop: 10, borderTop: `1px solid ${T.glassEdge}` }}>
                    {s.beats!.map((b, k) => (
                      <div key={k} style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
                        <span style={{ color: T.violet, fontSize: 12, lineHeight: 1.4, flexShrink: 0 }}>›</span>
                        <p style={{ fontSize: 11.5, lineHeight: 1.45, color: T.textSecondary }}>{b}</p>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          }))}

        {section === "facts" && (fFacts.length === 0
          ? <EmptySection text={q ? "No facts match." : "No facts yet."} />
          : fFacts.map((f, i) => (
            <div key={i} style={{ display: "flex", gap: 10, padding: "9px 12px", borderRadius: 11, background: "rgba(255,255,255,0.03)", border: `0.5px solid ${T.border}` }}>
              <div style={{ width: 3, borderRadius: 3, background: T.accent, flexShrink: 0 }} />
              <div>
                {f.k && <p style={{ fontSize: 11, fontWeight: 700, color: T.text, marginBottom: 2 }}>{f.k}</p>}
                <p style={{ fontSize: 11.5, lineHeight: 1.45, color: T.textSecondary }}>{f.v}</p>
              </div>
            </div>
          )))}

        {section === "questions" && (fQuestions.length === 0
          ? <EmptySection text={q ? "No questions match." : "No questions yet."} />
          : fQuestions.map((x, i) => (
            <div key={i} style={{ padding: "10px 12px", borderRadius: 12, background: "rgba(255,255,255,0.03)", border: `0.5px solid ${T.border}` }}>
              <div style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
                <Icon name="chevRight" size={13} color={T.accent} sw={2} />
                <p style={{ fontSize: 12.5, fontWeight: 600, color: T.text, lineHeight: 1.4, letterSpacing: "-0.15px" }}>{x.q}</p>
              </div>
              {x.why && <p style={{ fontSize: 10.5, lineHeight: 1.45, color: T.textTertiary, marginTop: 6, paddingLeft: 21 }}>{x.why}</p>}
            </div>
          )))}

        {cheat?.updatedAt && (
          <p style={{ fontSize: 9.5, color: T.textTertiary, textAlign: "center", marginTop: 4 }}>
            Updated {new Date(cheat.updatedAt).toLocaleString()}
          </p>
        )}
      </div>
    </div>
  );
}

const EmptySection = ({ text }: { text: string }) => (
  <div style={{ padding: "18px 12px", textAlign: "center" }}>
    <p style={{ fontSize: 11.5, color: T.textTertiary }}>{text}</p>
  </div>
);

// ── View: settings (privacy / stealth) ──────────────────────────────────────
const SToggle = ({ on, c = T.green }: { on: boolean; c?: string }) => (
  <div style={{ width: 34, height: 20, borderRadius: 100, background: on ? c : T.surface2, border: `0.5px solid ${on ? c : T.border}`, display: "flex", alignItems: "center", padding: 2, justifyContent: on ? "flex-end" : "flex-start", flexShrink: 0 }}>
    <div style={{ width: 15, height: 15, borderRadius: "50%", background: "#fff", boxShadow: "0 1px 3px rgba(0,0,0,0.4)" }} />
  </div>
);
const SRow = ({ icon, title, sub, on, c, last }: { icon: IconName; title: string; sub: string; on: boolean; c?: string; last?: boolean }) => (
  <div style={{ display: "flex", alignItems: "center", gap: 11, padding: "11px 4px", borderBottom: last ? "none" : `1px solid ${T.glassEdge}` }}>
    <div style={{ width: 30, height: 30, borderRadius: 9, flexShrink: 0, background: "rgba(255,255,255,0.05)", display: "flex", alignItems: "center", justifyContent: "center" }}>
      <Icon name={icon} size={14} color={c ?? T.textSecondary} />
    </div>
    <div style={{ flex: 1, minWidth: 0 }}>
      <p style={{ fontSize: 12.5, fontWeight: 600, color: T.text, letterSpacing: "-0.15px" }}>{title}</p>
      <p style={{ fontSize: 10.5, color: T.textTertiary, lineHeight: 1.4, marginTop: 1 }}>{sub}</p>
    </div>
    <SToggle on={on} c={c} />
  </div>
);
const STEALTH_CARD: Record<string, { c: string; title: string; sub: string }> = {
  excluded: { c: T.green, title: "Stealth mode is on", sub: "Invisible to Zoom, Meet, Teams screen-share & recordings. Hidden from Alt+Tab too." },
  monitor: { c: T.amber, title: "Limited stealth", sub: "Old Windows — the window shows as a black box in captures (content hidden, outline visible)." },
  none: { c: T.red, title: "Stealth is OFF", sub: "This window WILL appear in screen-share & recordings. Display-affinity cloak failed." },
  unknown: { c: T.textTertiary, title: "Checking stealth…", sub: "Reading capture-protection status from the OS." },
};
function SettingsBody({ cloak, opacity, onOpacity }: { cloak: CloakStatus; opacity: number; onOpacity: (v: number) => void }) {
  const card = STEALTH_CARD[cloak.mode] ?? STEALTH_CARD.unknown;
  const hidden = cloak.mode === "excluded" || cloak.mode === "monitor";
  return (
    <div style={{ padding: "13px 14px" }}>
      <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.1em", textTransform: "uppercase", color: T.textTertiary, paddingLeft: 2 }}>Appearance</span>
      <div style={{ marginTop: 6, marginBottom: 14, padding: "11px 12px", borderRadius: 12, background: "rgba(255,255,255,0.03)", border: `0.5px solid ${T.border}` }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 9 }}>
          <span style={{ fontSize: 12.5, fontWeight: 600, color: T.text, letterSpacing: "-0.15px" }}>Window opacity</span>
          <span style={{ fontSize: 11, color: T.textSecondary, fontVariantNumeric: "tabular-nums" }}>{Math.round(opacity * 100)}%</span>
        </div>
        {/* Mouse-driven — works on the no-activate overlay without taking focus. */}
        <input
          type="range" min={30} max={100} step={5} value={Math.round(opacity * 100)}
          onChange={(e) => onOpacity(parseInt(e.target.value, 10) / 100)}
          style={{ width: "100%", accentColor: T.accent, cursor: "default" }}
        />
        <p style={{ fontSize: 10.5, color: T.textTertiary, lineHeight: 1.45, marginTop: 7 }}>See-through HUD so you can read the call behind it. The stealth cloak still hides it from screen-share.</p>
      </div>
      <div style={{ background: `linear-gradient(135deg, ${card.c}1a, ${card.c}05)`, border: `0.5px solid ${card.c}4d`, borderRadius: 14, padding: "13px 14px", display: "flex", gap: 12, alignItems: "center", marginBottom: 12 }}>
        <div style={{ width: 38, height: 38, borderRadius: 11, flexShrink: 0, background: `${card.c}24`, display: "flex", alignItems: "center", justifyContent: "center" }}>
          <Icon name={cloak.mode === "none" ? "eye" : "eyeOff"} size={19} color={card.c} sw={1.9} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <p style={{ fontSize: 13, fontWeight: 700, color: T.text, letterSpacing: "-0.2px", fontFamily: T.fontDisplay }}>{card.title}</p>
          <p style={{ fontSize: 11, color: card.c, opacity: 0.92, lineHeight: 1.45, marginTop: 2 }}>{card.sub}</p>
        </div>
        <SToggle on={hidden} c={card.c} />
      </div>
      {cloak.remote && (
        <div style={{ display: "flex", gap: 9, alignItems: "flex-start", padding: "9px 11px", borderRadius: 11, background: "rgba(245,158,11,0.10)", border: `0.5px solid rgba(245,158,11,0.3)`, marginBottom: 14 }}>
          <Icon name="alert" size={14} color={T.amber} />
          <p style={{ fontSize: 10.5, color: "#e9c07a", lineHeight: 1.5 }}><b style={{ color: T.amber }}>Remote session detected.</b> An RDP/remote host streams the screen on its side and can bypass the cloak. Stealth is not guaranteed here.</p>
        </div>
      )}
      <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.1em", textTransform: "uppercase", color: T.textTertiary, paddingLeft: 2 }}>Capture</span>
      <div style={{ marginTop: 4 }}>
        <SRow icon="mic" title="Your microphone" sub="Powers question capture when Rec is on" on c={T.accent} />
        <SRow icon="camera" title="Screen capture" sub="Tap Capture to send a screenshot — never automatic" on={false} last />
      </div>
      <span style={{ fontSize: 10, fontWeight: 700, letterSpacing: "0.1em", textTransform: "uppercase", color: T.textTertiary, paddingLeft: 2, display: "block", marginTop: 14 }}>Privacy</span>
      <div style={{ marginTop: 4 }}>
        <SRow icon="monitor" title="Hide from screen-share" sub={cloak.mode === "excluded" ? "Excluded from capture (SetWindowDisplayAffinity)" : cloak.mode === "monitor" ? "Black-box fallback (old Windows)" : "Not active — cloak failed"} on={hidden} c={hidden ? card.c : T.red} />
        <SRow icon="monitor" title="Hidden from Alt+Tab" sub="Tool-window — not listed in the app switcher" on c={T.green} />
        <SRow icon="lock" title="Process on-device" sub="Audio handled by the local sidecar" on c={T.green} last />
      </div>
      <div style={{ marginTop: 12, display: "flex", gap: 8, alignItems: "flex-start", padding: "9px 11px", borderRadius: 11, background: "rgba(255,255,255,0.03)", border: `0.5px solid ${T.border}` }}>
        <Icon name="shield" size={13} color={T.textTertiary} />
        <p style={{ fontSize: 10.5, color: T.textTertiary, lineHeight: 1.5 }}>Cloak blocks software capture only — a phone camera or hardware capture card still sees the screen. Use Copilot only where permitted.</p>
      </div>
    </div>
  );
}

// ════════════════════════════════════════════════════════════════════════════
// Main — stateful, backend-wired
// ════════════════════════════════════════════════════════════════════════════
const STATE_LABEL: Record<View, string> = { live: "Listening", answer: "Answer ready", cheat: "Cheatsheet", settings: "Settings" };

export default function Copilot() {
  const [view, setView] = useState<View>("answer");
  const [recording, setRecording] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [listening, setListening] = useState(false);
  // Pipeline phase shown in the live view + drag bar so the user always knows
  // what's happening (listening → transcribing → thinking → answering).
  const [phase, setPhase] = useState<Phase>("idle");
  const [input, setInput] = useState("");
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState("");
  const [answering, setAnswering] = useState(false);
  const [lines, setLines] = useState<{ speaker: string; text: string; last: boolean }[]>([]);
  const [cloak, setCloak] = useState<CloakStatus>({ mode: "unknown", remote: false });
  const [jobLabel, setJobLabel] = useState("");
  const [cheat, setCheat] = useState<Cheatsheet | null>(null);
  const [cheatBusy, setCheatBusy] = useState(false);
  const [opacity, setOpacity] = useState<number>(() => {
    const v = parseFloat(localStorage.getItem("co-opacity") ?? "1");
    return v >= 0.3 && v <= 1 ? v : 1;
  });

  const credsRef = useRef<Creds>({ llmProvider: "gemini" });
  // The active job's research context, mirrored from the main window via Rust.
  // Refreshed right before each stream so the answer is grounded on the job the
  // user has selected *now* — works whether the overlay was opened by button or
  // the global hotkey, since the freshest copy always lives in Rust.
  const jobContextRef = useRef<string>("");
  // Latest grounding label, so the one-shot cheatsheet:updated listener can
  // confirm an incoming update is for the job currently shown.
  const jobLabelRef = useRef<string>("");
  const activeStream = useRef<string | null>(null);
  const recordingRef = useRef(false);
  const lastQARef = useRef<{ q: string; a: string }>({ q: "", a: "" });
  // Live capture level (RMS + pitch/ZCR), updated by the high-frequency
  // `voice:level` event. Held in a ref — the Wave meter reads it on a rAF loop
  // so the bars react to the voice without re-rendering React on every packet.
  const levelRef = useRef({ level: 0, pitch: 0 });
  // Token batching. Fast providers stream many tokens/sec; calling setAnswer per
  // token re-renders the whole overlay each time and freezes the UI on long
  // answers. Instead buffer incoming tokens and flush at most once per animation
  // frame (~60fps), so the answer still appears live but rendering is bounded.
  const tokBuf = useRef("");
  const tokRaf = useRef<number | null>(null);
  // Timer fallback for the token flush. This overlay is a never-foreground,
  // cloaked window, so Chromium can throttle/pause its rAF — which would leave
  // streamed tokens buffered but unrendered (stuck in "Thinking…"). A parallel
  // setTimeout guarantees the flush still fires; whichever wins cancels the other.
  const tokTimer = useRef<number | null>(null);

  // Inject keyframes + solid dark page surface once.
  useEffect(() => {
    if (!document.getElementById("co-anim")) {
      const s = document.createElement("style");
      s.id = "co-anim";
      s.textContent = `
        @keyframes co-bar{0%{transform:scaleY(0.28)}100%{transform:scaleY(1)}}
        @keyframes co-pulse{0%,100%{opacity:1;transform:scale(1)}50%{opacity:0.45;transform:scale(0.82)}}
        @keyframes co-sheen{0%{transform:translateX(-120%)}100%{transform:translateX(220%)}}
        @keyframes co-blink{0%,100%{opacity:1}50%{opacity:0.3}}`;
      document.head.appendChild(s);
    }
    const html = document.documentElement;
    const root = document.getElementById("root");
    html.style.background = T.glass;
    document.body.style.background = T.glass;
    if (root) root.style.background = T.glass;
  }, []);

  useEffect(() => {
    invoke<Creds>("load_credentials").then((c) => { credsRef.current = c; }).catch(() => {});
  }, []);

  // Pull the active job's research context + cheatsheet from Rust and fan it out
  // to the grounding label, the stream context ref, and the Cheatsheet tab.
  const refreshContext = async () => {
    try {
      const c = await invoke<CopilotCtx>("get_copilot_context");
      jobContextRef.current = c.context;
      jobLabelRef.current = c.label;
      setJobLabel(c.label);
      setCheat(c.cheatsheet ?? null);
      return c;
    } catch { return null; }
  };

  // Pull once on open so the grounding label + cheatsheet show immediately. It's
  // re-pulled before each stream too (runStream), covering a reused overlay
  // window where the user switched jobs while it stayed open.
  useEffect(() => { void refreshContext(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, []);

  // The main window finished (re)building a job's cheatsheet → apply it if it's
  // for the job we're showing, and clear the building state. The fresh sheet
  // rides in the payload (Rust mirror may not have committed yet).
  useEffect(() => {
    const unsubs: UnlistenFn[] = [];
    let alive = true;
    listen<{ label?: string; cheatsheet?: Cheatsheet; error?: string }>("cheatsheet:updated", (e) => {
      const p = e.payload;
      // Ignore updates for a different job than the one currently grounded.
      if (p.label && jobLabelRef.current && p.label !== jobLabelRef.current) return;
      setCheatBusy(false);
      if (p.cheatsheet) setCheat(p.cheatsheet);
    }).then((u) => { if (alive) unsubs.push(u); else u(); });
    return () => { alive = false; unsubs.forEach((u) => u()); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Re-apply a saved (non-default) opacity once the overlay is past its first
  // paint. Deferred beyond harden_new's 1200ms so flipping WS_EX_LAYERED can't
  // race the initial composition and blank the page.
  useEffect(() => {
    if (opacity >= 1) return;
    const t = setTimeout(() => invoke("copilot_set_opacity", { opacity }).catch(() => {}), 1400);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const applyOpacity = (v: number) => {
    setOpacity(v);
    localStorage.setItem("co-opacity", String(v));
    invoke("copilot_set_opacity", { opacity: v }).catch(() => {});
  };

  // Ask the main window to rebuild this job's cheatsheet (it owns the full
  // conversation corpus). It targets whatever job is selected there.
  const refreshCheatsheet = () => {
    if (cheatBusy) return;
    setCheatBusy(true);
    emit("cheatsheet:refresh", {}).catch(() => setCheatBusy(false));
  };

  // Poll the real OS-level cloak status so the badge never lies. Check on mount,
  // again shortly after (the window's affinity is applied right at creation),
  // and on a slow interval (catches RDP connect/disconnect mid-session).
  useEffect(() => {
    const check = () => invoke<CloakStatus>("copilot_cloak_status").then(setCloak).catch(() => {});
    check();
    const t = setTimeout(check, 700);
    const id = setInterval(check, 5000);
    return () => { clearTimeout(t); clearInterval(id); };
  }, []);

  // Rec timer.
  useEffect(() => {
    if (!recording) return;
    const id = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(id);
  }, [recording]);

  const fmt = (s: number) => `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;

  // Warm speech recognition as soon as the overlay opens. The copilot flow
  // never goes through the mock-interview "Preparing engine…" modal, so without
  // this the FIRST captured question pays the STT cold start (~13s of
  // "Transcribing…" — model load + VAD download) right in the middle of a live
  // interview. voice_prepare blocks server-side until ready, but we fire and
  // forget: the user opens the overlay well before the first question lands.
  // Idempotent + cheap when already warm.
  useEffect(() => {
    invoke("voice_prepare", { engine: "piper", speaker: "" }).catch(() => {});
  }, []);

  // Which source the current capture is from, so the transcript line is labelled
  // correctly: "system" = the interviewer's voice off the loopback (Rec button),
  // "mic" = the user speaking their own question (QuickAsk mic button).
  const listenSourceRef = useRef<"mic" | "system">("mic");
  // Mic one-shot — capture the user's own spoken question.
  const startListen = () => {
    listenSourceRef.current = "mic";
    invoke("voice_listen").catch(() => setListening(false));
  };
  // System-audio loop — capture the interviewer's question off the loopback. The
  // listen→transcribe→answer cycle re-arms this on every turn while Rec is on.
  const startSystemListen = () => {
    listenSourceRef.current = "system";
    invoke("voice_listen_system").catch(() => setListening(false));
  };

  // Drain the buffered tokens into the answer in one render. Safe to call
  // directly (final drain on done/error) or via rAF (per-frame during stream).
  const flushTokens = () => {
    if (tokRaf.current != null) { cancelAnimationFrame(tokRaf.current); tokRaf.current = null; }
    if (tokTimer.current != null) { clearTimeout(tokTimer.current); tokTimer.current = null; }
    if (!tokBuf.current) return;
    const chunk = tokBuf.current;
    tokBuf.current = "";
    setPhase("answering");
    setAnswer((a) => { const next = a + chunk; lastQARef.current.a = next; return next; });
  };

  // chat:* + voice:* listeners (StrictMode-safe).
  useEffect(() => {
    const unsubs: UnlistenFn[] = [];
    let alive = true;
    const reg = <P,>(ev: string, h: (p: P) => void) => {
      listen<P>(ev, (e) => h(e.payload)).then((u) => { if (alive) unsubs.push(u); else u(); });
    };

    reg<{ streamId: string; content: string }>("chat:token", (p) => {
      if (p.streamId !== activeStream.current) return;
      // Buffer, then flush to bound re-renders. Schedule via rAF (smooth while
      // visible) AND a timer fallback (in case rAF is throttled in the
      // backgrounded overlay); flushTokens cancels whichever loses the race.
      tokBuf.current += p.content;
      if (tokRaf.current == null) tokRaf.current = requestAnimationFrame(flushTokens);
      if (tokTimer.current == null) tokTimer.current = window.setTimeout(flushTokens, 50);
    });
    reg<{ streamId: string }>("chat:done", (p) => {
      if (p.streamId !== activeStream.current) return;
      flushTokens();                 // drain any buffered tail before finishing
      activeStream.current = null;
      setAnswering(false);
      setPhase(recordingRef.current ? "listening" : "idle");
      if (recordingRef.current) startSystemListen(); // re-arm for the next question
    });
    reg<{ streamId: string; content: string }>("chat:error", (p) => {
      if (p.streamId !== activeStream.current) return;
      flushTokens();
      activeStream.current = null;
      setAnswering(false);
      setAnswer((a) => a || `Error: ${p.content}`);
      setPhase(recordingRef.current ? "listening" : "idle");
      if (recordingRef.current) startSystemListen();
    });

    reg<{ level: number; pitch: number }>("voice:level", (p) => {
      // High-frequency: write to the ref only (the Wave meter reads it via rAF).
      levelRef.current = { level: p.level ?? 0, pitch: p.pitch ?? 0 };
    });
    reg<unknown>("voice:listening", () => { setListening(true); setPhase("listening"); });
    reg<unknown>("voice:transcribing", () => { setListening(false); setPhase("transcribing"); });
    reg<string>("voice:transcript", (text) => {
      setListening(false);
      const t = (text || "").trim();
      if (!t) { setPhase(recordingRef.current ? "listening" : "idle"); if (recordingRef.current) startSystemListen(); return; }
      const speaker = listenSourceRef.current === "system" ? "Interviewer" : "You";
      setLines((ls) => [...ls.map((l) => ({ ...l, last: false })), { speaker, text: t, last: true }]);
      ask(t);
    });
    reg<unknown>("voice:error", () => { setListening(false); setPhase(recordingRef.current ? "listening" : "idle"); if (recordingRef.current) startSystemListen(); });

    return () => { alive = false; unsubs.forEach((u) => u()); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const runStream = async (message: string, history: [string, string][]) => {
    if (activeStream.current) return;
    const sid = newStreamId();
    activeStream.current = sid;
    // Drop any tokens still buffered from a prior answer so they don't bleed in.
    if (tokRaf.current != null) { cancelAnimationFrame(tokRaf.current); tokRaf.current = null; }
    tokBuf.current = "";
    setAnswer("");
    setAnswering(true);
    setPhase("thinking");
    // During the Rec loop keep the user on the live view (transcript + status +
    // streamed answer all in one place). For a manual/typed ask, jump to the
    // dedicated Answer tab.
    if (!recordingRef.current) setView("answer");
    // Grab the freshest active-job context right before streaming so the answer
    // is grounded on the job the user is on now (it may have changed since open).
    await refreshContext();
    if (activeStream.current !== sid) return; // superseded/cancelled while awaiting
    invoke("start_chat_stream", {
      message, jobContext: jobContextRef.current, history, mode: "coach",
      llm: llmPayload(credsRef.current), documents: [], streamId: sid,
    }).catch((e) => {
      if (activeStream.current === sid) { activeStream.current = null; setAnswering(false); setAnswer(`Error: ${String(e)}`); }
    });
  };

  const ask = (q: string) => {
    const t = q.trim();
    if (!t) return;
    setQuestion(t);
    lastQARef.current = { q: t, a: "" };
    runStream(
      `You are an interview copilot. Draft a strong, concise spoken answer (first person, ~4-6 sentences) to this interview question:\n\n"${t}"`,
      [],
    );
  };

  const submit = () => { const t = input; setInput(""); ask(t); };

  const followUp = (instruction: string) => {
    const { q, a } = lastQARef.current;
    if (!q) return;
    runStream(instruction, [["user", q], ["assistant", a]]);
  };

  const toggleRec = () => {
    const next = !recording;
    setRecording(next);
    recordingRef.current = next;
    // Rec captures SYSTEM audio (the interviewer's voice via the meeting app),
    // endpoints on silence, transcribes the question, and drafts an answer —
    // then re-arms for the next question (see the chat:done handler).
    if (next) {
      setElapsed(0); setView("live"); setPhase("listening");
      // Re-fire the STT warmup (idempotent, ~instant when already warm) in case
      // the overlay sat open long enough for the sidecar to have restarted.
      invoke("voice_prepare", { engine: "piper", speaker: "" }).catch(() => {});
      startSystemListen();
    } else { invoke("voice_stop_listening").catch(() => {}); setListening(false); setPhase("idle"); }
  };

  const micOnce = () => { if (!listening) startListen(); };

  const capture = () => {
    // Screenshot capture isn't wired to a backend command yet — flash only.
    const el = document.getElementById("co-flash");
    if (el) { el.style.animation = "none"; void el.offsetWidth; el.style.animation = "co-flash .36s ease-out"; }
  };

  const win = getCurrentWebviewWindow();
  const placeholder = view === "cheat" ? "Ask Copilot to pull a story…" : view === "live" ? "Ask privately while they talk…" : "Ask anything…";

  return (
    <div style={{ height: "100vh", width: "100vw", background: T.glass, color: T.text, fontFamily: T.fontBody, display: "flex", flexDirection: "column", overflow: "hidden", borderLeft: `2px solid ${T.accent}`, boxSizing: "border-box", position: "relative" }}>
      <div id="co-flash" style={{ position: "absolute", inset: 0, background: "#fff", opacity: 0, zIndex: 60, pointerEvents: "none" }} />
      <style>{`@keyframes co-flash{0%{opacity:0}12%{opacity:0.9}100%{opacity:0}}`}</style>
      <DragBar state={phase === "idle" ? STATE_LABEL[view] : PHASE_UI[phase].label} job={jobLabel} cloak={cloak} onMin={() => win.minimize().catch(() => {})} onClose={() => win.close().catch(() => {})} />
      <NavBar view={view} setView={setView} recording={recording} onToggleRec={toggleRec} onCapture={capture} time={fmt(elapsed)} />
      <div style={{ flex: 1, overflow: "auto", minHeight: 0 }}>
        {view === "live" && <ListenBody phase={phase} time={fmt(elapsed)} lines={lines} levelRef={levelRef} question={question} answer={answer} answering={answering} onFinish={() => invoke("voice_finish_listening").catch(() => {})} />}
        {view === "answer" && <AnswerBody question={question} answer={answer} answering={answering} jobLabel={jobLabel} onRephrase={() => followUp("Rephrase that answer more concisely.")} onDeeper={() => followUp("Go deeper — expand that answer with a specific example and metrics.")} onCopy={() => navigator.clipboard.writeText(answer).catch(() => {})} />}
        {view === "cheat" && <CheatBody cheat={cheat} busy={cheatBusy} jobLabel={jobLabel} onRefresh={refreshCheatsheet} />}
        {view === "settings" && <SettingsBody cloak={cloak} opacity={opacity} onOpacity={applyOpacity} />}
      </div>
      {view !== "settings" && (
        <QuickAsk placeholder={placeholder} value={input} onChange={setInput} onSend={submit} busy={answering} listening={listening} onMic={micOnce} />
      )}
    </div>
  );
}
