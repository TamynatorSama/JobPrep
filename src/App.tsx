// ─── InterPrep — React + TypeScript port of the InterPrep.html design ──────
//
// This is a pixel-fidelity port of the Claude Design HTML prototype found at
// `.tmp_design/project/InterPrep.html`. Component structure mirrors the
// prototype intentionally so future styling tweaks can be cross-referenced
// against the source. State currently uses local mock data — the next phase
// wires backend modules (jobs store, credentials, recorder, Python sidecar,
// SSE chat stream) through Tauri IPC.

import {
  useState,
  useEffect,
  useRef,
  type CSSProperties,
  type ReactNode,
} from "react";
import { flushSync } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as pdfjs from "pdfjs-dist";
// pdf.js needs a worker. Vite bundles the worker script as a separate URL.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import mammoth from "mammoth";

pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

// ─── Resume file parsing ──────────────────────────────────────────────────

/** Extracts plain text from PDF/DOCX/MD/TXT. Throws on unknown extension. */
async function extractResumeText(file: File): Promise<string> {
  const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "txt" || ext === "md") {
    return await file.text();
  }
  if (ext === "docx") {
    const buf = await file.arrayBuffer();
    const result = await mammoth.extractRawText({ arrayBuffer: buf });
    return result.value;
  }
  if (ext === "pdf") {
    const buf = await file.arrayBuffer();
    const doc = await pdfjs.getDocument({ data: buf }).promise;
    const pages: string[] = [];
    for (let i = 1; i <= doc.numPages; i++) {
      const page = await doc.getPage(i);
      const content = await page.getTextContent();
      pages.push(
        content.items
          .map((it) => ("str" in it ? it.str : ""))
          .join(" "),
      );
    }
    return pages.join("\n\n");
  }
  throw new Error(`Unsupported file type: .${ext}`);
}

// ─── Backend lifecycle ─────────────────────────────────────────────────────

type BackendStatus =
  | { status: "starting" }
  | { status: "ready"; url?: string }
  | { status: "failed"; error: string };

// ─── Theme tokens ──────────────────────────────────────────────────────────

const T = {
  // Canvas & surfaces
  bg:           "#0C0C0C",
  surface:      "#1A1A1A",
  surface2:     "#242424",
  surfaceHover: "#222222",
  border:       "#1F1F1F",
  borderSoft:   "#161616",
  // Text — binary hierarchy: ink / ink-muted / ink-tertiary
  text:          "#FFFFFF",
  textSecondary: "#999999",
  textTertiary:  "#666666",
  // Accent — signal only
  accent:     "#0099FF",
  accentSoft: "rgba(0,153,255,0.12)",
  accentRing: "0 0 0 1px rgba(0,153,255,0.5)",
  // Sidebar
  sidebar:       "#0C0C0C",
  sidebarBorder: "#1F1F1F",
  inputBg:       "#1A1A1A",
  // Shadows — Framer elevation system
  shadow:   "0 1px 2px rgba(0,0,0,0.4)",
  shadowMd: "rgba(255,255,255,0.10) 0px 0.5px 0px inset, rgba(0,0,0,0.25) 0px 10px 30px",
  shadowLg: "0 20px 60px rgba(0,0,0,0.6)",
  // Gradient spotlights
  gradientViolet:  "linear-gradient(135deg, #4a1a8a 0%, #7c3aed 40%, #a855f7 70%, #c084fc 100%)",
  gradientMagenta: "linear-gradient(135deg, #831843 0%, #be185d 40%, #ec4899 70%, #f472b6 100%)",
  gradientOrange:  "linear-gradient(135deg, #7c2d12 0%, #c2410c 40%, #f97316 70%, #fb923c 100%)",
  gradientCoral:   "linear-gradient(135deg, #9f1239 0%, #e11d48 45%, #fb7185 80%, #fda4af 100%)",
  // Font stacks
  fontDisplay: "'Geist', 'Inter', sans-serif",
  fontBody:    "'Inter', sans-serif",
} as const;

type JobStatusKey = "Applied" | "Screening" | "Technical" | "Offer" | "Rejected";

const STATUS_CONFIG: Record<JobStatusKey, { color: string; bg: string; label: string }> = {
  Applied:   { color: "#F59E0B", bg: "rgba(245,158,11,0.12)", label: "Applied" },
  Screening: { color: "#0099FF", bg: "rgba(0,153,255,0.12)",  label: "Screening" },
  Technical: { color: "#a855f7", bg: "rgba(168,85,247,0.12)", label: "Technical" },
  Offer:     { color: "#22c55e", bg: "rgba(34,197,94,0.12)",  label: "Offer" },
  Rejected:  { color: "#EF4444", bg: "rgba(239,68,68,0.12)",  label: "Rejected" },
};

const STAGES = ["Applied", "Screen", "Technical 1", "Technical 2", "Final", "Offer"] as const;

// ─── Domain types ──────────────────────────────────────────────────────────

interface ChatMsg {
  role: "user" | "ai";
  content: string;
  /** Agent progress/tool-use lines shown in a collapsible disclosure above
   *  the bubble, like Claude's tool-use card. */
  logs?: string[];
  /** True while tokens are still streaming into `content`. */
  streaming?: boolean;
}

interface ChatThread {
  id: string;
  title: string;
  preview?: string;
  messages: ChatMsg[];
}

interface StageNote {
  date: string;
  outcome: string;
  notes: string;
}

interface Resume {
  id:   number;
  name: string;
  text: string;
}

interface Credentials {
  geminiApiKey:     string;
  glassdoorEmail:   string;
  glassdoorPassword:string;
  indeedEmail:      string;
  indeedPassword:   string;
}

const EMPTY_CREDS: Credentials = {
  geminiApiKey: "", glassdoorEmail: "", glassdoorPassword: "",
  indeedEmail: "", indeedPassword: "",
};

interface Scorecard {
  verbatim_match_score?: number;
  role_title_alignment?: "Yes" | "No";
  quantification_check?: "Pass" | "Needs Work";
  hire_recommendation?: "Hire" | "No Hire";
  skills_matched?: string[];
}

interface Job {
  id: string;
  company: string;
  role: string;
  location: string;
  url: string;
  status: JobStatusKey;
  appliedDate: string;
  currentStage: number;
  stageNotes: Record<number, StageNote>;
  avatar: string;
  avatarColor: string;
  chats: ChatThread[];
  /** Full job description text. Used as context for chat + research. */
  jobDescription?: string;
  /** Hidden from the main sidebar list once true. */
  archived?: boolean;
  /** Plain-text tailored resume produced by the Application-Prep step. */
  tailoredResume?: string;
  /** Absolute path to the generated `resume.docx` on disk. */
  resumeDocxPath?: string;
  /** ATS scorecard returned by the tailoring step. */
  scorecard?: Scorecard;
}

type Screen = "chat" | "timeline";

// ─── Seed sample data ──────────────────────────────────────────────────────

const INITIAL_JOBS: Job[] = [
  {
    id: "job1",
    company: "Stripe",
    role: "Software Engineer, Payments",
    location: "San Francisco, CA",
    url: "https://stripe.com/jobs",
    status: "Technical",
    appliedDate: "Apr 18, 2026",
    currentStage: 2,
    stageNotes: {
      0: { date: "Apr 18", outcome: "Submitted", notes: "Applied via referral from Alex K." },
      1: { date: "Apr 24", outcome: "Passed", notes: "Great chat with recruiter Sarah. Next steps in 1 week." },
      2: { date: "May 2",  outcome: "In progress", notes: "System design round scheduled May 8." },
    },
    avatar: "S",
    avatarColor: "#635BFF",
    chats: [
      {
        id: "c1",
        title: "System Design Prep",
        preview: "Let's practice distributed systems...",
        messages: [
          { role: "user", content: "Help me prep for a system design interview at Stripe. I'll be asked to design a payment processing system." },
          { role: "ai",   content: "Great choice to prepare for this! Stripe's system design interviews typically focus on scalability, reliability, and real-world tradeoffs. Here's how I'd approach designing a payment processing system:\n\n**1. Clarify Requirements**\n- Throughput: ~100K transactions/sec (peak)\n- Latency: <200ms p99\n- Durability: Every transaction must be recorded\n- Idempotency: Retry-safe operations\n\n**2. Core Components**\n- **API Gateway**: Rate limiting, auth, routing\n- **Payment Orchestrator**: Coordinates the transaction lifecycle\n- **Ledger Service**: Immutable, append-only transaction log\n- **Bank Connector**: Manages connections to card networks\n\nWant me to deep-dive into any of these components, or shall we practice the actual interview format?" },
        ],
      },
      {
        id: "c2",
        title: "Behavioral Questions",
        preview: "Tell me about a time you...",
        messages: [
          { role: "user", content: "What behavioral questions should I expect at Stripe?" },
          { role: "ai",   content: "Stripe is known for its \"operating principles\" culture. Their behavioral questions often probe for these key traits:\n\n**Move with urgency and focus**\n- \"Describe a time you shipped something under significant time pressure\"\n\n**Think rigorously**\n- \"Walk me through a complex technical decision you made. What data did you use?\"\n\n**Trust and amplify each other**\n- \"Tell me about a time you disagreed with a teammate. How did you resolve it?\"\n\n**Act like an owner**\n- \"Describe a time you identified and fixed a problem outside your direct responsibility\"\n\nI'd suggest preparing 4–5 strong STAR stories that you can adapt to these themes. Want me to help you craft and refine any specific story?" },
        ],
      },
    ],
  },
  {
    id: "job2",
    company: "Anthropic",
    role: "ML Engineer, Alignment",
    location: "San Francisco, CA",
    url: "https://anthropic.com/careers",
    status: "Applied",
    appliedDate: "May 1, 2026",
    currentStage: 0,
    stageNotes: {
      0: { date: "May 1", outcome: "Submitted", notes: "Applied cold. Strong cover letter." },
    },
    avatar: "A",
    avatarColor: "#D97757",
    chats: [
      {
        id: "c3",
        title: "Job Description Analysis",
        preview: "Key requirements and red flags...",
        messages: [
          { role: "user", content: "Analyze this job description for ML Engineer at Anthropic and tell me what they're really looking for." },
          { role: "ai",   content: "Based on the Anthropic ML Engineer (Alignment) role, here's my analysis:\n\n**What they're explicitly asking for:**\n- Strong ML fundamentals (transformers, RLHF, fine-tuning)\n- Research engineering: turning papers into working systems\n- Python + PyTorch fluency\n- Experience with large-scale training runs\n\n**What they're implicitly signaling:**\n- They want someone who cares about the *why*, not just the *how* — alignment curiosity is essential\n- \"Works well with ambiguity\" = research taste, not just execution\n- \"Fast learner\" = codebase is probably messy and evolving quickly\n\n**Your strongest alignment points:**\n- Distributed training experience ✓\n- Prior RLHF project work ✓\n\n**Gap to address:**\n- Emphasize any experience with model evaluation frameworks or red-teaming\n\nWant me to help tailor your resume bullets for this role?" },
        ],
      },
    ],
  },
];

// ─── Icon system ───────────────────────────────────────────────────────────

interface IconProps {
  name: IconName;
  size?: number;
  color?: string;
  strokeWidth?: number;
}

type IconName =
  | "plus" | "folder" | "chat" | "settings" | "chevronRight" | "chevronDown"
  | "chevronLeft" | "moreHoriz" | "send" | "attach" | "mic" | "copy"
  | "refresh" | "bookmark" | "sparkle" | "zap" | "search" | "x" | "check"
  | "sun" | "moon" | "upload" | "briefcase" | "edit" | "trash" | "user"
  | "bell" | "layout" | "mapPin" | "calendar" | "link" | "note" | "analyze"
  | "interview" | "panel" | "archive" | "key" | "eye" | "eyeOff";

const Icon = ({ name, size = 16, color = "currentColor", strokeWidth = 1.75 }: IconProps) => {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: color,
    strokeWidth,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  const paths: Record<IconName, ReactNode> = {
    plus:         <><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></>,
    folder:       <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>,
    chat:         <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>,
    settings:     <><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></>,
    chevronRight: <polyline points="9 18 15 12 9 6"/>,
    chevronDown:  <polyline points="6 9 12 15 18 9"/>,
    chevronLeft:  <polyline points="15 18 9 12 15 6"/>,
    moreHoriz:    <><circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/></>,
    send:         <><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></>,
    attach:       <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48"/>,
    mic:          <><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></>,
    copy:         <><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></>,
    refresh:      <><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></>,
    bookmark:     <path d="m19 21-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z"/>,
    sparkle:      <path d="m12 3-1.912 5.813a2 2 0 0 1-1.275 1.275L3 12l5.813 1.912a2 2 0 0 1 1.275 1.275L12 21l1.912-5.813a2 2 0 0 1 1.275-1.275L21 12l-5.813-1.912a2 2 0 0 1-1.275-1.275L12 3Z"/>,
    zap:          <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>,
    search:       <><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></>,
    x:            <><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></>,
    check:        <polyline points="20 6 9 17 4 12"/>,
    sun:          <><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></>,
    moon:         <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>,
    upload:       <><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></>,
    briefcase:    <><rect x="2" y="7" width="20" height="14" rx="2" ry="2"/><path d="M16 21V5a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16"/></>,
    edit:         <><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></>,
    trash:        <><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></>,
    user:         <><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></>,
    bell:         <><path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"/><path d="M13.73 21a2 2 0 0 1-3.46 0"/></>,
    layout:       <><rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><line x1="3" y1="9" x2="21" y2="9"/><line x1="9" y1="21" x2="9" y2="9"/></>,
    mapPin:       <><path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z"/><circle cx="12" cy="10" r="3"/></>,
    calendar:     <><rect x="3" y="4" width="18" height="18" rx="2" ry="2"/><line x1="16" y1="2" x2="16" y2="6"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="3" y1="10" x2="21" y2="10"/></>,
    link:         <><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></>,
    note:         <><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/></>,
    analyze:      <><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></>,
    interview:    <><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></>,
    panel:        <><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="3" x2="9" y2="21"/></>,
    archive:      <><polyline points="21 8 21 21 3 21 3 8"/><rect x="1" y="3" width="22" height="5"/><line x1="10" y1="12" x2="14" y2="12"/></>,
    key:          <><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></>,
    eye:          <><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></>,
    eyeOff:       <><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"/><line x1="1" y1="1" x2="23" y2="23"/></>,
  };
  return <svg {...common}>{paths[name]}</svg>;
};

// ─── Markdown renderer ─────────────────────────────────────────────────────

interface MarkdownTextProps { content: string; }

const MarkdownText = ({ content }: MarkdownTextProps) => {
  const lines = content.split("\n");
  const elements: ReactNode[] = [];

  const renderInlineBold = (text: string, key: string | number): ReactNode[] =>
    text.split(/(\*\*[^*]+\*\*)/g).map((part, j) =>
      part.startsWith("**")
        ? <strong key={`${key}-b-${j}`}>{part.slice(2, -2)}</strong>
        : <span key={`${key}-s-${j}`}>{part}</span>,
    );

  lines.forEach((line, i) => {
    if (line.startsWith("**") && line.endsWith("**") && line.length > 4) {
      elements.push(
        <p key={i} style={{ fontWeight: 700, color: T.text, marginBottom: 2 }}>
          {line.slice(2, -2)}
        </p>,
      );
    } else if (line.startsWith("- ") || line.startsWith("• ")) {
      elements.push(
        <div key={i} style={{ display: "flex", gap: 8, marginBottom: 2 }}>
          <span style={{ color: T.accent, flexShrink: 0, marginTop: 2 }}>›</span>
          <span>{renderInlineBold(line.slice(2), i)}</span>
        </div>,
      );
    } else if (line.startsWith("---")) {
      elements.push(<hr key={i} style={{ border: "none", borderTop: `1px solid ${T.border}`, margin: "12px 0" }} />);
    } else if (line === "") {
      elements.push(<div key={i} style={{ height: 8 }} />);
    } else {
      elements.push(
        <p key={i} style={{ marginBottom: 2 }}>{renderInlineBold(line, i)}</p>,
      );
    }
  });

  return (
    <div style={{ fontSize: 14, lineHeight: 1.65, color: T.textSecondary }}>
      {elements}
    </div>
  );
};

// ─── Small style helper for popup menu rows ───────────────────────────────

const menuItemStyle = (color: string = T.textSecondary): CSSProperties => ({
  display: "flex", alignItems: "center", gap: 8,
  width: "100%", padding: "6px 10px", borderRadius: 6,
  border: "none", background: "transparent",
  color, fontSize: 12, fontWeight: 500,
  cursor: "pointer", textAlign: "left",
  fontFamily: T.fontBody, letterSpacing: "-0.12px",
});

// ─── Sidebar ───────────────────────────────────────────────────────────────

interface SidebarProps {
  jobs: Job[];
  selectedJobId: string | null;
  selectedChatId: string | null;
  onSelectJob: (id: string) => void;
  onSelectChat: (id: string | null) => void;
  onNewJob: () => void;
  onSettings: () => void;
  onArchiveJob: (id: string) => void;
  onUnarchiveJob: (id: string) => void;
  onDeleteJob: (id: string) => void;
  collapsed: boolean;
  activeScreen: Screen;
  onSetScreen: (s: Screen) => void;
}

const Sidebar = (p: SidebarProps) => {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({ job1: true, job2: false });
  const [hovered, setHovered]   = useState<string | null>(null);
  /** Job id whose kebab popup is currently open, or `null`. */
  const [menuFor, setMenuFor]   = useState<string | null>(null);
  /** Whether the "Archived (N)" disclosure is expanded. */
  const [showArchived, setShowArchived] = useState(false);

  // Close any open popup when the user clicks outside the sidebar.
  useEffect(() => {
    if (!menuFor) return;
    const close = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (!t.closest("[data-job-menu]")) setMenuFor(null);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menuFor]);

  const activeJobs   = p.jobs.filter((j) => !j.archived);
  const archivedJobs = p.jobs.filter((j) =>  j.archived);

  const toggle = (id: string) => setExpanded((f) => ({ ...f, [id]: !f[id] }));

  interface NavBtnProps { icon: IconName; label: string; screen: Screen; }
  const NavBtn = ({ icon, label, screen }: NavBtnProps) => {
    const active = p.activeScreen === screen;
    return (
      <button
        onClick={() => p.onSetScreen(screen)}
        onMouseEnter={(e) => { if (!active) e.currentTarget.style.background = T.surfaceHover; }}
        onMouseLeave={(e) => { if (!active) e.currentTarget.style.background = "none"; }}
        style={{
          display: "flex", alignItems: "center", gap: 9,
          padding: p.collapsed ? "9px 0" : "8px 12px",
          borderRadius: 100, border: "none",
          background: active ? T.surface : "none",
          color: active ? T.text : T.textSecondary,
          fontSize: 13, fontWeight: active ? 500 : 400,
          cursor: "pointer", width: "100%",
          fontFamily: T.fontBody,
          justifyContent: p.collapsed ? "center" : "flex-start",
          letterSpacing: "-0.13px",
        }}
      >
        <Icon name={icon} size={14} color={active ? T.text : T.textSecondary} />
        {!p.collapsed && label}
      </button>
    );
  };

  return (
    <div style={{
      width: p.collapsed ? 64 : 280,
      flexShrink: 0,
      background: T.sidebar,
      borderRight: `1px solid ${T.sidebarBorder}`,
      display: "flex", flexDirection: "column",
      height: "100%", overflow: "hidden",
      transition: "width 0.25s cubic-bezier(0.4,0,0.2,1)",
    }}>
      {/* Logo */}
      <div style={{
        padding: p.collapsed ? "16px 0" : "16px 16px",
        display: "flex", alignItems: "center", gap: 10,
        borderBottom: `1px solid ${T.border}`,
        justifyContent: p.collapsed ? "center" : "flex-start",
      }}>
        <div style={{
          width: 28, height: 28, borderRadius: 8, background: "#fff",
          display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
        }}>
          <Icon name="sparkle" size={14} color="#0C0C0C" strokeWidth={2.2} />
        </div>
        {!p.collapsed && (
          <span style={{
            fontSize: 14, fontWeight: 700, color: T.text,
            letterSpacing: "-0.5px", fontFamily: T.fontDisplay,
          }}>InterPrep</span>
        )}
      </div>

      {/* Top nav */}
      <div style={{ padding: p.collapsed ? "8px 6px" : "8px 8px", borderBottom: `1px solid ${T.border}` }}>
        {!p.collapsed && (
          <button onClick={p.onNewJob} style={{
            width: "100%", height: 36, borderRadius: 100, border: "none",
            background: "#fff", color: "#0C0C0C",
            fontSize: 13, fontWeight: 500, cursor: "pointer",
            display: "flex", alignItems: "center", justifyContent: "center", gap: 7,
            fontFamily: T.fontBody, marginBottom: 8, letterSpacing: "-0.14px",
          }}>
            <Icon name="plus" size={14} color="#0C0C0C" strokeWidth={2.5} />
            New Job
          </button>
        )}
        <NavBtn icon="calendar" label="Timeline" screen="timeline" />
        <NavBtn icon="chat"     label="Research" screen="chat" />
      </div>

      {!p.collapsed && p.activeScreen === "chat" && (
        <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden" }}>
          <div style={{ padding: "10px 12px 4px" }}>
            <p style={{
              fontSize: 11, fontWeight: 700, color: T.textTertiary,
              letterSpacing: "0.06em", textTransform: "uppercase",
              marginBottom: 4, paddingLeft: 4,
            }}>Job Research</p>
          </div>
          <div style={{ flex: 1, overflowY: "auto", padding: "0 8px 8px" }}>
            {activeJobs.map((job) => (
              <div key={job.id} style={{ marginBottom: 1 }}>
                <div
                  onClick={() => { toggle(job.id); p.onSelectJob(job.id); }}
                  onMouseEnter={() => setHovered(`f-${job.id}`)}
                  onMouseLeave={() => setHovered(null)}
                  style={{
                    display: "flex", alignItems: "center", gap: 7,
                    padding: "7px 10px", borderRadius: 8, cursor: "pointer",
                    position: "relative",
                    background:
                      p.selectedJobId === job.id && !expanded[job.id]
                        ? T.surface
                        : hovered === `f-${job.id}` || menuFor === job.id
                          ? T.surfaceHover : "transparent",
                  }}
                >
                  <span style={{ color: T.textTertiary, display: "flex" }}>
                    <Icon name={expanded[job.id] ? "chevronDown" : "chevronRight"} size={12} />
                  </span>
                  <div style={{
                    width: 18, height: 18, borderRadius: 5,
                    background: `${job.avatarColor}20`,
                    border: `0.5px solid ${job.avatarColor}40`,
                    display: "flex", alignItems: "center", justifyContent: "center",
                    fontSize: 10, fontWeight: 700, color: job.avatarColor, flexShrink: 0,
                  }}>{job.avatar}</div>
                  <span style={{
                    fontSize: 12, fontWeight: 500, color: T.text, flex: 1,
                    whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                    letterSpacing: "-0.12px",
                  }}>{job.company}</span>

                  {/* Kebab + status pill swap based on hover state. */}
                  {hovered === `f-${job.id}` || menuFor === job.id ? (
                    <button
                      data-job-menu
                      onClick={(e) => {
                        e.stopPropagation();
                        setMenuFor(menuFor === job.id ? null : job.id);
                      }}
                      style={{
                        width: 22, height: 22, borderRadius: 6, border: "none",
                        background: menuFor === job.id ? T.surface2 : "transparent",
                        color: T.textSecondary, cursor: "pointer",
                        display: "flex", alignItems: "center", justifyContent: "center",
                        flexShrink: 0, padding: 0,
                      }}
                    >
                      <Icon name="moreHoriz" size={13} />
                    </button>
                  ) : (
                    <span style={{
                      fontSize: 10, fontWeight: 500,
                      padding: "2px 7px", borderRadius: 100,
                      background: STATUS_CONFIG[job.status].bg,
                      color: STATUS_CONFIG[job.status].color,
                      flexShrink: 0, letterSpacing: "-0.1px",
                    }}>{job.status}</span>
                  )}

                  {/* Popup menu — Archive / Delete. */}
                  {menuFor === job.id && (
                    <div
                      data-job-menu
                      onClick={(e) => e.stopPropagation()}
                      style={{
                        position: "absolute", top: "100%", right: 4, marginTop: 2,
                        background: T.surface, border: `0.5px solid ${T.border}`,
                        borderRadius: 10, padding: 4, boxShadow: T.shadowLg,
                        zIndex: 60, minWidth: 150,
                      }}
                    >
                      <button
                        onClick={() => { p.onArchiveJob(job.id); setMenuFor(null); }}
                        style={menuItemStyle()}
                        onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
                        onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                      >
                        <Icon name="archive" size={12} /> Archive
                      </button>
                      <button
                        onClick={() => { p.onDeleteJob(job.id); setMenuFor(null); }}
                        style={menuItemStyle("#EF4444")}
                        onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
                        onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                      >
                        <Icon name="trash" size={12} /> Delete
                      </button>
                    </div>
                  )}
                </div>
                {expanded[job.id] && (
                  <div style={{ paddingLeft: 24 }}>
                    {job.chats.map((chat) => {
                      const isActive = p.selectedChatId === chat.id && p.selectedJobId === job.id;
                      return (
                        <div
                          key={chat.id}
                          onClick={() => { p.onSelectJob(job.id); p.onSelectChat(chat.id); }}
                          onMouseEnter={() => setHovered(`c-${chat.id}`)}
                          onMouseLeave={() => setHovered(null)}
                          style={{
                            display: "flex", alignItems: "center", gap: 7,
                            padding: "6px 10px", borderRadius: 8, cursor: "pointer",
                            background: isActive ? T.surface : hovered === `c-${chat.id}` ? T.surfaceHover : "transparent",
                            marginBottom: 1,
                          }}
                        >
                          <Icon name="chat" size={11} color={isActive ? T.text : T.textTertiary} />
                          <div style={{ flex: 1, minWidth: 0 }}>
                            <p style={{
                              fontSize: 12, fontWeight: isActive ? 500 : 400,
                              color: isActive ? T.text : T.textSecondary,
                              whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                              letterSpacing: "-0.12px",
                            }}>{chat.title}</p>
                          </div>
                        </div>
                      );
                    })}
                    <div
                      onMouseEnter={() => setHovered(`new-${job.id}`)}
                      onMouseLeave={() => setHovered(null)}
                      onClick={() => { p.onSelectJob(job.id); p.onSelectChat(null); }}
                      style={{
                        display: "flex", alignItems: "center", gap: 7,
                        padding: "5px 10px", borderRadius: 8, cursor: "pointer",
                        background: hovered === `new-${job.id}` ? T.surfaceHover : "transparent",
                        marginBottom: 1,
                      }}
                    >
                      <Icon name="plus" size={11} color={T.textTertiary} />
                      <p style={{ fontSize: 11, color: T.textTertiary, letterSpacing: "-0.11px" }}>New chat</p>
                    </div>
                  </div>
                )}
              </div>
            ))}

            {/* ── Archived (N) disclosure ───────────────────────────────── */}
            {archivedJobs.length > 0 && (
              <div style={{ marginTop: 8 }}>
                <div
                  onClick={() => setShowArchived((v) => !v)}
                  onMouseEnter={(e) => { e.currentTarget.style.background = T.surfaceHover; }}
                  onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                  style={{
                    display: "flex", alignItems: "center", gap: 7,
                    padding: "7px 10px", borderRadius: 8, cursor: "pointer",
                  }}
                >
                  <span style={{ color: T.textTertiary, display: "flex" }}>
                    <Icon name={showArchived ? "chevronDown" : "chevronRight"} size={12} />
                  </span>
                  <span style={{
                    fontSize: 11, color: T.textTertiary, flex: 1,
                    letterSpacing: "-0.11px",
                  }}>Archived ({archivedJobs.length})</span>
                </div>

                {showArchived && archivedJobs.map((job) => (
                  <div
                    key={job.id}
                    onMouseEnter={() => setHovered(`a-${job.id}`)}
                    onMouseLeave={() => setHovered(null)}
                    style={{
                      display: "flex", alignItems: "center", gap: 7,
                      padding: "6px 10px 6px 22px", borderRadius: 8,
                      background: hovered === `a-${job.id}` ? T.surfaceHover : "transparent",
                    }}
                  >
                    <div style={{
                      width: 16, height: 16, borderRadius: 4,
                      background: `${job.avatarColor}18`,
                      display: "flex", alignItems: "center", justifyContent: "center",
                      fontSize: 9, fontWeight: 700, color: job.avatarColor,
                      flexShrink: 0, opacity: 0.7,
                    }}>{job.avatar}</div>
                    <span style={{
                      fontSize: 12, color: T.textTertiary, flex: 1,
                      whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                    }}>{job.company}</span>
                    <button
                      onClick={() => p.onUnarchiveJob(job.id)}
                      title="Restore"
                      onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; e.currentTarget.style.color = T.text; }}
                      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; e.currentTarget.style.color = T.textSecondary; }}
                      style={{
                        width: 22, height: 22, borderRadius: 6, border: "none",
                        background: "transparent", color: T.textSecondary,
                        cursor: "pointer", display: "flex",
                        alignItems: "center", justifyContent: "center",
                        flexShrink: 0, padding: 0,
                      }}
                    >
                      <Icon name="refresh" size={11} />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {p.collapsed && <div style={{ flex: 1 }} />}

      {/* Settings */}
      <div style={{ padding: p.collapsed ? "8px 6px" : "8px 8px", borderTop: `1px solid ${T.border}` }}>
        <button
          onClick={p.onSettings}
          onMouseEnter={(e) => { e.currentTarget.style.background = T.surfaceHover; }}
          onMouseLeave={(e) => { e.currentTarget.style.background = "none"; }}
          style={{
            display: "flex", alignItems: "center", gap: 9,
            padding: p.collapsed ? "9px 0" : "8px 12px",
            borderRadius: 100, border: "none", background: "none",
            color: T.textSecondary, fontSize: 13, fontWeight: 500,
            cursor: "pointer", width: "100%", fontFamily: T.fontBody,
            justifyContent: p.collapsed ? "center" : "flex-start",
          }}
        >
          <Icon name="settings" size={15} />
          {!p.collapsed && "Settings"}
        </button>
      </div>
    </div>
  );
};

// ─── Workspace header ─────────────────────────────────────────────────────

// ─── Backend status badge ─────────────────────────────────────────────────

const BackendBadge = ({ status }: { status: BackendStatus }) => {
  const palette =
    status.status === "ready"
      ? { dot: "#22c55e", text: T.textSecondary, label: "Backend ready" }
      : status.status === "failed"
      ? { dot: "#EF4444", text: "#EF4444",       label: "Backend failed" }
      : { dot: "#F59E0B", text: T.textSecondary, label: "Starting…" };

  const title = status.status === "failed" ? status.error : palette.label;

  return (
    <div
      title={title}
      style={{
        display: "flex", alignItems: "center", gap: 5,
        padding: "3px 9px", borderRadius: 100,
        background: T.surface, border: `0.5px solid ${T.border}`,
        fontSize: 11, color: palette.text, flexShrink: 0,
        letterSpacing: "-0.11px", whiteSpace: "nowrap",
      }}
    >
      <span style={{
        width: 6, height: 6, borderRadius: "50%", background: palette.dot,
        animation: status.status === "starting" ? "pulse 1.4s ease-in-out infinite" : undefined,
      }} />
      {palette.label}
      <style>{`@keyframes pulse { 0%,100%{opacity:1} 50%{opacity:0.4} }`}</style>
    </div>
  );
};

interface WorkspaceHeaderProps {
  job: Job | null;
  onToggleSidebar: () => void;
  backend: BackendStatus;
}

const WorkspaceHeader = ({ job, onToggleSidebar, backend }: WorkspaceHeaderProps) => {
  const st = job ? STATUS_CONFIG[job.status] : null;
  const headerRef = useRef<HTMLDivElement>(null);
  const [headerWidth, setHeaderWidth] = useState(900);

  useEffect(() => {
    if (!headerRef.current) return;
    const ro = new ResizeObserver((entries) => setHeaderWidth(entries[0].contentRect.width));
    ro.observe(headerRef.current);
    return () => ro.disconnect();
  }, []);

  const showMeta   = headerWidth > 600;
  const showStatus = headerWidth > 480;
  const showLabels = headerWidth > 720;

  const actions: { icon: IconName; label: string; color: string }[] = [
    { icon: "interview", label: "Mock Interview", color: "#a855f7" },
    { icon: "note",      label: "Add Note",       color: "#F59E0B" },
  ];

  return (
    <div
      ref={headerRef}
      style={{
        height: 56, flexShrink: 0,
        borderBottom: `1px solid ${T.border}`,
        display: "flex", alignItems: "center",
        padding: "0 16px", gap: 10,
        background: T.bg, position: "relative", zIndex: 2, overflow: "hidden",
      }}
    >
      <button
        onClick={onToggleSidebar}
        onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
        onMouseLeave={(e) => { e.currentTarget.style.background = T.surface; }}
        style={{
          width: 30, height: 30, borderRadius: 100,
          border: `0.5px solid ${T.border}`, background: T.surface,
          cursor: "pointer",
          display: "flex", alignItems: "center", justifyContent: "center",
          color: T.textSecondary, flexShrink: 0,
        }}
      >
        <Icon name="panel" size={13} />
      </button>

      {job ? (
        <>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flex: 1, minWidth: 0, overflow: "hidden" }}>
            <div style={{
              width: 30, height: 30, borderRadius: 8,
              background: `${job.avatarColor}18`,
              border: `0.5px solid ${job.avatarColor}44`,
              display: "flex", alignItems: "center", justifyContent: "center",
              fontSize: 12, fontWeight: 700, color: job.avatarColor, flexShrink: 0,
              fontFamily: T.fontDisplay,
            }}>{job.avatar}</div>
            <div style={{ minWidth: 0, flex: 1 }}>
              <h1 style={{
                fontSize: 13, fontWeight: 600, color: T.text,
                whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                letterSpacing: "-0.3px", fontFamily: T.fontDisplay,
              }}>{job.company}</h1>
              <p style={{
                fontSize: 11, color: T.textSecondary,
                whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                letterSpacing: "-0.11px",
              }}>{job.role}</p>
            </div>
            {showMeta && (
              <div style={{ display: "flex", alignItems: "center", gap: 10, flexShrink: 0 }}>
                {job.location && (
                  <span style={{ fontSize: 11, color: T.textTertiary, display: "flex", alignItems: "center", gap: 3, whiteSpace: "nowrap" }}>
                    <Icon name="mapPin" size={10} />{job.location}
                  </span>
                )}
                <span style={{ fontSize: 11, color: T.textTertiary, display: "flex", alignItems: "center", gap: 3, whiteSpace: "nowrap" }}>
                  <Icon name="calendar" size={10} />{job.appliedDate}
                </span>
              </div>
            )}
          </div>

          {showStatus && st && (
            <div style={{
              padding: "3px 9px", borderRadius: 100,
              background: st.bg, color: st.color,
              fontSize: 11, fontWeight: 500, flexShrink: 0,
              letterSpacing: "-0.11px", whiteSpace: "nowrap",
            }}>{job.status}</div>
          )}

          <BackendBadge status={backend} />


          <div style={{ display: "flex", gap: 4, flexShrink: 0 }}>
            {actions.map((a) => (
              <button
                key={a.label}
                onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; e.currentTarget.style.color = T.text; }}
                onMouseLeave={(e) => { e.currentTarget.style.background = T.surface; e.currentTarget.style.color = T.textSecondary; }}
                style={{
                  height: 30, padding: showLabels ? "0 11px" : "0 9px",
                  borderRadius: 100, border: "none",
                  background: T.surface, color: T.textSecondary,
                  fontSize: 12, fontWeight: 500, cursor: "pointer",
                  display: "flex", alignItems: "center", gap: 5,
                  fontFamily: T.fontBody, whiteSpace: "nowrap", letterSpacing: "-0.12px",
                }}
              >
                <Icon name={a.icon} size={12} />
                {showLabels && <span>{a.label}</span>}
              </button>
            ))}
          </div>
        </>
      ) : (
        <p style={{ fontSize: 14, fontWeight: 600, color: T.text, letterSpacing: "-0.3px", fontFamily: T.fontDisplay }}>
          InterPrep
        </p>
      )}
    </div>
  );
};

// ─── Gantt timeline ───────────────────────────────────────────────────────

const MONTH_NAMES = [
  "January","February","March","April","May","June",
  "July","August","September","October","November","December",
];
const STAGE_COLORS = ["#F59E0B","#0099FF","#a855f7","#a855f7","#ec4899","#22c55e"];
const STAGES_GANTT = ["Applied","Screen","Technical 1","Technical 2","Final","Offer"];
const DAY_MS = 86_400_000;

interface GanttProps {
  jobs: Job[];
  onSelectJob: (id: string) => void;
  onNewJob: () => void;
  onToggleSidebar: () => void;
  onUpdateJob: (id: string, updater: (j: Job) => Job) => void;
}

const Gantt = (p: GanttProps) => {
  // Today is read once on mount. If we re-read it every render the "today"
  // column would jitter when nothing meaningful changed.
  const [todayRef] = useState<Date>(() => new Date());
  const ROW_HEIGHT = 64;
  const LEFT_COL   = 200;
  const DAY_W      = 36;
  const TOTAL_DAYS = 60;

  /** Days before today the view starts on. Negative values pan into the past. */
  const [startOffset, setStartOffset] = useState(-14);

  // Editor popover state: which stage on which job is being edited.
  const [editing, setEditing] = useState<{
    jobId: string; stageIdx: number; anchor: { x: number; y: number };
  } | null>(null);

  const viewStart = new Date(todayRef.getTime() + startOffset * DAY_MS);
  const viewEnd   = new Date(viewStart.getTime() + TOTAL_DAYS * DAY_MS);

  // Build day columns.
  const days: Date[] = [];
  for (let i = 0; i < TOTAL_DAYS; i++) {
    days.push(new Date(viewStart.getTime() + i * DAY_MS));
  }

  // Group days by month for the header row.
  const monthGroups: { label: string; start: number; count: number }[] = [];
  days.forEach((d, i) => {
    const label = `${MONTH_NAMES[d.getMonth()].slice(0,3)} ${d.getFullYear()}`;
    const last = monthGroups[monthGroups.length - 1];
    if (!last || last.label !== label) {
      monthGroups.push({ label, start: i, count: 1 });
    } else {
      last.count++;
    }
  });

  const dateToX = (d: Date) =>
    LEFT_COL + ((d.getTime() - viewStart.getTime()) / DAY_MS) * DAY_W;
  const todayX = dateToX(todayRef);

  const parseStageDate = (s: string): Date | null => {
    if (!s) return null;
    const months: Record<string, number> = {
      Jan:0,Feb:1,Mar:2,Apr:3,May:4,Jun:5,
      Jul:6,Aug:7,Sep:8,Oct:9,Nov:10,Dec:11,
    };
    const parts = s.replace(",", "").trim().split(" ");
    if (parts.length >= 2 && months[parts[0]] !== undefined) {
      const year = parts[2] ? parseInt(parts[2]) : todayRef.getFullYear();
      return new Date(year, months[parts[0]], parseInt(parts[1]));
    }
    return null;
  };

  const handleSaveStage = (jobId: string, stageIdx: number, data: StageNote) => {
    p.onUpdateJob(jobId, (job) => ({
      ...job,
      currentStage: Math.max(job.currentStage, stageIdx),
      stageNotes: { ...job.stageNotes, [stageIdx]: data },
    }));
    setEditing(null);
  };

  const activeJobs = p.jobs.filter((j) => !j.archived);

  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", background: T.bg }}>
      {/* ── Toolbar ────────────────────────────────────────────────── */}
      <div style={{
        height: 52, flexShrink: 0,
        borderBottom: `1px solid ${T.border}`,
        background: T.bg, display: "flex", alignItems: "center",
        padding: "0 16px", gap: 8, overflow: "hidden",
      }}>
        <button
          onClick={p.onToggleSidebar}
          onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
          onMouseLeave={(e) => { e.currentTarget.style.background = T.surface; }}
          style={{
            width: 30, height: 30, borderRadius: 100,
            border: `0.5px solid ${T.border}`, background: T.surface,
            cursor: "pointer", display: "flex",
            alignItems: "center", justifyContent: "center", color: T.textSecondary,
          }}
        >
          <Icon name="panel" size={13} />
        </button>
        <span style={{
          fontSize: 14, fontWeight: 600, color: T.text,
          letterSpacing: "-0.4px", fontFamily: T.fontDisplay, marginRight: 4,
        }}>Timeline</span>

        <div style={{
          display: "flex", alignItems: "center", gap: 2,
          background: T.surface, borderRadius: 100, padding: 3,
          border: `0.5px solid ${T.border}`,
        }}>
          <button onClick={() => setStartOffset((o) => o - 7)} style={navBtnStyle()}>
            <Icon name="chevronLeft" size={13} />
          </button>
          <span style={{
            fontSize: 12, color: T.textSecondary, minWidth: 110,
            textAlign: "center", letterSpacing: "-0.12px",
          }}>
            {viewStart.toLocaleDateString("en-US", { month: "short", day: "numeric" })}
            {" – "}
            {viewEnd.toLocaleDateString("en-US", { month: "short", day: "numeric" })}
          </span>
          <button onClick={() => setStartOffset((o) => o + 7)} style={navBtnStyle()}>
            <Icon name="chevronRight" size={13} />
          </button>
        </div>
        <button
          onClick={() => setStartOffset(-14)}
          style={{
            padding: "5px 12px", borderRadius: 100,
            border: `0.5px solid ${T.border}`,
            background: "transparent", color: T.textSecondary,
            fontSize: 12, cursor: "pointer", fontFamily: T.fontBody, letterSpacing: "-0.12px",
          }}
        >Today</button>
        <div style={{ flex: 1 }} />
        <button
          onClick={p.onNewJob}
          style={{
            display: "flex", alignItems: "center", gap: 6,
            padding: "6px 14px", borderRadius: 100, border: "none",
            background: "#fff", color: "#0C0C0C",
            fontSize: 12, fontWeight: 500, cursor: "pointer",
            fontFamily: T.fontBody, letterSpacing: "-0.12px",
          }}
        >
          <Icon name="plus" size={13} color="#0C0C0C" strokeWidth={2.5} />
          Add Job
        </button>
      </div>

      {/* ── Body (scrollable) ──────────────────────────────────────── */}
      <div style={{ flex: 1, overflow: "auto", position: "relative" }}>
        <div style={{ minWidth: LEFT_COL + TOTAL_DAYS * DAY_W, position: "relative" }}>
          {/* Header rows (month + day numbers), sticky to the top. */}
          <div style={{ position: "sticky", top: 0, zIndex: 10, background: T.bg, borderBottom: `1px solid ${T.border}` }}>
            <div style={{ display: "flex", marginLeft: LEFT_COL, height: 22 }}>
              {monthGroups.map((mg, i) => (
                <div key={i} style={{
                  width: mg.count * DAY_W, flexShrink: 0,
                  padding: "3px 8px", fontSize: 11, fontWeight: 600,
                  color: T.textTertiary, letterSpacing: "0.02em",
                  textTransform: "uppercase",
                  borderRight: `1px solid ${T.border}`,
                  overflow: "hidden", whiteSpace: "nowrap",
                }}>{mg.label}</div>
              ))}
            </div>
            <div style={{ display: "flex", marginLeft: LEFT_COL, height: 28, borderBottom: `1px solid ${T.border}` }}>
              {days.map((d, i) => {
                const isToday = d.toDateString() === todayRef.toDateString();
                const isWk = d.getDay() === 0 || d.getDay() === 6;
                return (
                  <div key={i} style={{
                    width: DAY_W, flexShrink: 0,
                    display: "flex", flexDirection: "column",
                    alignItems: "center", justifyContent: "center",
                    fontSize: 10,
                    color: isToday ? "#fff" : isWk ? T.textTertiary : T.textSecondary,
                    background: isToday ? T.accent : "transparent",
                    fontWeight: isToday ? 700 : 400,
                    borderRight: `1px solid ${T.border}`,
                    letterSpacing: "-0.1px",
                  }}>
                    <span style={{ fontSize: 9, opacity: 0.6 }}>{"SMTWTFS"[d.getDay()]}</span>
                    <span>{d.getDate()}</span>
                  </div>
                );
              })}
            </div>
          </div>

          {/* Job rows. */}
          {activeJobs.map((job) => {
            const stageEntries = Object.entries(job.stageNotes || {})
              .map(([k, v]) => ({ idx: parseInt(k), note: v as StageNote }))
              .sort((a, b) => a.idx - b.idx);

            return (
              <div key={job.id} style={{
                display: "flex", height: ROW_HEIGHT,
                borderBottom: `1px solid ${T.border}`,
                alignItems: "center", position: "relative",
              }}>
                {/* Sticky left label */}
                <div
                  onClick={() => p.onSelectJob(job.id)}
                  style={{
                    width: LEFT_COL, flexShrink: 0,
                    padding: "0 16px", display: "flex",
                    alignItems: "center", gap: 10,
                    position: "sticky", left: 0, background: T.bg,
                    zIndex: 5, borderRight: `1px solid ${T.border}`,
                    height: "100%", cursor: "pointer",
                  }}
                >
                  <div style={{
                    width: 28, height: 28, borderRadius: 8,
                    background: `${job.avatarColor}18`,
                    border: `0.5px solid ${job.avatarColor}40`,
                    display: "flex", alignItems: "center", justifyContent: "center",
                    fontSize: 12, fontWeight: 700, color: job.avatarColor,
                    flexShrink: 0, fontFamily: T.fontDisplay,
                  }}>{job.avatar}</div>
                  <div style={{ minWidth: 0 }}>
                    <p style={{ fontSize: 12, fontWeight: 600, color: T.text, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", letterSpacing: "-0.3px" }}>{job.company}</p>
                    <p style={{ fontSize: 11, color: T.textSecondary, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", letterSpacing: "-0.11px" }}>{job.role.split(",")[0]}</p>
                  </div>
                </div>

                {/* Day-stripe background */}
                <div style={{ position: "absolute", left: LEFT_COL, right: 0, top: 0, bottom: 0, display: "flex", pointerEvents: "none" }}>
                  {days.map((d, i) => (
                    <div key={i} style={{
                      width: DAY_W, flexShrink: 0, height: "100%",
                      background: (d.getDay() === 0 || d.getDay() === 6)
                        ? "rgba(255,255,255,0.015)"
                        : "transparent",
                      borderRight: `1px solid ${T.border}`,
                    }} />
                  ))}
                </div>

                {/* Today line */}
                <div style={{
                  position: "absolute", left: todayX, top: 0, bottom: 0,
                  width: 2, background: T.accent, zIndex: 4, opacity: 0.7,
                  pointerEvents: "none",
                }} />

                {/* Stage bars + dots */}
                {stageEntries.map(({ idx, note }) => {
                  const d = parseStageDate(note.date);
                  if (!d) return null;
                  const x = dateToX(d);
                  if (x < LEFT_COL - 20 || x > LEFT_COL + TOTAL_DAYS * DAY_W + 20) return null;

                  const color = STAGE_COLORS[idx] ?? T.accent;
                  const isDone    = idx <  job.currentStage;
                  const isCurrent = idx === job.currentStage;
                  const next = stageEntries.find((e) => e.idx === idx + 1);
                  const nextDate = next ? parseStageDate(next.note.date) : (isCurrent ? todayRef : null);
                  const barW = nextDate ? Math.max(8, ((nextDate.getTime() - d.getTime()) / DAY_MS) * DAY_W) : 0;

                  return (
                    <div key={idx} style={{ position: "absolute", top: "50%", transform: "translateY(-50%)", zIndex: 6 }}>
                      {barW > 0 && (
                        <div style={{
                          position: "absolute", left: x, top: "50%",
                          transform: "translateY(-50%)",
                          height: 20, width: barW,
                          background: `${color}25`,
                          borderRadius: 4, border: `1px solid ${color}40`,
                          display: "flex", alignItems: "center", paddingLeft: 22, overflow: "hidden",
                        }}>
                          <span style={{ fontSize: 10, color, opacity: 0.8, whiteSpace: "nowrap", letterSpacing: "-0.1px" }}>
                            {STAGES_GANTT[idx]}
                          </span>
                        </div>
                      )}
                      <button
                        onClick={(e) => {
                          const r = e.currentTarget.getBoundingClientRect();
                          setEditing({ jobId: job.id, stageIdx: idx, anchor: { x: r.left, y: r.bottom + 8 } });
                          e.stopPropagation();
                        }}
                        style={{
                          position: "absolute", left: x - 7, top: "50%",
                          transform: "translateY(-50%)",
                          width: 14, height: 14, borderRadius: "50%",
                          background: isDone || isCurrent ? color : T.surface2,
                          border: `2px solid ${color}`, cursor: "pointer", zIndex: 7,
                          boxShadow: isCurrent ? `0 0 0 3px ${color}30` : "none",
                          padding: 0,
                        }}
                      />
                    </div>
                  );
                })}

                {/* "+ next stage" affordance */}
                {(() => {
                  const lastIdx = stageEntries.length ? stageEntries[stageEntries.length - 1].idx : -1;
                  const nextIdx = Math.min(lastIdx + 1, STAGES_GANTT.length - 1);
                  if (nextIdx > lastIdx || lastIdx === -1) {
                    const x = todayX + 10;
                    return (
                      <button
                        onClick={(e) => {
                          const r = e.currentTarget.getBoundingClientRect();
                          setEditing({ jobId: job.id, stageIdx: nextIdx, anchor: { x: r.left, y: r.bottom + 8 } });
                          e.stopPropagation();
                        }}
                        style={{
                          position: "absolute", left: x, top: "50%",
                          transform: "translateY(-50%)",
                          width: 20, height: 20, borderRadius: "50%",
                          background: T.surface2,
                          border: `1px dashed ${T.textTertiary}`,
                          cursor: "pointer", zIndex: 7,
                          display: "flex", alignItems: "center", justifyContent: "center",
                          color: T.textTertiary, padding: 0,
                        }}
                      >
                        <Icon name="plus" size={10} />
                      </button>
                    );
                  }
                  return null;
                })()}
              </div>
            );
          })}

          {/* Add job row */}
          <div style={{ display: "flex", height: 44, alignItems: "center", borderBottom: `1px solid ${T.border}` }}>
            <div style={{
              width: LEFT_COL, flexShrink: 0, padding: "0 16px",
              position: "sticky", left: 0, background: T.bg, zIndex: 5,
              height: "100%", display: "flex", alignItems: "center",
              borderRight: `1px solid ${T.border}`,
            }}>
              <button
                onClick={p.onNewJob}
                onMouseEnter={(e) => { e.currentTarget.style.color = T.text; }}
                onMouseLeave={(e) => { e.currentTarget.style.color = T.textTertiary; }}
                style={{
                  display: "flex", alignItems: "center", gap: 6,
                  color: T.textTertiary, background: "none", border: "none",
                  cursor: "pointer", fontSize: 12, fontFamily: T.fontBody, letterSpacing: "-0.12px",
                }}
              >
                <Icon name="plus" size={12} /> Add job
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Stage editor popover */}
      {editing && (
        <StageEditor
          job={p.jobs.find((j) => j.id === editing.jobId)!}
          stageIdx={editing.stageIdx}
          anchor={editing.anchor}
          onSave={(idx, data) => handleSaveStage(editing.jobId, idx, data)}
          onClose={() => setEditing(null)}
        />
      )}
    </div>
  );
};

const navBtnStyle = (): CSSProperties => ({
  width: 26, height: 26, borderRadius: 100,
  border: "none", background: "none", cursor: "pointer",
  display: "flex", alignItems: "center", justifyContent: "center",
  color: T.textSecondary, padding: 0,
});

// ─── Stage editor popover ─────────────────────────────────────────────────

interface StageEditorProps {
  job:      Job;
  stageIdx: number;
  anchor:   { x: number; y: number };
  onSave:   (idx: number, data: StageNote) => void;
  onClose:  () => void;
}

const StageEditor = ({ job, stageIdx, anchor, onSave, onClose }: StageEditorProps) => {
  const existing = job.stageNotes[stageIdx];
  const [date,    setDate]    = useState(existing?.date    ?? "");
  const [outcome, setOutcome] = useState(existing?.outcome ?? "");
  const [notes,   setNotes]   = useState(existing?.notes   ?? "");
  const stage = STAGES_GANTT[stageIdx];

  const inp: CSSProperties = {
    width: "100%", padding: "7px 10px", borderRadius: 8,
    border: `0.5px solid ${T.border}`, background: T.bg,
    color: T.text, fontSize: 12, fontFamily: T.fontBody,
    outline: "none", letterSpacing: "-0.12px",
  };

  return (
    <div onClick={onClose} style={{ position: "fixed", inset: 0, zIndex: 200 }}>
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          position: "fixed",
          top:  anchor.y,
          left: Math.min(anchor.x, window.innerWidth - 300),
          background: T.surface, border: `0.5px solid ${T.border}`,
          borderRadius: 14, padding: 16, width: 280,
          boxShadow: T.shadowLg, zIndex: 201,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 14 }}>
          <p style={{ fontSize: 13, fontWeight: 600, color: T.text, letterSpacing: "-0.3px", fontFamily: T.fontDisplay }}>{stage}</p>
          <button onClick={onClose} style={{
            background: T.surface2, border: "none", borderRadius: 100,
            width: 22, height: 22, display: "flex",
            alignItems: "center", justifyContent: "center",
            cursor: "pointer", color: T.textSecondary,
          }}>
            <Icon name="x" size={11} />
          </button>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <div>
            <label style={{ fontSize: 11, color: T.textSecondary, display: "block", marginBottom: 4, letterSpacing: "-0.11px" }}>Date</label>
            <input style={inp} value={date} onChange={(e) => setDate(e.target.value)} placeholder="e.g. Apr 18" />
          </div>
          <div>
            <label style={{ fontSize: 11, color: T.textSecondary, display: "block", marginBottom: 4, letterSpacing: "-0.11px" }}>Outcome</label>
            <select style={{ ...inp, cursor: "pointer" }} value={outcome} onChange={(e) => setOutcome(e.target.value)}>
              <option value="">Select outcome</option>
              <option value="Submitted">Submitted</option>
              <option value="Passed">Passed</option>
              <option value="In progress">In progress</option>
              <option value="Scheduled">Scheduled</option>
              <option value="Rejected">Rejected</option>
              <option value="Offer received">Offer received</option>
            </select>
          </div>
          <div>
            <label style={{ fontSize: 11, color: T.textSecondary, display: "block", marginBottom: 4, letterSpacing: "-0.11px" }}>Notes</label>
            <textarea
              style={{ ...inp, minHeight: 60, resize: "vertical" }}
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              placeholder="Add notes..."
            />
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button onClick={onClose} style={{
            flex: 1, padding: "7px 0", borderRadius: 100, border: "none",
            background: T.surface2, color: T.textSecondary,
            fontSize: 12, cursor: "pointer", fontFamily: T.fontBody,
          }}>Cancel</button>
          <button
            onClick={() => onSave(stageIdx, { date, outcome, notes })}
            style={{
              flex: 2, padding: "7px 0", borderRadius: 100, border: "none",
              background: "#fff", color: "#0C0C0C",
              fontSize: 12, fontWeight: 500, cursor: "pointer", fontFamily: T.fontBody,
            }}
          >Save</button>
        </div>
      </div>
    </div>
  );
};

// ─── Thinking disclosure (above AI bubbles when agent emitted stage logs) ─

interface ThinkingProps {
  logs: string[];
  streaming: boolean;
}

const Thinking = ({ logs, streaming }: ThinkingProps) => {
  // Default-open while streaming; users can toggle and the state sticks.
  const [open, setOpen] = useState(streaming);
  const count = logs.length;
  const plural = count === 1 ? "" : "s";
  const title = streaming
    ? `Thinking… · ${count} step${plural}`
    : `Thought for ${count} step${plural}`;

  // Strip backend padding (empty lines, "---" dividers, lone dots) and
  // render inline **bold**. Keeps long agent logs compact.
  const cleanLine = (raw: string): string => raw
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l && !/^[-._*]+$/.test(l))
    .join(" ");

  return (
    <div style={{
      background: T.bg, borderRadius: 10,
      border: `0.5px solid ${T.border}`,
      padding: "8px 12px", marginBottom: 6,
    }}>
      <button
        onClick={() => setOpen((v) => !v)}
        style={{
          display: "flex", alignItems: "center", gap: 6,
          background: "transparent", border: "none", cursor: "pointer",
          padding: 0, width: "100%", textAlign: "left",
          color: T.textSecondary, fontFamily: T.fontBody,
          fontSize: 12, fontWeight: 500, letterSpacing: "-0.12px",
        }}
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} size={11} color={T.textTertiary} />
        <span>{title}</span>
        {!open && count > 0 && (
          <span style={{
            color: T.textTertiary, fontWeight: 400,
            whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
            flex: 1, minWidth: 0,
          }}>
            {" — "}
            {cleanLine(logs[count - 1] ?? "").slice(0, 72)}
          </span>
        )}
      </button>
      {open && (
        <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 4 }}>
          {logs.map((raw, i) => {
            const text = cleanLine(raw);
            if (!text) return null;
            return (
              <div key={i} style={{ display: "flex", gap: 6, alignItems: "flex-start" }}>
                <span style={{ color: T.textTertiary, fontSize: 11.5, lineHeight: 1.6 }}>·</span>
                <span style={{
                  color: T.textSecondary, fontSize: 11.5, lineHeight: 1.6,
                  letterSpacing: "-0.115px",
                }}>
                  {text.split(/(\*\*[^*]+\*\*)/g).map((part, j) =>
                    part.startsWith("**")
                      ? <strong key={j} style={{ color: T.text }}>{part.slice(2, -2)}</strong>
                      : <span key={j}>{part}</span>
                  )}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
};

// ─── Chat area ────────────────────────────────────────────────────────────

// ─── ATS scorecard card (Application-Prep extra) ──────────────────────────

const ScorecardCard = ({ card }: { card: Scorecard }) => {
  const recommendation = card.hire_recommendation ?? "—";
  const recColor =
    recommendation === "Hire"    ? "#10B981" :
    recommendation === "No Hire" ? "#EF4444" :
                                   T.textSecondary;
  const cell = (label: string, value: string, color: string = T.text) => (
    <div style={{ display: "flex", flexDirection: "column", gap: 1, marginRight: 24 }}>
      <span style={{ fontSize: 10.5, color: T.textTertiary }}>{label}</span>
      <span style={{ fontSize: 13, fontWeight: 600, color }}>{value}</span>
    </div>
  );
  const MAX_SKILL_CHARS = 28;
  const skills = (card.skills_matched ?? []).map((s) =>
    s.length > MAX_SKILL_CHARS ? s.slice(0, MAX_SKILL_CHARS - 1) + "…" : s,
  );
  return (
    <div style={{
      marginTop: 10, padding: "12px 14px",
      borderRadius: 12, border: `1px solid ${T.border}`,
      background: T.surface,
    }}>
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 8 }}>
        <Icon name="analyze" size={14} color={T.accent} />
        <span style={{ fontSize: 12.5, fontWeight: 600, color: T.text }}>ATS Scorecard</span>
      </div>
      <div style={{ display: "flex", flexWrap: "wrap", rowGap: 8 }}>
        {cell("Verbatim Match",      card.verbatim_match_score != null ? `${card.verbatim_match_score}%` : "—")}
        {cell("Role Title Aligned",  card.role_title_alignment ?? "—")}
        {cell("Quantification",      card.quantification_check ?? "—")}
        {cell("Recommendation",      recommendation, recColor)}
      </div>
      {skills.length > 0 && (
        <>
          <div style={{ fontSize: 10.5, color: T.textTertiary, marginTop: 10, marginBottom: 4 }}>
            Skills matched
          </div>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
            {skills.map((s, i) => (
              <span key={i} style={{
                padding: "3px 8px", borderRadius: 100,
                background: T.surface2, color: T.textSecondary,
                fontSize: 11, whiteSpace: "nowrap",
              }}>
                {s}
              </span>
            ))}
          </div>
        </>
      )}
    </div>
  );
};

interface ChatAreaProps {
  chat: ChatThread | null;
  job: Job | null;
  onSendMessage: (text: string) => void;
  isLoading: boolean;
  /// ID of the chat thread currently streaming. Used to scope the loading
  /// indicator so it only appears in the active thread, not every chat.
  streamingChatId: string | null;
  /// Launches the saved .docx in the OS default app (Word, etc.).
  onOpenResumeDocx?: (path: string) => void;
  /// Kicks off the recruiter-knockout simulation thread for the current job.
  onSimulateKnockout?: () => void;
}

const ChatArea = ({ chat, job, onSendMessage, isLoading, streamingChatId, onOpenResumeDocx, onSimulateKnockout }: ChatAreaProps) => {
  // Loader is per-thread, not global. Even if isLoading is true (some other
  // stream is running), don't draw the bouncing dots unless the user is
  // looking at the thread the tokens are flowing into.
  const showLoader = isLoading && chat != null && chat.id === streamingChatId;
  const endRef = useRef<HTMLDivElement>(null);
  const [hoveredMsg, setHoveredMsg] = useState<number | null>(null);

  useEffect(() => {
    const parent = endRef.current?.parentElement;
    if (parent) parent.scrollTop = endRef.current!.offsetTop;
  }, [chat?.messages]);

  if (!chat) {
    return (
      <div style={{
        flex: 1, display: "flex", flexDirection: "column",
        alignItems: "center", justifyContent: "center",
        gap: 20, padding: 32,
      }}>
        <div style={{
          width: 160, height: 110, borderRadius: 30,
          background: T.gradientMagenta,
          display: "flex", alignItems: "center", justifyContent: "center",
          boxShadow: "0 16px 48px rgba(190,24,93,0.35)",
        }}>
          <Icon name="sparkle" size={32} color="rgba(255,255,255,0.9)" strokeWidth={1.5} />
        </div>
        <div style={{ textAlign: "center" }}>
          <p style={{ fontSize: 20, fontWeight: 700, color: T.text, marginBottom: 6, letterSpacing: "-0.6px", fontFamily: T.fontDisplay, lineHeight: 1.1 }}>
            Start a new chat
          </p>
          <p style={{ fontSize: 13, color: T.textSecondary, maxWidth: 280, lineHeight: 1.5, letterSpacing: "-0.13px" }}>
            Ask anything about your application, practice questions, or get interview tips.
          </p>
        </div>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, justifyContent: "center", maxWidth: 440 }}>
          {[
            "Help me prep for system design",
            "Analyze the job description",
            "Generate behavioral questions",
            "Review my cover letter",
          ].map((prompt) => (
            <button
              key={prompt}
              onClick={() => onSendMessage(prompt)}
              onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; e.currentTarget.style.color = T.text; }}
              onMouseLeave={(e) => { e.currentTarget.style.background = T.surface; e.currentTarget.style.color = T.textSecondary; }}
              style={{
                padding: "7px 14px", borderRadius: 100,
                border: `0.5px solid ${T.border}`,
                background: T.surface, color: T.textSecondary,
                fontSize: 12, cursor: "pointer", fontFamily: T.fontBody,
                fontWeight: 400, letterSpacing: "-0.12px",
              }}
            >
              {prompt}
            </button>
          ))}
        </div>
      </div>
    );
  }

  return (
    <div style={{ flex: 1, overflowY: "auto", padding: "24px 0" }}>
      {chat.messages.map((msg, i) => (
        <div
          key={i}
          onMouseEnter={() => setHoveredMsg(i)}
          onMouseLeave={() => setHoveredMsg(null)}
          style={{
            padding: "0 24px", marginBottom: 24, display: "flex",
            flexDirection: "column", alignItems: msg.role === "user" ? "flex-end" : "flex-start",
          }}
        >
          {msg.role === "user" ? (
            <div style={{
              maxWidth: "68%", padding: "10px 16px",
              borderRadius: "20px 20px 6px 20px",
              background: "#fff", color: "#0C0C0C",
              fontSize: 13, lineHeight: 1.6, fontWeight: 500, letterSpacing: "-0.13px",
            }}>
              {msg.content}
            </div>
          ) : (
            <div style={{ maxWidth: "80%", display: "flex", gap: 10, alignItems: "flex-start" }}>
              <div style={{
                width: 26, height: 26, borderRadius: 100,
                background: T.surface, border: `0.5px solid ${T.border}`,
                display: "flex", alignItems: "center", justifyContent: "center",
                flexShrink: 0, marginTop: 2,
              }}>
                <Icon name="sparkle" size={12} color={T.textSecondary} />
              </div>
              <div style={{ flex: 1, minWidth: 0 }}>
                {msg.logs && msg.logs.length > 0 && (
                  <Thinking logs={msg.logs} streaming={!!msg.streaming} />
                )}
                <div style={{
                  padding: "12px 16px",
                  borderRadius: "6px 20px 20px 20px",
                  background: T.surface, boxShadow: T.shadowMd,
                }}>
                  {msg.streaming && !msg.content
                    ? <span style={{ color: T.textTertiary, fontSize: 13, display: "inline-flex", alignItems: "center", gap: 6 }}>
                        <Icon name="sparkle" size={11} color={T.textTertiary} />
                        Working — first tokens incoming…
                      </span>
                    : <MarkdownText content={msg.streaming ? msg.content + "▊" : msg.content} />
                  }
                </div>
                {/* Application-Prep extras: scorecard + Open Resume + KO buttons.
                    Only on the last finished AI message of the Prep thread, only
                    when the Job has the corresponding data. */}
                {chat?.title === "Application Prep"
                  && !msg.streaming
                  && i === chat.messages.length - 1
                  && job && (
                  <>
                    {job.scorecard && <ScorecardCard card={job.scorecard} />}
                    {(job.resumeDocxPath || job.tailoredResume) && (
                      <div style={{ display: "flex", gap: 8, marginTop: 8, flexWrap: "wrap" }}>
                        {job.resumeDocxPath && (
                          <button
                            onClick={() => onOpenResumeDocx?.(job.resumeDocxPath!)}
                            style={{
                              display: "flex", alignItems: "center", gap: 6,
                              padding: "8px 14px", borderRadius: 100,
                              border: `1px solid ${T.accentSoft}`,
                              background: T.surface2, color: T.text,
                              fontSize: 12.5, cursor: "pointer", fontFamily: T.fontBody,
                            }}
                          >
                            <Icon name="note" size={13} /> Open Tailored Resume
                          </button>
                        )}
                        {job.tailoredResume && (
                          <button
                            onClick={() => onSimulateKnockout?.()}
                            style={{
                              display: "flex", alignItems: "center", gap: 6,
                              padding: "8px 14px", borderRadius: 100,
                              border: `1px solid ${T.accentSoft}`,
                              background: T.surface2, color: T.text,
                              fontSize: 12.5, cursor: "pointer", fontFamily: T.fontBody,
                            }}
                          >
                            <Icon name="interview" size={13} /> Simulate Knockout Screen
                          </button>
                        )}
                      </div>
                    )}
                  </>
                )}
                {hoveredMsg === i && (
                  <div style={{ display: "flex", gap: 4, marginTop: 6 }}>
                    {([
                      { icon: "copy"     as IconName, label: "Copy" },
                      { icon: "refresh"  as IconName, label: "Regenerate" },
                      { icon: "bookmark" as IconName, label: "Save" },
                    ]).map((a) => (
                      <button
                        key={a.icon}
                        style={{
                          display: "flex", alignItems: "center", gap: 5,
                          padding: "4px 8px", borderRadius: 100, border: "none",
                          background: T.surface2, color: T.textSecondary,
                          fontSize: 11, cursor: "pointer", fontFamily: T.fontBody,
                        }}
                      >
                        <Icon name={a.icon} size={11} />{a.label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      ))}
      {showLoader && (
        <div style={{ padding: "0 24px", marginBottom: 24, display: "flex", gap: 12, alignItems: "flex-start" }}>
          <div style={{
            width: 28, height: 28, borderRadius: 8,
            background: T.accentSoft,
            display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
          }}>
            <Icon name="sparkle" size={13} color={T.accent} />
          </div>
          <div style={{ padding: "14px 18px", borderRadius: "4px 14px 14px 14px", background: T.surface, border: `1px solid ${T.border}` }}>
            <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
              {[0, 1, 2].map((j) => (
                <div key={j} style={{
                  width: 7, height: 7, borderRadius: "50%",
                  background: T.accent,
                  animation: `bounce 1.2s ease-in-out ${j * 0.2}s infinite`,
                }} />
              ))}
            </div>
          </div>
        </div>
      )}
      <div ref={endRef} />
      <style>{`@keyframes bounce { 0%,60%,100%{transform:translateY(0)} 30%{transform:translateY(-8px)} }`}</style>
    </div>
  );
};

// ─── Input composer ───────────────────────────────────────────────────────

interface InputComposerProps {
  onSend: (text: string) => void;
  disabled: boolean;
}

const InputComposer = ({ onSend, disabled }: InputComposerProps) => {
  const [value, setValue] = useState("");
  const taRef = useRef<HTMLTextAreaElement>(null);

  const send = () => {
    const t = value.trim();
    if (!t || disabled) return;
    onSend(t);
    setValue("");
  };

  useEffect(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 160) + "px";
  }, [value]);

  const hasText = !!value.trim();

  return (
    <div style={{ padding: "10px 16px 14px", borderTop: `1px solid ${T.border}`, background: T.bg }}>
      <div style={{
        background: T.surface, border: `0.5px solid ${T.border}`,
        borderRadius: 20, display: "flex", flexDirection: "column",
        overflow: "hidden", boxShadow: T.shadowMd,
      }}>
        <textarea
          ref={taRef}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }}
          placeholder="Ask anything about this role, practice questions, review your resume…"
          style={{
            width: "100%", minHeight: 52, resize: "none",
            border: "none", outline: "none",
            padding: "14px 16px 0",
            fontSize: 13, fontFamily: T.fontBody,
            color: T.text, background: "transparent",
            lineHeight: 1.6, letterSpacing: "-0.13px",
            fontFeatureSettings: '"cv01","cv05","cv09","cv11","ss03","ss07"',
          }}
        />
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "6px 10px 8px" }}>
          <div style={{ display: "flex", gap: 2 }}>
            {(["attach", "mic"] as IconName[]).map((icon) => (
              <button
                key={icon}
                onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
                onMouseLeave={(e) => { e.currentTarget.style.background = "none"; }}
                style={{
                  width: 30, height: 30, borderRadius: 100,
                  border: "none", background: "none",
                  color: T.textTertiary, cursor: "pointer",
                  display: "flex", alignItems: "center", justifyContent: "center",
                }}
              >
                <Icon name={icon} size={14} />
              </button>
            ))}
          </div>
          <button
            onClick={send}
            disabled={!hasText || disabled}
            style={{
              width: 32, height: 32, borderRadius: 100, border: "none",
              background: hasText && !disabled ? "#fff" : T.surface2,
              cursor: hasText && !disabled ? "pointer" : "default",
              display: "flex", alignItems: "center", justifyContent: "center",
            }}
          >
            <Icon name="send" size={13} color={hasText && !disabled ? "#0C0C0C" : T.textTertiary} />
          </button>
        </div>
      </div>
      <p style={{
        fontSize: 11, color: T.textTertiary,
        textAlign: "center", marginTop: 6, letterSpacing: "-0.11px",
      }}>
        InterPrep AI may make mistakes. Always verify important information.
      </p>
    </div>
  );
};

// ─── Empty state (no jobs at all) ─────────────────────────────────────────

interface EmptyStateProps { onNewJob: () => void; }

const EmptyState = ({ onNewJob }: EmptyStateProps) => (
  <div style={{
    flex: 1, display: "flex", flexDirection: "column",
    alignItems: "center", justifyContent: "center",
    gap: 24, padding: 40,
  }}>
    <div style={{
      width: 200, height: 140, borderRadius: 30,
      background: T.gradientViolet,
      display: "flex", alignItems: "center", justifyContent: "center",
      boxShadow: "0 20px 60px rgba(124,58,237,0.4)",
    }}>
      <Icon name="briefcase" size={40} color="rgba(255,255,255,0.9)" strokeWidth={1.5} />
    </div>
    <div style={{ textAlign: "center", maxWidth: 340 }}>
      <h2 style={{ fontSize: 28, fontWeight: 700, color: T.text, marginBottom: 10, letterSpacing: "-1px", fontFamily: T.fontDisplay, lineHeight: 1.1 }}>
        Your command center
      </h2>
      <p style={{ fontSize: 15, color: T.textSecondary, lineHeight: 1.5, letterSpacing: "-0.15px" }}>
        Track applications, prep with AI, research companies — all from one dark canvas.
      </p>
    </div>
    <button onClick={onNewJob} style={{
      padding: "10px 20px", borderRadius: 100, border: "none",
      background: "#fff", color: "#0C0C0C",
      fontSize: 14, fontWeight: 500, cursor: "pointer",
      fontFamily: T.fontBody, letterSpacing: "-0.14px",
    }}>
      Add your first job
    </button>
  </div>
);

// ─── New-job modal ────────────────────────────────────────────────────────

interface NewJobFormState {
  company: string;
  role: string;
  url: string;
  location: string;
  status: JobStatusKey;
  jobDescription: string;
  notes: string;
}

interface NewJobModalProps {
  onClose: () => void;
  onSubmit: (form: NewJobFormState) => void;
}

const NewJobModal = ({ onClose, onSubmit }: NewJobModalProps) => {
  const [form, setForm] = useState<NewJobFormState>({
    company: "", role: "", url: "", location: "", status: "Applied",
    jobDescription: "", notes: "",
  });
  const [errors, setErrors] = useState<Partial<Record<keyof NewJobFormState, string>>>({});

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const errs: typeof errors = {};
    if (!form.company.trim()) errs.company = "Required";
    if (!form.role.trim())    errs.role    = "Required";
    if (Object.keys(errs).length) { setErrors(errs); return; }
    onSubmit(form);
  };

  const fieldStyle = (err?: string): CSSProperties => ({
    width: "100%", padding: "10px 14px", borderRadius: 10,
    border: `0.5px solid ${err ? "#EF4444" : T.border}`,
    background: T.bg, color: T.text, fontSize: 13,
    fontFamily: T.fontBody, outline: "none", letterSpacing: "-0.13px",
    fontFeatureSettings: '"cv01","cv05","cv09","cv11"',
  });

  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed", inset: 0,
        background: "rgba(0,0,0,0.7)", backdropFilter: "blur(8px)",
        display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: T.surface, borderRadius: 20, padding: 28,
          width: 460, maxWidth: "90vw",
          boxShadow: T.shadowLg, border: `0.5px solid ${T.border}`,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 22 }}>
          <div>
            <h2 style={{ fontSize: 18, fontWeight: 700, color: T.text, letterSpacing: "-0.5px", fontFamily: T.fontDisplay }}>Add New Job</h2>
            <p style={{ fontSize: 12, color: T.textSecondary, marginTop: 3, letterSpacing: "-0.12px" }}>Track a new opportunity</p>
          </div>
          <button onClick={onClose} style={{
            background: T.surface2, border: "none", cursor: "pointer",
            padding: 6, borderRadius: 100, color: T.textSecondary,
            display: "flex", alignItems: "center", width: 28, height: 28, justifyContent: "center",
          }}>
            <Icon name="x" size={14} />
          </button>
        </div>
        <form onSubmit={submit}>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0 14px" }}>
            <Field label="Company Name" id="company" required error={errors.company}>
              <input
                style={fieldStyle(errors.company)} value={form.company}
                onChange={(e) => setForm((f) => ({ ...f, company: e.target.value }))}
                placeholder="e.g. Google"
              />
            </Field>
            <Field label="Role Title" id="role" required error={errors.role}>
              <input
                style={fieldStyle(errors.role)} value={form.role}
                onChange={(e) => setForm((f) => ({ ...f, role: e.target.value }))}
                placeholder="e.g. Software Engineer"
              />
            </Field>
          </div>
          <Field label="Job URL" id="url">
            <input
              style={fieldStyle()} value={form.url}
              onChange={(e) => setForm((f) => ({ ...f, url: e.target.value }))}
              placeholder="https://..."
            />
          </Field>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0 14px" }}>
            <Field label="Location" id="location">
              <input
                style={fieldStyle()} value={form.location}
                onChange={(e) => setForm((f) => ({ ...f, location: e.target.value }))}
                placeholder="City, State"
              />
            </Field>
            <Field label="Status" id="status">
              <select
                style={{ ...fieldStyle(), cursor: "pointer" }}
                value={form.status}
                onChange={(e) => setForm((f) => ({ ...f, status: e.target.value as JobStatusKey }))}
              >
                {(Object.keys(STATUS_CONFIG) as JobStatusKey[]).map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
            </Field>
          </div>
          <Field label="Job Description" id="jobDescription">
            <textarea
              style={{ ...fieldStyle(), minHeight: 120, resize: "vertical" }}
              value={form.jobDescription}
              onChange={(e) => setForm((f) => ({ ...f, jobDescription: e.target.value }))}
              placeholder="Paste the JD here so InterPrep can research the company, process, and likely questions."
            />
          </Field>
          <Field label="Notes" id="notes">
            <textarea
              style={{ ...fieldStyle(), minHeight: 72, resize: "vertical" }}
              value={form.notes}
              onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
              placeholder="Any initial notes..."
            />
          </Field>
          <div style={{ display: "flex", gap: 10, marginTop: 4 }}>
            <button type="button" onClick={onClose} style={{
              flex: 1, padding: "10px 0", borderRadius: 100, border: "none",
              background: T.surface2, color: T.textSecondary,
              fontSize: 13, fontWeight: 500, cursor: "pointer",
              fontFamily: T.fontBody, letterSpacing: "-0.13px",
            }}>Cancel</button>
            <button type="submit" style={{
              flex: 2, padding: "10px 0", borderRadius: 100, border: "none",
              background: "#fff", color: "#0C0C0C",
              fontSize: 13, fontWeight: 500, cursor: "pointer",
              fontFamily: T.fontBody, letterSpacing: "-0.13px",
            }}>Create Job</button>
          </div>
        </form>
      </div>
    </div>
  );
};

interface FieldProps {
  label: string;
  id: string;
  required?: boolean;
  error?: string;
  children: ReactNode;
}

const Field = ({ label, required, error, children }: FieldProps) => (
  <div style={{ marginBottom: 14 }}>
    <label style={{ display: "block", fontSize: 12, fontWeight: 500, color: T.textSecondary, marginBottom: 6, letterSpacing: "-0.12px" }}>
      {label}{required && <span style={{ color: "#EF4444", marginLeft: 2 }}>*</span>}
    </label>
    {children}
    {error && <p style={{ fontSize: 11, color: "#EF4444", marginTop: 4 }}>{error}</p>}
  </div>
);

// ─── Settings modal ───────────────────────────────────────────────────────

interface SettingsModalProps {
  credentials: Credentials;
  onCredentialsChange: (c: Credentials) => void;
  resumes: Resume[];
  onResumesChange: (r: Resume[]) => void;
  onClose: () => void;
}

const SettingsModal = ({
  credentials, onCredentialsChange,
  resumes, onResumesChange,
  onClose,
}: SettingsModalProps) => {
  const [section, setSection] = useState("resume");
  const sections = [
    { id: "account",       label: "Account",        icon: "user"      as IconName },
    { id: "appearance",    label: "Appearance",     icon: "sun"       as IconName },
    { id: "resume",        label: "Resume",         icon: "upload"    as IconName },
    { id: "notifications", label: "Notifications",  icon: "bell"      as IconName },
    { id: "data",          label: "Data & Privacy", icon: "briefcase" as IconName },
    { id: "apiKeys",       label: "API Keys",       icon: "key"       as IconName },
  ];

  const update = (patch: Partial<Credentials>) =>
    onCredentialsChange({ ...credentials, ...patch });

  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed", inset: 0,
        background: "rgba(0,0,0,0.7)", backdropFilter: "blur(8px)",
        display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: T.surface, borderRadius: 20,
          width: 660, height: 520,
          display: "flex", boxShadow: T.shadowLg,
          border: `0.5px solid ${T.border}`, overflow: "hidden",
        }}
      >
        <div style={{
          width: 190, background: T.bg,
          borderRight: `0.5px solid ${T.border}`,
          padding: 16, display: "flex", flexDirection: "column",
        }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 16 }}>
            <h2 style={{ fontSize: 13, fontWeight: 600, color: T.text, letterSpacing: "-0.3px", fontFamily: T.fontDisplay }}>Settings</h2>
            <button onClick={onClose} style={{
              background: T.surface, border: "none", cursor: "pointer",
              color: T.textSecondary, padding: 4, borderRadius: 100,
              display: "flex", width: 24, height: 24, alignItems: "center", justifyContent: "center",
            }}>
              <Icon name="x" size={13} />
            </button>
          </div>
          <nav style={{ display: "flex", flexDirection: "column", gap: 1 }}>
            {sections.map((s) => (
              <button key={s.id} onClick={() => setSection(s.id)} style={{
                display: "flex", alignItems: "center", gap: 8,
                padding: "7px 10px", borderRadius: 100, border: "none",
                background: section === s.id ? T.surface : "none",
                color: section === s.id ? T.text : T.textSecondary,
                fontSize: 12, fontWeight: section === s.id ? 500 : 400,
                cursor: "pointer", textAlign: "left",
                fontFamily: T.fontBody, letterSpacing: "-0.12px",
              }}>
                <Icon name={s.icon} size={13} />
                {s.label}
              </button>
            ))}
          </nav>
        </div>
        <div style={{ flex: 1, padding: 24, overflowY: "auto" }}>
          <h3 style={{ fontSize: 15, fontWeight: 700, color: T.text, marginBottom: 18, letterSpacing: "-0.4px", fontFamily: T.fontDisplay }}>
            {sections.find((s) => s.id === section)?.label}
          </h3>

          {section === "apiKeys" ? (
            <CredentialsTab credentials={credentials} update={update} />
          ) : section === "resume" ? (
            <ResumeTab resumes={resumes} onChange={onResumesChange} />
          ) : (
            <p style={{ fontSize: 12, color: T.textTertiary, letterSpacing: "-0.12px" }}>
              Settings for this section coming soon.
            </p>
          )}
        </div>
      </div>
    </div>
  );
};

// ─── API Keys tab body ────────────────────────────────────────────────────

interface CredentialsTabProps {
  credentials: Credentials;
  update: (patch: Partial<Credentials>) => void;
}

const CredentialsTab = ({ credentials, update }: CredentialsTabProps) => (
  <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
    <Card>
      <CardTitle>Gemini API Key</CardTitle>
      <CardDesc>
        Powers AI chat and JD analysis. Get yours from Google AI Studio.
      </CardDesc>
      <SecretField
        value={credentials.geminiApiKey}
        placeholder="AIza..."
        onChange={(v) => update({ geminiApiKey: v })}
      />
      <CardHint>Stored in Windows Credential Manager — encrypted with your user account.</CardHint>
    </Card>

    <Card>
      <CardTitle>Glassdoor Account</CardTitle>
      <CardDesc>
        Optional. Unlocks salary data and reviews behind the login wall.
      </CardDesc>
      <FieldLabel>Email</FieldLabel>
      <PlainField
        value={credentials.glassdoorEmail}
        placeholder="you@example.com"
        onChange={(v) => update({ glassdoorEmail: v })}
      />
      <div style={{ height: 8 }} />
      <FieldLabel>Password</FieldLabel>
      <SecretField
        value={credentials.glassdoorPassword}
        placeholder="Password"
        onChange={(v) => update({ glassdoorPassword: v })}
      />
    </Card>

    <Card>
      <CardTitle>Indeed Account</CardTitle>
      <CardDesc>
        Optional. Unlocks detailed company reviews and salary data.
      </CardDesc>
      <FieldLabel>Email</FieldLabel>
      <PlainField
        value={credentials.indeedEmail}
        placeholder="you@example.com"
        onChange={(v) => update({ indeedEmail: v })}
      />
      <div style={{ height: 8 }} />
      <FieldLabel>Password</FieldLabel>
      <SecretField
        value={credentials.indeedPassword}
        placeholder="Password"
        onChange={(v) => update({ indeedPassword: v })}
      />
      <CardHint>Credentials used only to automate your own account. Encrypted at rest.</CardHint>
    </Card>
  </div>
);

const Card = ({ children }: { children: ReactNode }) => (
  <div style={{
    background: T.bg, borderRadius: 10,
    border: `0.5px solid ${T.border}`, padding: 16,
  }}>{children}</div>
);
const CardTitle = ({ children }: { children: ReactNode }) => (
  <div style={{ fontSize: 13, fontWeight: 600, color: T.text, letterSpacing: "-0.3px", marginBottom: 4 }}>{children}</div>
);
const CardDesc = ({ children }: { children: ReactNode }) => (
  <p style={{ fontSize: 12, color: T.textSecondary, marginBottom: 12, letterSpacing: "-0.12px", lineHeight: 1.5 }}>{children}</p>
);
const CardHint = ({ children }: { children: ReactNode }) => (
  <p style={{ fontSize: 11, color: T.textTertiary, marginTop: 8, letterSpacing: "-0.11px" }}>{children}</p>
);

// ─── Resume tab ──────────────────────────────────────────────────────────

interface ResumeTabProps {
  resumes: Resume[];
  onChange: (r: Resume[]) => void;
}

const ResumeTab = ({ resumes, onChange }: ResumeTabProps) => {
  const [err,     setErr]     = useState<string | null>(null);
  const [busy,    setBusy]    = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const handleFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setBusy(true);
    setErr(null);
    const added: Resume[] = [];
    for (const file of Array.from(files)) {
      try {
        const text = await extractResumeText(file);
        if (!text.trim()) throw new Error("File is empty or unreadable.");
        // Strip the extension so the displayed name stays clean.
        const baseName = file.name.replace(/\.[^.]+$/, "");
        added.push({
          id:   Date.now() + Math.floor(Math.random() * 1000),
          name: baseName,
          text: text.trim(),
        });
      } catch (e) {
        setErr(`${file.name}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    if (added.length) onChange([...resumes, ...added]);
    setBusy(false);
    if (fileInput.current) fileInput.current.value = "";
  };

  const remove = (id: number) => onChange(resumes.filter((r) => r.id !== id));

  return (
    <Card>
      <CardTitle>Master resumes</CardTitle>
      <CardDesc>
        Add one or more variants — InterPrep picks the closest match for each
        job and tailors it to the JD before starting company research.
      </CardDesc>

      <div style={{ height: 12 }} />
      <div style={{ height: 1, background: T.border, marginBottom: 10 }} />

      {resumes.length === 0 ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Icon name="briefcase" size={13} color="#F59E0B" />
          <span style={{ fontSize: 12, color: T.textTertiary, letterSpacing: "-0.12px" }}>
            No resumes yet — add one below.
          </span>
        </div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {resumes.map((r) => (
            <div key={r.id} style={{
              display: "flex", alignItems: "center", gap: 10,
              padding: "8px 10px", borderRadius: 8,
              background: T.surface, border: `0.5px solid ${T.border}`,
            }}>
              <Icon name="note" size={13} color={T.accent} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontSize: 13, fontWeight: 600, color: T.text, letterSpacing: "-0.3px" }}>{r.name}</div>
                <div style={{ fontSize: 11, color: T.textTertiary, letterSpacing: "-0.11px" }}>
                  {r.text.length.toLocaleString()} characters
                </div>
              </div>
              <button
                onClick={() => remove(r.id)}
                onMouseEnter={(e) => { e.currentTarget.style.background = T.surface2; }}
                onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                style={{
                  display: "flex", alignItems: "center", gap: 5,
                  padding: "5px 10px", borderRadius: 14, border: "none",
                  background: "transparent", color: "#EF4444",
                  fontSize: 11, cursor: "pointer", fontFamily: T.fontBody,
                }}
              >
                <Icon name="trash" size={11} /> Remove
              </button>
            </div>
          ))}
        </div>
      )}

      <div style={{ height: 12 }} />

      <input
        ref={fileInput}
        type="file"
        accept=".pdf,.docx,.md,.txt"
        multiple
        style={{ display: "none" }}
        onChange={(e) => handleFiles(e.target.files)}
      />
      <div
        onClick={() => !busy && fileInput.current?.click()}
        onDragOver={(e) => { e.preventDefault(); e.currentTarget.style.borderColor = T.accent; }}
        onDragLeave={(e) => { e.currentTarget.style.borderColor = T.border; }}
        onDrop={(e) => {
          e.preventDefault();
          e.currentTarget.style.borderColor = T.border;
          handleFiles(e.dataTransfer.files);
        }}
        style={{
          border: `1px dashed ${T.border}`, borderRadius: 12,
          padding: 22, textAlign: "center", cursor: busy ? "default" : "pointer",
          background: T.surface, opacity: busy ? 0.6 : 1,
        }}
      >
        <Icon name="upload" size={22} color={T.textTertiary} />
        <p style={{ marginTop: 8, fontSize: 13, color: T.text, fontWeight: 500, letterSpacing: "-0.13px" }}>
          {busy ? "Reading…" : "Drop a resume here, or click to choose"}
        </p>
        <p style={{ marginTop: 4, fontSize: 11, color: T.textTertiary, letterSpacing: "-0.11px" }}>
          PDF, DOCX, MD, or TXT
        </p>
      </div>

      {err && <p style={{ fontSize: 11, color: "#EF4444", marginTop: 8 }}>{err}</p>}
    </Card>
  );
};
const FieldLabel = ({ children }: { children: ReactNode }) => (
  <div style={{ fontSize: 12, color: T.textSecondary, marginBottom: 4, letterSpacing: "-0.12px" }}>{children}</div>
);

const fieldInputStyle = (): CSSProperties => ({
  width: "100%", padding: "10px 12px", borderRadius: 8,
  border: `0.5px solid ${T.border}`, background: T.surface,
  color: T.text, fontSize: 13, fontFamily: T.fontBody,
  outline: "none", letterSpacing: "-0.13px",
});

interface CredentialFieldProps {
  value: string;
  placeholder?: string;
  onChange: (v: string) => void;
}

const PlainField = ({ value, placeholder, onChange }: CredentialFieldProps) => (
  <input
    type="text"
    value={value}
    placeholder={placeholder}
    onChange={(e) => onChange(e.target.value)}
    onFocus={(e) => { e.target.style.borderColor = T.accent; e.target.style.boxShadow = T.accentRing; }}
    onBlur={(e)  => { e.target.style.borderColor = T.border; e.target.style.boxShadow = "none"; }}
    style={fieldInputStyle()}
  />
);

const SecretField = ({ value, placeholder, onChange }: CredentialFieldProps) => {
  const [visible, setVisible] = useState(false);
  return (
    <div style={{ display: "flex", gap: 6 }}>
      <input
        type={visible ? "text" : "password"}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        onFocus={(e) => { e.target.style.borderColor = T.accent; e.target.style.boxShadow = T.accentRing; }}
        onBlur={(e)  => { e.target.style.borderColor = T.border; e.target.style.boxShadow = "none"; }}
        style={{ ...fieldInputStyle(), flex: 1 }}
      />
      <button
        type="button"
        onClick={() => setVisible((v) => !v)}
        title={visible ? "Hide" : "Show"}
        style={{
          width: 36, height: 36, borderRadius: 8,
          background: T.surface2, color: T.textSecondary,
          border: `0.5px solid ${T.border}`, cursor: "pointer",
          display: "flex", alignItems: "center", justifyContent: "center",
          flexShrink: 0, padding: 0,
        }}
      >
        <Icon name={visible ? "eyeOff" : "eye"} size={14} />
      </button>
    </div>
  );
};

// ─── Main app ─────────────────────────────────────────────────────────────

const App = () => {
  const [jobs, setJobs] = useState<Job[]>([]);
  /** Becomes `true` once `list_jobs` has resolved, so we don't save the
   *  initial empty array over a real file before the load completes. */
  const [jobsLoaded, setJobsLoaded] = useState(false);
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [selectedChatId, setSelectedChatId] = useState<string | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [showNewJobModal, setShowNewJobModal] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [activeScreen, setActiveScreen] = useState<Screen>("chat");

  // ── Backend lifecycle ──────────────────────────────────────────────────
  // The Python sidecar starts in the background. Subscribe to its lifecycle
  // events AND poll the current status on mount so we don't miss the ready
  // event if the page subscribes too late.
  const [backend, setBackend] = useState<BackendStatus>({ status: "starting" });

  useEffect(() => {
    let unsubReady:  UnlistenFn | undefined;
    let unsubError:  UnlistenFn | undefined;

    invoke<BackendStatus>("backend_status")
      .then((s) => setBackend(s))
      .catch((e) => console.error("backend_status failed:", e));

    // StrictMode-safe: see chat:* listeners below for the same pattern.
    let cancelled = false;
    listen<string>("sidecar:ready", (e) => setBackend({ status: "ready", url: e.payload }))
      .then((un) => { if (cancelled) un(); else unsubReady = un; });
    listen<string>("sidecar:error", (e) => setBackend({ status: "failed", error: e.payload }))
      .then((un) => { if (cancelled) un(); else unsubError = un; });

    return () => {
      cancelled = true;
      unsubReady?.();
      unsubError?.();
    };
  }, []);

  // ── Chat stream listeners ──────────────────────────────────────────────
  // The Rust side broadcasts `chat:token` / `chat:log` / `chat:done` /
  // `chat:error` events for whichever stream is currently active. The
  // frontend appends to the most recent AI message in the current thread —
  // since we disable send while a stream is running, there's only ever one.
  const streamingTargetRef = useRef<{ jobId: string; chatId: string } | null>(null);
  /// Mirror of `streamingTargetRef.current?.chatId` exposed as React state so
  /// the loader indicator can be scoped to the active thread.
  const [streamingChatId, setStreamingChatId] = useState<string | null>(null);
  /// Buffers the tailored-resume text emitted mid-stream so we can persist it
  /// to the Job *and* hand it to the Company-Research chain on `chat:done`.
  const pendingTailoredResumeRef = useRef<string | null>(null);

  // Refs so listener callbacks can read latest credentials + jobs without
  // re-registering listeners on every state change.
  const credentialsRef = useRef<Credentials>(EMPTY_CREDS);
  const jobsRef        = useRef<Job[]>([]);
  /// Populated once `startCompanyResearchFor` is defined further down. Lets
  /// the `chat:done` listener (registered in a one-shot useEffect) call into
  /// the latest closure.
  const startCompanyResearchForRef = useRef<((job: Job, tailoredResume: string) => void) | null>(null);

  useEffect(() => {
    // StrictMode-safe listener registration. React's dev StrictMode runs
    // effects twice (mount → cleanup → mount). With `listen().then(u =>
    // unsubs.push(u))`, the first cleanup fires BEFORE the async listen()
    // resolves, so its `u` never lands in `unsubs` and the first set of
    // listeners stays registered forever. Two live `chat:done` listeners
    // then race: one chains Prep→Research and sets the next streamingTargetRef,
    // the other immediately nulls it, dropping every subsequent SSE event.
    //
    // The fix: a `cancelled` flag + `register()` helper that either pushes
    // each unlisten into the active list or unsubscribes it inline if
    // cleanup already ran.
    let cancelled = false;
    const cleanups: UnlistenFn[] = [];
    const register = async <T,>(
      ev: string,
      handler: (e: { payload: T }) => void,
    ) => {
      const un = await listen<T>(ev, handler);
      if (cancelled) {
        un();
      } else {
        cleanups.push(un);
      }
    };

    const updateJob = (jobId: string, mutate: (j: Job) => Job) => {
      setJobs((prev) => prev.map((j) => j.id === jobId ? mutate(j) : j));
    };

    const appendToLast = (mutate: (m: ChatMsg) => ChatMsg) => {
      const target = streamingTargetRef.current;
      if (!target) return;
      updateJob(target.jobId, (j) => ({
        ...j,
        chats: j.chats.map((c) =>
          c.id !== target.chatId
            ? c
            : {
                ...c,
                messages: c.messages.map((m, i) =>
                  i === c.messages.length - 1 ? mutate(m) : m,
                ),
              },
        ),
      }));
    };

    register<string>("chat:token", (e) => {
      appendToLast((m) => ({ ...m, content: m.content + e.payload }));
    });

    register<string>("chat:log", (e) => {
      appendToLast((m) => ({ ...m, logs: [...(m.logs ?? []), e.payload] }));
    });

    register<Scorecard>("chat:scorecard", (e) => {
      const target = streamingTargetRef.current;
      if (!target) return;
      updateJob(target.jobId, (j) => ({ ...j, scorecard: e.payload }));
    });

    register<string>("chat:resume_docx", async (e) => {
      const target = streamingTargetRef.current;
      if (!target) return;
      try {
        const path = await invoke<string>("save_resume_docx", {
          jobId: target.jobId,
          b64:   e.payload,
        });
        updateJob(target.jobId, (j) => ({ ...j, resumeDocxPath: path }));
      } catch (err) {
        console.error("save_resume_docx failed:", err);
      }
    });

    register<string>("chat:tailored_resume", (e) => {
      const target = streamingTargetRef.current;
      if (!target) return;
      pendingTailoredResumeRef.current = e.payload;
      updateJob(target.jobId, (j) => ({ ...j, tailoredResume: e.payload }));
    });

    register<string>("chat:done", () => {
      const target = streamingTargetRef.current;
      appendToLast((m) => ({ ...m, streaming: false }));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      setIsLoading(false);

      // Chain: Application Prep → Company Research, using the tailored
      // resume as candidate context. We resolve the thread from the just-
      // finished `target` because state hasn't refreshed yet.
      if (!target) return;
      const job = jobsRef.current.find((j) => j.id === target.jobId);
      if (!job) return;
      const thread = job.chats.find((c) => c.id === target.chatId);
      if (!thread || thread.title !== "Application Prep") return;

      const tailored = pendingTailoredResumeRef.current ?? job.tailoredResume ?? "";
      pendingTailoredResumeRef.current = null;
      startCompanyResearchForRef.current?.(job, tailored);
    });

    register<string>("chat:error", (e) => {
      appendToLast((m) => ({
        ...m,
        streaming: false,
        content: m.content || `**Error:** ${e.payload}`,
      }));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      pendingTailoredResumeRef.current = null;
      setIsLoading(false);
    });

    return () => {
      cancelled = true;
      cleanups.forEach((u) => u());
    };
  }, []);

  // ── Credentials (Windows Credential Manager via Tauri) ─────────────────
  // Loaded once on mount, kept in local state while the Settings modal is
  // open, written back to the OS keychain when the modal closes.
  const [credentials, setCredentials] = useState<Credentials>(EMPTY_CREDS);
  const [credentialsLoaded, setCredentialsLoaded] = useState(false);

  useEffect(() => {
    invoke<Credentials>("load_credentials")
      .then((c) => setCredentials(c))
      .catch((e) => console.error("load_credentials failed:", e))
      .finally(() => setCredentialsLoaded(true));
  }, []);

  // Keep refs in sync so the one-shot SSE listeners read latest state.
  useEffect(() => { credentialsRef.current = credentials; }, [credentials]);
  useEffect(() => { jobsRef.current        = jobs; },        [jobs]);

  // ── Resume library ─────────────────────────────────────────────────────
  // Load on mount, save on every mutation (cheap; the file is small).
  const [resumes, setResumes] = useState<Resume[]>([]);
  const [resumesLoaded, setResumesLoaded] = useState(false);

  useEffect(() => {
    invoke<Resume[]>("list_resumes")
      .then((r) => setResumes(r))
      .catch((e) => console.error("list_resumes failed:", e))
      .finally(() => setResumesLoaded(true));
  }, []);

  useEffect(() => {
    if (!resumesLoaded) return;
    invoke("save_resumes", { resumes }).catch((e) =>
      console.error("save_resumes failed:", e),
    );
  }, [resumes, resumesLoaded]);

  // ── Load persisted jobs on mount ────────────────────────────────────────
  useEffect(() => {
    invoke<Job[]>("list_jobs")
      .then((loaded) => {
        // First-run fallback: if the on-disk file is empty, seed with the
        // sample jobs so the user has something to look at and explore.
        const list = loaded.length > 0 ? loaded : INITIAL_JOBS;
        setJobs(list);
        const first = list.find((j) => !j.archived);
        setSelectedJobId(first?.id ?? null);
        setSelectedChatId(first?.chats[0]?.id ?? null);
      })
      .catch((e) => {
        console.error("list_jobs failed, falling back to seed:", e);
        setJobs(INITIAL_JOBS);
        setSelectedJobId(INITIAL_JOBS[0].id);
        setSelectedChatId(INITIAL_JOBS[0].chats[0]?.id ?? null);
      })
      .finally(() => setJobsLoaded(true));
  }, []);

  // ── Persist on every change (cheap; jobs.json is small) ────────────────
  useEffect(() => {
    if (!jobsLoaded) return;
    invoke("save_jobs", { jobs }).catch((e) =>
      console.error("save_jobs failed:", e),
    );
  }, [jobs, jobsLoaded]);

  const selectedJob  = jobs.find((j) => j.id === selectedJobId) ?? null;
  const selectedChat = selectedJob?.chats.find((c) => c.id === selectedChatId) ?? null;

  const onSelectJob = (jobId: string) => {
    setSelectedJobId(jobId);
    const j = jobs.find((x) => x.id === jobId);
    setSelectedChatId(j?.chats[0]?.id ?? null);
  };

  // ── Archive / restore / delete actions ────────────────────────────────
  // All three mutate `jobs` — the save-on-change effect above persists.

  const reselectAfterRemoval = (currentList: Job[]) => {
    if (selectedJobId && currentList.some((j) => j.id === selectedJobId && !j.archived)) {
      return; // Selected job still active — leave selection alone.
    }
    const next = currentList.find((j) => !j.archived) ?? null;
    setSelectedJobId(next?.id ?? null);
    setSelectedChatId(next?.chats[0]?.id ?? null);
  };

  const onArchiveJob = (id: string) => {
    setJobs((prev) => {
      const next = prev.map((j) => j.id === id ? { ...j, archived: true } : j);
      reselectAfterRemoval(next);
      return next;
    });
  };

  const onUnarchiveJob = (id: string) => {
    setJobs((prev) => prev.map((j) => j.id === id ? { ...j, archived: false } : j));
  };

  const onDeleteJob = (id: string) => {
    setJobs((prev) => {
      const next = prev.filter((j) => j.id !== id);
      reselectAfterRemoval(next);
      return next;
    });
  };

  const onUpdateJob = (id: string, updater: (j: Job) => Job) => {
    setJobs((prev) => prev.map((j) => j.id === id ? updater(j) : j));
  };

  const onSendMessage = (text: string) => {
    if (!selectedJobId) return;
    let chatId = selectedChatId;

    if (!chatId) {
      chatId = `c-${Date.now()}`;
      const newChat: ChatThread = { id: chatId, title: text.slice(0, 30) + "...", preview: text, messages: [] };
      setJobs((prev) => prev.map((j) => j.id === selectedJobId ? { ...j, chats: [...j.chats, newChat] } : j));
      setSelectedChatId(chatId);
    }
    const targetChatId = chatId;

    // Append the user's message AND an empty AI bubble that the SSE stream
    // will fill in tokens-first. Keeping them in one setJobs call avoids a
    // wasted re-render between the two appends.
    const aiPlaceholder: ChatMsg = { role: "ai", content: "", streaming: true, logs: [] };
    setJobs((prev) => prev.map((j) =>
      j.id !== selectedJobId
        ? j
        : {
            ...j,
            chats: j.chats.map((c) =>
              c.id !== targetChatId
                ? c
                : {
                    ...c,
                    messages: [
                      ...c.messages,
                      { role: "user", content: text },
                      aiPlaceholder,
                    ],
                  },
            ),
          },
    ));

    // Build a tight context blob the backend can prepend to its system prompt.
    const job = jobs.find((j) => j.id === selectedJobId);
    // Most-recently-added resume is treated as the active one. Trimmed to
    // 4k chars so the system prompt stays under reasonable token budgets.
    const resume = resumes.length > 0 ? resumes[resumes.length - 1] : null;

    const jobContext = job
      ? [
          `Company: ${job.company}`,
          `Role: ${job.role}`,
          job.location ? `Location: ${job.location}` : "",
          job.jobDescription
            ? `\nJob Description:\n${job.jobDescription.slice(0, 1500)}`
            : "",
          resume
            ? `\nCandidate Resume (${resume.name}):\n${resume.text.slice(0, 4000)}`
            : "",
        ].filter(Boolean).join("\n")
      : "";

    streamingTargetRef.current = { jobId: selectedJobId, chatId: targetChatId };
    setStreamingChatId(targetChatId);
    setIsLoading(true);

    // Build chronological history from completed (non-streaming, non-empty)
    // turns BEFORE we appended the new user message above. The backend uses
    // this to keep multi-turn context — critical for mock interviews.
    const threadForHistory = job?.chats.find((c) => c.id === targetChatId);
    const history: [string, string][] = threadForHistory
      ? threadForHistory.messages
          .filter((m) => !m.streaming && m.content.trim().length > 0)
          .map((m) => [m.role === "user" ? "user" : "assistant", m.content] as [string, string])
      : [];

    // Mock-interview threads switch the system prompt + model on the backend.
    const mode = threadForHistory?.title === "Mock Interview" ? "interviewer" : "coach";

    invoke("start_chat_stream", {
      message: text,
      jobContext,
      history,
      mode,
      apiKey: credentials.geminiApiKey,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== selectedJobId
          ? j
          : {
              ...j,
              chats: j.chats.map((c) =>
                c.id !== targetChatId
                  ? c
                  : {
                      ...c,
                      messages: c.messages.map((m, i) =>
                        i === c.messages.length - 1
                          ? { ...m, streaming: false, content: `**Error:** ${errMsg}` }
                          : m,
                      ),
                    },
              ),
            },
      ));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      setIsLoading(false);
    });
  };

  /// Chain-step: kick off Company Research with the tailored resume as
  /// candidate context. Called from the `chat:done` listener when the
  /// Application-Prep thread finishes. Creates the Research thread lazily so
  /// it only exists after Prep succeeds.
  const startCompanyResearchFor = (job: Job, tailoredResume: string) => {
    const creds = credentialsRef.current;
    const researchThreadId = `c-research-${job.id}`;
    // Seed the placeholder with a first log line so the user sees activity
    // immediately. Playwright cold-start + supervisor's first hop can take
    // ~30s before the backend emits its own stage banner; an empty bubble
    // during that window reads as "broken" even though it's working.
    const placeholderThread: ChatThread = {
      id: researchThreadId,
      title: "Company Research",
      messages: [{
        role: "ai",
        content: "",
        streaming: true,
        logs: ["**Spinning up company research** — warming Playwright + supervisor…"],
      }],
    };
    // flushSync commits the placeholder + streaming target synchronously so
    // the SSE listener can find the chat by id when the first event arrives.
    // Without this, Rust can emit `chat:log` before React commits the
    // placeholder; the listener's `chats.map(c.id === target.chatId)` finds
    // no match and silently drops the event.
    flushSync(() => {
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : { ...j, chats: [...j.chats, placeholderThread] },
      ));
      streamingTargetRef.current = { jobId: job.id, chatId: researchThreadId };
      setStreamingChatId(researchThreadId);
      setIsLoading(true);
    });

    // Don't auto-switch view — user is reading the cover letter + scorecard
    // on the Prep thread; jerking them to Research is jarring. Research is
    // available via the sidebar once it lands.

    invoke("start_company_research_stream", {
      company:           job.company,
      role:              job.role,
      location:          job.location,
      jobDescription:    job.jobDescription ?? "",
      tailoredResume,
      apiKey:            creds.geminiApiKey,
      glassdoorEmail:    creds.glassdoorEmail,
      glassdoorPassword: creds.glassdoorPassword,
      indeedEmail:       creds.indeedEmail,
      indeedPassword:    creds.indeedPassword,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== researchThreadId ? c : {
            ...c,
            messages: c.messages.map((m, i) =>
              i === c.messages.length - 1
                ? { ...m, streaming: false, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      setIsLoading(false);
    });
  };

  // Publish to ref so the chat:done listener (registered once, outside this
  // closure) can invoke the latest version on completion of Prep.
  startCompanyResearchForRef.current = startCompanyResearchFor;

  /// Simulate a recruiter knockout phone-screen for the given job. Requires
  /// `tailoredResume` (from Application Prep). Creates a "Knockout Screen"
  /// thread and streams the predicted Q&A dossier.
  const startKnockoutScreen = (job: Job) => {
    const resume = job.tailoredResume?.trim();
    if (!resume) {
      alert("Run Application Prep first — the knockout screen needs the tailored resume.");
      return;
    }
    const koThreadId = `c-knockout-${job.id}-${Date.now()}`;
    setJobs((prev) => prev.map((j) =>
      j.id !== job.id ? j : {
        ...j,
        chats: [...j.chats, {
          id: koThreadId,
          title: "Knockout Screen",
          messages: [{ role: "ai", content: "", streaming: true, logs: [] }],
        }],
      },
    ));
    setSelectedChatId(koThreadId);

    streamingTargetRef.current = { jobId: job.id, chatId: koThreadId };
    setStreamingChatId(koThreadId);
    setIsLoading(true);
    invoke("start_knockout_screen_stream", {
      company:        job.company,
      role:           job.role,
      location:       job.location,
      jobDescription: job.jobDescription ?? "",
      tailoredResume: resume,
      apiKey:         credentialsRef.current.geminiApiKey,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== koThreadId ? c : {
            ...c,
            messages: c.messages.map((m, i) =>
              i === c.messages.length - 1
                ? { ...m, streaming: false, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      setIsLoading(false);
    });
  };

  const onCreateJob = (form: NewJobFormState) => {
    // Gate: can't tailor without at least one master resume on file.
    if (resumes.length === 0) {
      alert("Add at least one master resume in Settings → Resume before creating a job.");
      return;
    }

    const id = `job-${Date.now()}`;
    const palette = ["#6366F1", "#10B981", "#F59E0B", "#EC4899", "#8B5CF6"];
    const prepThreadId = `c-prep-${id}`;
    const jd = form.jobDescription.trim();
    const newJob: Job = {
      id,
      company: form.company,
      role: form.role,
      location: form.location,
      url: form.url,
      status: form.status,
      appliedDate: new Date().toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" }),
      currentStage: 0,
      stageNotes: {
        0: {
          date: new Date().toLocaleDateString("en-US", { month: "short", day: "numeric" }),
          outcome: "Submitted",
          notes: form.notes || "Application submitted.",
        },
      },
      avatar: form.company[0]?.toUpperCase() ?? "?",
      avatarColor: palette[jobs.length % palette.length],
      chats: [
        { id: `c-new-${id}`, title: "General Prep", preview: "Start your prep here...", messages: [] },
        // Application Prep: tailored resume + cover letter + scorecard. The
        // SSE stream fills the placeholder AI message; on `done`, the
        // listener chains into a Company Research thread automatically.
        {
          id: prepThreadId,
          title: "Application Prep",
          messages: [{ role: "ai", content: "", streaming: true, logs: [] }],
        },
      ],
      jobDescription: jd || undefined,
    };
    setJobs((prev) => [...prev, newJob]);
    setSelectedJobId(id);
    setSelectedChatId(prepThreadId);
    setShowNewJobModal(false);

    // Ship every master resume to the backend — the LLM picks the best fit.
    const masterResumes: [string, string][] = resumes.map((r) => [r.name, r.text]);

    streamingTargetRef.current = { jobId: id, chatId: prepThreadId };
    setStreamingChatId(prepThreadId);
    setIsLoading(true);
    invoke("start_application_tailor_stream", {
      company:        newJob.company,
      role:           newJob.role,
      location:       newJob.location,
      jobDescription: jd,
      masterResumes,
      apiKey:         credentials.geminiApiKey,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== prepThreadId ? c : {
            ...c,
            messages: c.messages.map((m, i) =>
              i === c.messages.length - 1
                ? { ...m, streaming: false, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      streamingTargetRef.current = null;
      setStreamingChatId(null);
      setIsLoading(false);
    });
  };

  return (
    <div style={{
      display: "flex", height: "100vh", width: "100vw",
      background: T.bg, color: T.text, fontFamily: T.fontBody, overflow: "hidden",
    }}>
      <Sidebar
        jobs={jobs}
        selectedJobId={selectedJobId}
        selectedChatId={selectedChatId}
        onSelectJob={onSelectJob}
        onSelectChat={setSelectedChatId}
        onNewJob={() => setShowNewJobModal(true)}
        onSettings={() => setShowSettings(true)}
        onArchiveJob={onArchiveJob}
        onUnarchiveJob={onUnarchiveJob}
        onDeleteJob={onDeleteJob}
        collapsed={sidebarCollapsed}
        activeScreen={activeScreen}
        onSetScreen={setActiveScreen}
      />

      <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", minWidth: 0 }}>
        {activeScreen === "timeline" ? (
          <Gantt
            jobs={jobs}
            onSelectJob={(id) => { onSelectJob(id); setActiveScreen("chat"); }}
            onNewJob={() => setShowNewJobModal(true)}
            onToggleSidebar={() => setSidebarCollapsed((c) => !c)}
            onUpdateJob={onUpdateJob}
          />
        ) : (
          <>
            <WorkspaceHeader
              job={selectedJob}
              onToggleSidebar={() => setSidebarCollapsed((c) => !c)}
              backend={backend}
            />
            {selectedJob ? (
              <>
                <ChatArea
                  chat={selectedChat}
                  job={selectedJob}
                  onSendMessage={onSendMessage}
                  isLoading={isLoading}
                  streamingChatId={streamingChatId}
                  onOpenResumeDocx={(path) => {
                    invoke("open_path", { path }).catch((e) =>
                      console.error("open_path failed:", e),
                    );
                  }}
                  onSimulateKnockout={() => {
                    if (selectedJob) startKnockoutScreen(selectedJob);
                  }}
                />
                <InputComposer onSend={onSendMessage} disabled={isLoading} />
              </>
            ) : (
              <EmptyState onNewJob={() => setShowNewJobModal(true)} />
            )}
          </>
        )}
      </div>

      {showNewJobModal && <NewJobModal onClose={() => setShowNewJobModal(false)} onSubmit={onCreateJob} />}
      {showSettings && (
        <SettingsModal
          credentials={credentials}
          onCredentialsChange={setCredentials}
          resumes={resumes}
          onResumesChange={setResumes}
          onClose={() => {
            setShowSettings(false);
            // Flush credentials to the OS keychain. Don't block close on it.
            if (credentialsLoaded) {
              invoke("save_credentials", { credentials }).catch((e) =>
                console.error("save_credentials failed:", e),
              );
            }
          }}
        />
      )}
    </div>
  );
};

export default App;
