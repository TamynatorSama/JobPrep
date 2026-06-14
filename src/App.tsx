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
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
// pdf.js needs a worker. Vite emits the worker script as a separate URL asset
// (the `?url` suffix), so this import is just a string — it does NOT pull the
// pdf.js library into the main bundle. The library itself + mammoth are
// dynamically imported on first use (see extractResumeText) so they don't bloat
// the initial UI load — most sessions never upload a resume.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

// ─── Resume file parsing ──────────────────────────────────────────────────

interface ResumeExtract {
  /** Plain-text content used as evidence by the LLM. */
  text: string;
  /** Base64-encoded raw .docx bytes — only set for .docx uploads. The backend
   *  uses these as the styling base for the tailored output. */
  docx_b64?: string;
}

function arrayBufferToBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let binary = "";
  // Chunked to dodge call-stack limits on very large files.
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(
      null,
      Array.from(bytes.subarray(i, i + chunk)),
    );
  }
  return btoa(binary);
}

/** Extracts plain text (and raw .docx bytes when applicable) from PDF/DOCX/MD/TXT.
 *  Throws on unknown extension. */
async function extractResumeText(file: File): Promise<ResumeExtract> {
  const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "txt" || ext === "md") {
    return { text: await file.text() };
  }
  if (ext === "docx") {
    const buf = await file.arrayBuffer();
    const mammoth = (await import("mammoth")).default;
    const result = await mammoth.extractRawText({ arrayBuffer: buf });
    return { text: result.value, docx_b64: arrayBufferToBase64(buf) };
  }
  if (ext === "pdf") {
    const buf = await file.arrayBuffer();
    const pdfjs = await import("pdfjs-dist");
    pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;
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
    return { text: pages.join("\n\n") };
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
  /** Id of the stream currently filling this bubble. Lets several streams run
   *  at once — each event routes to its own message by this id — and is cleared
   *  (with `streaming`) when the stream ends. Not persisted meaningfully. */
  streamId?: string;
  /** Sent to the backend but not rendered (e.g. the Mock Interview kickoff). */
  hidden?: boolean;
}

/** Pre-start configuration for a Mock Interview thread. */
interface InterviewConfig {
  focus: string;       // Behavioral | Technical | System Design | Full Loop
  difficulty: string;  // Junior | Mid | Senior | Staff
  tone: string;        // Friendly | Neutral | Tough
  length: string;      // e.g. "Standard (8–12 questions)"
}

interface ChatThread {
  id: string;
  title: string;
  preview?: string;
  messages: ChatMsg[];
  /** "interviewer" drives the live mock-interview persona; default "coach". */
  mode?: "coach" | "interviewer";
  /** Set on Mock Interview threads from the pre-start modal. */
  interviewConfig?: InterviewConfig;
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
  /** Base64-encoded raw .docx bytes when the upload was a .docx. Sent to the
   *  backend as the styling base for in-place section editing so the
   *  tailored output preserves the original document's fonts and layout. */
  docx_b64?: string | null;
}

type LlmProvider = "gemini" | "openai" | "anthropic";

interface Credentials {
  /** Which provider powers every AI call. The keys for all providers are kept
   *  so switching is instant and company research can use spares as fallback
   *  lanes. */
  llmProvider:      string;
  geminiApiKey:     string;
  openaiApiKey:     string;
  anthropicApiKey:  string;
}

const EMPTY_CREDS: Credentials = {
  llmProvider: "gemini", geminiApiKey: "", openaiApiKey: "",
  anthropicApiKey: "",
};

/** Snake_cased provider config attached to every backend request — mirrors
 *  the Python `LLMConfig` pydantic model. */
const llmPayload = (c: Credentials) => ({
  provider:          c.llmProvider || "gemini",
  gemini_api_key:    c.geminiApiKey,
  openai_api_key:    c.openaiApiKey,
  anthropic_api_key: c.anthropicApiKey,
});

interface Scorecard {
  verbatim_match_score?: number;
  role_title_alignment?: "Yes" | "No";
  quantification_check?: "Pass" | "Needs Work";
  hire_recommendation?: "Hire" | "No Hire";
  skills_matched?: string[];
  resumes_used?: string[];
  aggregation_notes?: string;
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
  /** Display metadata parsed at capture time by the browser extension. */
  employmentType?: string;
  workMode?: string;
  salary?: string;
  posted?: string;
  requirements?: string[];
  /** Hidden from the main sidebar list once true. */
  archived?: boolean;
  /** Plain-text tailored resume produced by the Application-Prep step. */
  tailoredResume?: string;
  /** Absolute path to the generated `resume.docx` on disk. */
  resumeDocxPath?: string;
  /** ATS scorecard returned by the tailoring step. */
  scorecard?: Scorecard;
  /** Outcome of the most recent completed mock interview. */
  lastInterview?: { outcome: string; date: string; chatId: string };
}

type Screen = "chat" | "timeline";

// ─── Mock-interview config ─────────────────────────────────────────────────

const INTERVIEW_OPTIONS = {
  focus:      ["Behavioral", "Technical", "System Design", "Full Loop"],
  difficulty: ["Junior", "Mid", "Senior", "Staff"],
  tone:       ["Friendly", "Neutral", "Tough"],
  length:     ["Short (5–6 questions)", "Standard (8–12 questions)", "Extended (12–15 questions)"],
} as const;

const DEFAULT_INTERVIEW_CONFIG: InterviewConfig = {
  focus: "Full Loop", difficulty: "Senior", tone: "Neutral", length: "Standard (8–12 questions)",
};

const INTERVIEW_DONE_MARKER = "--- INTERVIEW COMPLETE ---";

const formatInterviewSetup = (c: InterviewConfig): string =>
  [
    "INTERVIEW SETUP (obey strictly):",
    `- Focus: ${c.focus}`,
    `- Candidate level / difficulty: ${c.difficulty}`,
    `- Interviewer tone: ${c.tone}`,
    `- Length: ${c.length}`,
  ].join("\n");

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

// Theme-mapped component overrides so react-markdown output matches the dark
// UI tokens. Defined at module scope so the object identity is stable across
// re-renders (the bubble re-renders on every streamed token).
const MD_COMPONENTS: Components = {
  p:      (props) => <p style={{ margin: "0 0 6px" }} {...props} />,
  strong: (props) => <strong style={{ color: T.text, fontWeight: 700 }} {...props} />,
  em:     (props) => <em style={{ fontStyle: "italic" }} {...props} />,
  a:      (props) => <a style={{ color: T.accent, textDecoration: "underline" }} target="_blank" rel="noreferrer" {...props} />,
  h1:     (props) => <h1 style={{ fontSize: 18,   fontWeight: 700, color: T.text, margin: "14px 0 6px", lineHeight: 1.3 }} {...props} />,
  h2:     (props) => <h2 style={{ fontSize: 16,   fontWeight: 700, color: T.text, margin: "12px 0 5px", lineHeight: 1.3 }} {...props} />,
  h3:     (props) => <h3 style={{ fontSize: 14.5, fontWeight: 700, color: T.text, margin: "10px 0 4px", lineHeight: 1.3 }} {...props} />,
  h4:     (props) => <h4 style={{ fontSize: 13.5, fontWeight: 700, color: T.text, margin: "8px 0 4px",  lineHeight: 1.3 }} {...props} />,
  ul:     (props) => <ul style={{ margin: "0 0 6px", paddingLeft: 20 }} {...props} />,
  ol:     (props) => <ol style={{ margin: "0 0 6px", paddingLeft: 20 }} {...props} />,
  li:     (props) => <li style={{ marginBottom: 3 }} {...props} />,
  blockquote: (props) => (
    <blockquote style={{ borderLeft: `3px solid ${T.accent}`, margin: "6px 0", padding: "2px 0 2px 12px", color: T.text }} {...props} />
  ),
  hr:     () => <hr style={{ border: "none", borderTop: `1px solid ${T.border}`, margin: "12px 0" }} />,
  pre:    (props) => (
    <pre style={{ background: T.surface2, padding: 10, borderRadius: 8, overflowX: "auto", margin: "6px 0", whiteSpace: "pre" }} {...props} />
  ),
  // react-markdown v9+ dropped the `inline` prop — a fenced block carries a
  // `language-*` className, inline code does not. Inline gets a chip; fenced
  // code is laid out by the `pre` wrapper above.
  code: ({ className, ...props }) =>
    /^language-/.test(className || "")
      ? <code className={className} style={{ fontFamily: "monospace", fontSize: 12.5 }} {...props} />
      : <code style={{ background: T.surface2, padding: "1px 5px", borderRadius: 4, fontFamily: "monospace", fontSize: 12.5, color: T.text }} {...props} />,
  table:  (props) => <table style={{ borderCollapse: "collapse", margin: "6px 0", width: "100%", fontSize: 13 }} {...props} />,
  th:     (props) => <th style={{ border: `1px solid ${T.border}`, padding: "4px 8px", textAlign: "left", color: T.text, fontWeight: 600 }} {...props} />,
  td:     (props) => <td style={{ border: `1px solid ${T.border}`, padding: "4px 8px" }} {...props} />,
};

const MarkdownText = ({ content }: MarkdownTextProps) => (
  <div style={{ fontSize: 14, lineHeight: 1.65, color: T.textSecondary }}>
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
      {content}
    </ReactMarkdown>
  </div>
);

// ─── Small style helper for popup menu rows ───────────────────────────────

// Distinct card for the post-interview feedback block (after the
// `--- INTERVIEW COMPLETE ---` marker), with a Pass/Borderline/Fail badge.
const FeedbackCard = ({ feedback }: { feedback: string }) => {
  const m = feedback.match(/Predicted outcome:?\**\s*(Pass|Borderline|Fail)/i);
  const outcome = m?.[1] ?? null;
  const color = outcome
    ? (/pass/i.test(outcome) ? "#22c55e" : /fail/i.test(outcome) ? "#ef4444" : "#f59e0b")
    : T.accent;
  return (
    <div style={{ marginTop: 10, border: `1px solid ${T.border}`, borderRadius: 14, overflow: "hidden", background: T.surface2 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "10px 16px", borderBottom: `1px solid ${T.border}` }}>
        <Icon name="analyze" size={14} color={color} />
        <span style={{ fontSize: 13, fontWeight: 700, color: T.text, fontFamily: T.fontDisplay }}>Interview Feedback</span>
        {outcome && (
          <span style={{ marginLeft: "auto", padding: "3px 10px", borderRadius: 100, background: `${color}22`, color, fontSize: 11, fontWeight: 600 }}>
            {outcome}
          </span>
        )}
      </div>
      <div style={{ padding: "4px 16px 12px" }}>
        <MarkdownText content={feedback} />
      </div>
    </div>
  );
};

// Copy / Regenerate pills shown under an AI bubble on hover.
const msgActionStyle: CSSProperties = {
  display: "flex", alignItems: "center", gap: 5,
  padding: "4px 8px", borderRadius: 100, border: "none",
  background: T.surface2, color: T.textSecondary,
  fontSize: 11, cursor: "pointer", fontFamily: T.fontBody,
};

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
  onDeleteChat: (jobId: string, chatId: string) => void;
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
                          {hovered === `c-${chat.id}` && (
                            <button
                              title="Delete chat"
                              onClick={(e) => { e.stopPropagation(); p.onDeleteChat(job.id, chat.id); }}
                              onMouseEnter={(e) => { e.currentTarget.style.color = "#ef4444"; }}
                              onMouseLeave={(e) => { e.currentTarget.style.color = T.textTertiary; }}
                              style={{
                                width: 20, height: 20, borderRadius: 6, border: "none",
                                background: "transparent", color: T.textTertiary, cursor: "pointer",
                                display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
                              }}
                            >
                              <Icon name="trash" size={11} />
                            </button>
                          )}
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
  /// Step launchers — each creates (and selects) the relevant chat thread.
  onStartApplicationPrep?: (job: Job) => void;
  onStartMockInterview?: (job: Job) => void;
}

const WorkspaceHeader = ({ job, onToggleSidebar, backend, onStartApplicationPrep, onStartMockInterview }: WorkspaceHeaderProps) => {
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

  // Each step button creates (and switches to) its chat thread on click.
  const actions: { icon: IconName; label: string; color: string; onClick?: () => void }[] = [
    { icon: "sparkle",   label: "Application Prep", color: "#10B981", onClick: () => job && onStartApplicationPrep?.(job) },
    { icon: "interview", label: "Mock Interview",   color: "#a855f7", onClick: () => job && onStartMockInterview?.(job) },
    { icon: "note",      label: "Add Note",         color: "#F59E0B" },
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
                onClick={a.onClick}
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
      {(card.resumes_used?.length ?? 0) > 0 && (
        <>
          <div style={{ fontSize: 10.5, color: T.textTertiary, marginTop: 10, marginBottom: 4 }}>
            Aggregated from
          </div>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
            {card.resumes_used!.map((s, i) => (
              <span key={i} style={{
                padding: "3px 8px", borderRadius: 100,
                background: T.surface2, color: T.textSecondary,
                fontSize: 11, whiteSpace: "nowrap",
              }}>
                {s}
              </span>
            ))}
          </div>
          {card.aggregation_notes && (
            <div style={{
              fontSize: 11.5, color: T.textSecondary,
              marginTop: 8, fontStyle: "italic", lineHeight: 1.4,
            }}>
              {card.aggregation_notes}
            </div>
          )}
        </>
      )}
    </div>
  );
};

interface ChatAreaProps {
  chat: ChatThread | null;
  job: Job | null;
  onSendMessage: (text: string) => void;
  /// Chat-thread ids with an active stream. Scopes the loading indicator to the
  /// thread the tokens flow into — several can stream at once.
  streamingChatIds: string[];
  /// Launches the saved .docx in the OS default app (Word, etc.).
  onOpenResumeDocx?: (path: string) => void;
  /// Kicks off the recruiter-knockout simulation thread for the current job.
  onSimulateKnockout?: () => void;
  /// Re-runs the most recent exchange: drops the last AI reply + its user
  /// prompt and re-sends that prompt. Only wired for the newest message.
  onRegenerate?: () => void;
  /// Ends a live mock interview and asks for structured feedback.
  onEndInterview?: () => void;
  /// Voice mode (spoken Q&A + barge-in) for interview threads.
  voiceEnabled?: boolean;
  voiceListening?: boolean;
  voiceStatus?: { available: boolean; device?: string; detail?: string } | null;
  onOpenVoice?: () => void;
}

const ChatArea = ({ chat, job, onSendMessage, streamingChatIds, onOpenResumeDocx, onSimulateKnockout, onRegenerate, onEndInterview, voiceEnabled, voiceListening, voiceStatus, onOpenVoice }: ChatAreaProps) => {
  // Loader is per-thread: draw the bouncing dots only in the thread whose
  // tokens are flowing, even though other threads may be streaming too.
  const chatStreaming = chat != null && streamingChatIds.includes(chat.id);
  const showLoader = chatStreaming;
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

  const isInterviewThread = chat.mode === "interviewer" || chat.title === "Mock Interview";
  const interviewComplete = chat.messages.some((m) => m.content.includes(INTERVIEW_DONE_MARKER));
  const questionCount = chat.messages.filter(
    (m) => m.role === "ai" && m.content.trim() && !m.content.includes(INTERVIEW_DONE_MARKER),
  ).length;

  return (
    <div style={{ flex: 1, overflowY: "auto", padding: "24px 0" }}>
      {isInterviewThread && (
        <div style={{
          position: "sticky", top: 0, zIndex: 5,
          display: "flex", alignItems: "center", gap: 10,
          margin: "0 24px 16px", padding: "8px 14px", borderRadius: 100,
          background: T.surface, border: `1px solid ${T.border}`, boxShadow: T.shadowMd,
        }}>
          <Icon name="interview" size={13} color={T.accent} />
          <span style={{ fontSize: 12, color: T.textSecondary, fontWeight: 500, letterSpacing: "-0.12px" }}>
            Mock Interview
            {chat.interviewConfig ? ` · ${chat.interviewConfig.focus} · ${chat.interviewConfig.tone}` : ""}
            {interviewComplete ? " · Complete" : questionCount > 0 ? ` · ~Q${questionCount}` : ""}
            {voiceEnabled && voiceListening ? " · 🎙 listening…" : ""}
            {voiceEnabled && voiceStatus && !voiceStatus.available ? " · voice unavailable" : ""}
            {voiceEnabled && voiceStatus?.available && voiceStatus.device ? ` · voice on (${voiceStatus.device.toUpperCase()})` : ""}
          </span>
          <button
            title={voiceEnabled ? "Open voice view" : "Voice interview (spoken Q&A)"}
            onClick={() => onOpenVoice?.()}
            style={{
              marginLeft: "auto", width: 28, height: 28, borderRadius: 100,
              border: `1px solid ${voiceEnabled ? T.accent : T.border}`,
              background: voiceEnabled ? T.accentSoft : T.surface2,
              color: voiceEnabled ? T.accent : T.textSecondary,
              cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
            }}
          >
            <Icon name="mic" size={13} />
          </button>
          {!interviewComplete && (
            <button
              onClick={() => onEndInterview?.()}
              disabled={chatStreaming}
              style={{
                padding: "5px 12px", borderRadius: 100,
                border: `1px solid ${T.border}`, background: T.surface2, color: T.text,
                fontSize: 11.5, fontWeight: 500, fontFamily: T.fontBody,
                cursor: chatStreaming ? "not-allowed" : "pointer", opacity: chatStreaming ? 0.5 : 1,
              }}
            >
              End &amp; get feedback
            </button>
          )}
        </div>
      )}
      {chat.messages.map((msg, i) => (
        msg.hidden ? null : (
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
                    : (() => {
                        const c = msg.streaming ? msg.content + "▊" : msg.content;
                        const idx = c.indexOf(INTERVIEW_DONE_MARKER);
                        if (idx === -1) return <MarkdownText content={c} />;
                        // Interview finished: render any closing line, then the
                        // structured feedback as a distinct card.
                        const pre = c.slice(0, idx).trim();
                        const fb = c.slice(idx + INTERVIEW_DONE_MARKER.length).trim();
                        return (
                          <>
                            {pre && <MarkdownText content={pre} />}
                            <FeedbackCard feedback={fb} />
                          </>
                        );
                      })()
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
                {hoveredMsg === i && !msg.streaming && (
                  <div style={{ display: "flex", gap: 4, marginTop: 6 }}>
                    <button
                      onClick={() => { void navigator.clipboard?.writeText(msg.content); }}
                      style={msgActionStyle}
                    >
                      <Icon name="copy" size={11} />Copy
                    </button>
                    {/* Regenerate only on the newest message, and only when a
                        user prompt precedes it (skips Prep/Research/Knockout
                        threads that have their own re-run controls). */}
                    {i === chat.messages.length - 1
                      && chat.messages.some((m) => m.role === "user") && (
                      <button onClick={() => onRegenerate?.()} style={msgActionStyle}>
                        <Icon name="refresh" size={11} />Regenerate
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
        )
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
  /// Draft text for the *currently selected* chat (lifted to the parent so each
  /// chat keeps its own in-progress message).
  value: string;
  onChange: (v: string) => void;
  onSend: (text: string) => void;
  disabled: boolean;
  /// Small line under the box (e.g. "queued — sending next"). Optional.
  hint?: string;
}

const InputComposer = ({ value, onChange, onSend, disabled, hint }: InputComposerProps) => {
  const taRef = useRef<HTMLTextAreaElement>(null);

  const send = () => {
    const t = value.trim();
    if (!t || disabled) return;
    onSend(t);
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
          onChange={(e) => onChange(e.target.value)}
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
        fontSize: 11, color: hint ? T.accent : T.textTertiary,
        textAlign: "center", marginTop: 6, letterSpacing: "-0.11px",
      }}>
        {hint || "InterPrep AI may make mistakes. Always verify important information."}
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

interface MockInterviewModalProps {
  job: Job;
  onClose: () => void;
  onStart: (config: InterviewConfig) => void;
}

const MockInterviewModal = ({ job, onClose, onStart }: MockInterviewModalProps) => {
  const [config, setConfig] = useState<InterviewConfig>(DEFAULT_INTERVIEW_CONFIG);
  const sel: CSSProperties = {
    width: "100%", padding: "10px 14px", borderRadius: 10,
    border: `0.5px solid ${T.border}`, background: T.bg, color: T.text,
    fontSize: 13, fontFamily: T.fontBody, outline: "none", cursor: "pointer",
    letterSpacing: "-0.13px",
  };
  const row = (label: string, key: keyof InterviewConfig, opts: readonly string[]) => (
    <Field label={label} id={key}>
      <select style={sel} value={config[key]} onChange={(e) => setConfig((c) => ({ ...c, [key]: e.target.value }))}>
        {opts.map((o) => <option key={o} value={o}>{o}</option>)}
      </select>
    </Field>
  );
  return (
    <div onClick={onClose} style={{
      position: "fixed", inset: 0, background: "rgba(0,0,0,0.7)", backdropFilter: "blur(8px)",
      display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1000,
    }}>
      <div onClick={(e) => e.stopPropagation()} style={{
        background: T.surface, borderRadius: 20, padding: 28, width: 440, maxWidth: "90vw",
        boxShadow: T.shadowLg, border: `0.5px solid ${T.border}`,
      }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 22 }}>
          <div>
            <h2 style={{ fontSize: 18, fontWeight: 700, color: T.text, letterSpacing: "-0.5px", fontFamily: T.fontDisplay }}>Mock Interview</h2>
            <p style={{ fontSize: 12, color: T.textSecondary, marginTop: 3, letterSpacing: "-0.12px" }}>{job.role} · {job.company}</p>
          </div>
          <button onClick={onClose} style={{
            background: T.surface2, border: "none", cursor: "pointer", padding: 6, borderRadius: 100,
            color: T.textSecondary, display: "flex", alignItems: "center", width: 28, height: 28, justifyContent: "center",
          }}>
            <Icon name="x" size={14} />
          </button>
        </div>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "0 14px" }}>
          {row("Focus", "focus", INTERVIEW_OPTIONS.focus)}
          {row("Difficulty", "difficulty", INTERVIEW_OPTIONS.difficulty)}
          {row("Interviewer tone", "tone", INTERVIEW_OPTIONS.tone)}
          {row("Length", "length", INTERVIEW_OPTIONS.length)}
        </div>
        <div style={{ display: "flex", gap: 10, marginTop: 8 }}>
          <button type="button" onClick={onClose} style={{
            flex: 1, padding: "10px 0", borderRadius: 100, border: "none",
            background: T.surface2, color: T.textSecondary, fontSize: 13, fontWeight: 500,
            cursor: "pointer", fontFamily: T.fontBody, letterSpacing: "-0.13px",
          }}>Cancel</button>
          <button type="button" onClick={() => onStart(config)} style={{
            flex: 1, padding: "10px 0", borderRadius: 100, border: "none",
            background: "#fff", color: "#0C0C0C", fontSize: 13, fontWeight: 600,
            cursor: "pointer", fontFamily: T.fontBody, letterSpacing: "-0.13px",
          }}>Start interview</button>
        </div>
      </div>
    </div>
  );
};

interface VoiceLevel { level: number; pitch: number; mode: string }

interface VoiceOverlayProps {
  levelRef: React.MutableRefObject<VoiceLevel>;
  phase: string;            // "speaking" | "listening" | "thinking" | "idle"
  subtitle?: string;
  bargeEnabled: boolean;
  onToggleBarge: () => void;
  onMinimize: () => void;   // back to chat, keep voice running
  onStop: () => void;       // turn voice off entirely
  onEnd: () => void;        // end the interview + get feedback
}

/// Full-screen voice visualizer: a canvas orb that morphs with the live audio
/// (RMS → size + core, ZCR pitch proxy → hue + wobble). Dismissible — the
/// transcript keeps flowing in the chat underneath.
const VoiceOverlay = ({ levelRef, phase, subtitle, bargeEnabled, onToggleBarge, onMinimize, onStop, onEnd }: VoiceOverlayProps) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    const size = 280;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = size * dpr;
    canvas.height = size * dpr;
    ctx.scale(dpr, dpr);
    const cx = size / 2, cy = size / 2;
    let raf = 0;
    let sl = 0, sp = 0; // smoothed level / pitch

    const draw = (t: number) => {
      const { level, pitch } = levelRef.current;
      sl += (Math.min(level * 3.2, 1) - sl) * 0.18;
      sp += (Math.min(pitch * 4, 1) - sp) * 0.07;
      ctx.clearRect(0, 0, size, size);

      const ph = phaseRef.current;
      const baseHue = ph === "listening" ? 160 : ph === "thinking" ? 210 : 265;
      const hue = baseHue + sp * 45;
      const baseR = 58 + sl * 40;

      // outer glow
      const grd = ctx.createRadialGradient(cx, cy, baseR * 0.2, cx, cy, baseR * 1.9);
      grd.addColorStop(0, `hsla(${hue},85%,66%,0.95)`);
      grd.addColorStop(0.55, `hsla(${hue},85%,55%,0.45)`);
      grd.addColorStop(1, `hsla(${hue},85%,50%,0)`);
      ctx.fillStyle = grd;
      ctx.beginPath();
      const pts = 72;
      for (let i = 0; i <= pts; i++) {
        const a = (i / pts) * Math.PI * 2;
        const wobble = Math.sin(a * 3 + t / 620) * 7 * sl + Math.sin(a * 5 - t / 430) * 5 * sp;
        const r = baseR + wobble;
        const x = cx + Math.cos(a) * r, y = cy + Math.sin(a) * r;
        i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
      }
      ctx.closePath();
      ctx.fill();

      // bright core
      ctx.beginPath();
      ctx.fillStyle = `hsla(${hue},92%,82%,0.92)`;
      ctx.arc(cx, cy, baseR * 0.42 + sl * 12, 0, Math.PI * 2);
      ctx.fill();

      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [levelRef]);

  const label = phase === "speaking" ? "Interviewer speaking…"
    : phase === "listening" ? "Listening…"
    : phase === "thinking" ? "Thinking…"
    : "Ready";

  const btn = (bg: string, color: string): CSSProperties => ({
    padding: "9px 18px", borderRadius: 100, border: "none", background: bg, color,
    fontSize: 13, fontWeight: 600, cursor: "pointer", fontFamily: T.fontBody, letterSpacing: "-0.13px",
  });

  return (
    <div style={{
      position: "fixed", inset: 0, zIndex: 1100,
      background: "rgba(8,8,12,0.88)", backdropFilter: "blur(12px)",
      display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: 26,
    }}>
      <button onClick={onMinimize} title="Back to chat" style={{
        position: "absolute", top: 18, right: 18, width: 34, height: 34, borderRadius: 100,
        border: `1px solid ${T.border}`, background: T.surface2, color: T.textSecondary,
        cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center",
      }}>
        <Icon name="x" size={16} />
      </button>

      <canvas ref={canvasRef} style={{ width: 280, height: 280 }} />

      <div style={{ textAlign: "center" }}>
        <p style={{ fontSize: 16, fontWeight: 700, color: T.text, fontFamily: T.fontDisplay, letterSpacing: "-0.3px" }}>{label}</p>
        {subtitle && <p style={{ fontSize: 12.5, color: T.textSecondary, marginTop: 5, letterSpacing: "-0.12px" }}>{subtitle}</p>}
      </div>

      <button
        onClick={onToggleBarge}
        title="Only enable with headphones — otherwise the mic hears the AI and cuts it off"
        style={{
          display: "flex", alignItems: "center", gap: 8, padding: "6px 14px", borderRadius: 100,
          border: `1px solid ${bargeEnabled ? T.accent : T.border}`,
          background: bargeEnabled ? T.accentSoft : "transparent",
          color: bargeEnabled ? T.accent : T.textSecondary,
          fontSize: 12, cursor: "pointer", fontFamily: T.fontBody,
        }}
      >
        <span style={{
          width: 28, height: 16, borderRadius: 100, background: bargeEnabled ? T.accent : T.surface2,
          position: "relative", transition: "background 0.15s", flexShrink: 0,
        }}>
          <span style={{
            position: "absolute", top: 2, left: bargeEnabled ? 14 : 2, width: 12, height: 12,
            borderRadius: 100, background: "#fff", transition: "left 0.15s",
          }} />
        </span>
        Let me interrupt (headphones)
      </button>

      <div style={{ display: "flex", gap: 10 }}>
        <button onClick={onMinimize} style={btn(T.surface2, T.text)}>Back to chat</button>
        <button onClick={onEnd} style={btn(T.surface2, T.text)}>End interview</button>
        <button onClick={onStop} style={btn("#ef4444", "#fff")}>Stop voice</button>
      </div>
    </div>
  );
};

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
  voxVoice: boolean;
  onVoxVoiceChange: (v: boolean) => void;
  panelSize: number;
  onPanelSizeChange: (n: number) => void;
  voiceStatus: { available: boolean; device?: string; detail?: string; vibe_available?: boolean; voices?: string[]; default_speaker?: string } | null;
  onClose: () => void;
}

const SettingsModal = ({
  credentials, onCredentialsChange,
  resumes, onResumesChange,
  voxVoice, onVoxVoiceChange, panelSize, onPanelSizeChange, voiceStatus,
  onClose,
}: SettingsModalProps) => {
  const [section, setSection] = useState("resume");
  const sections = [
    { id: "account",       label: "Account",        icon: "user"      as IconName },
    { id: "appearance",    label: "Appearance",     icon: "sun"       as IconName },
    { id: "resume",        label: "Resume",         icon: "upload"    as IconName },
    { id: "voice",         label: "Voice",          icon: "mic"       as IconName },
    { id: "notifications", label: "Notifications",  icon: "bell"      as IconName },
    { id: "data",          label: "Data & Privacy", icon: "briefcase" as IconName },
    { id: "apiKeys",       label: "API Keys",       icon: "key"       as IconName },
    { id: "integrations",  label: "Integrations",   icon: "link"      as IconName },
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
          ) : section === "integrations" ? (
            <IntegrationsTab />
          ) : section === "resume" ? (
            <ResumeTab resumes={resumes} onChange={onResumesChange} />
          ) : section === "voice" ? (
            <VoiceTab voxVoice={voxVoice} onVoxVoiceChange={onVoxVoiceChange} panelSize={panelSize} onPanelSizeChange={onPanelSizeChange} voiceStatus={voiceStatus} />
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

// ─── Voice tab ──────────────────────────────────────────────────────────

interface VoiceTabProps {
  voxVoice: boolean;
  onVoxVoiceChange: (v: boolean) => void;
  panelSize: number;
  onPanelSizeChange: (n: number) => void;
  voiceStatus: { available: boolean; device?: string; detail?: string; vibe_available?: boolean; voices?: string[]; default_speaker?: string } | null;
}

const VoiceTab = ({ voxVoice, onVoxVoiceChange, panelSize, onPanelSizeChange, voiceStatus }: VoiceTabProps) => {
  // Only block enabling VibeVoice when we positively know it's unavailable
  // (status reported it missing). If status hasn't loaded, allow the toggle.
  const vibeInstalled = voiceStatus?.vibe_available !== false;
  const blocked = !voxVoice && !vibeInstalled;
  const toggle = () => { if (!blocked) onVoxVoiceChange(!voxVoice); };
  // How many distinct preset voices VibeVoice ships (caps the panel size). When
  // status hasn't loaded (undefined) assume the full 4; a known-empty list means
  // no presets, but keep at least one button so the control still renders.
  const maxPanel = Math.min(4, voiceStatus?.voices?.length ?? 4) || 1;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <Card>
        <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 16 }}>
          <div style={{ flex: 1 }}>
            <CardTitle>Humanlike voice (VibeVoice)</CardTitle>
            <CardDesc>
              Off uses Piper — a fast voice that starts speaking almost instantly.
              On uses Microsoft VibeVoice for a warmer, more humanlike interviewer,
              at the cost of slower replies (noticeably slower without a GPU).
            </CardDesc>
          </div>
          <button
            type="button"
            onClick={toggle}
            aria-pressed={voxVoice}
            disabled={blocked}
            style={{
              flexShrink: 0, marginTop: 2, position: "relative",
              width: 36, height: 20, borderRadius: 100, border: "none",
              background: voxVoice ? T.accent : T.surface2,
              cursor: blocked ? "not-allowed" : "pointer",
              opacity: blocked ? 0.5 : 1, transition: "background 0.15s",
            }}
          >
            <div style={{
              position: "absolute", top: 2, left: voxVoice ? 18 : 2,
              width: 16, height: 16, borderRadius: "50%", background: "#fff",
              transition: "left 0.15s",
            }} />
          </button>
        </div>
        {voiceStatus?.device && (
          <CardHint>Running on {voiceStatus.device === "cuda" ? "GPU (fast)" : "CPU"}.</CardHint>
        )}
        {!vibeInstalled && (
          <CardHint>
            VibeVoice isn't installed — run <code>backend\setup.ps1 -Voice</code> to enable the humanlike voice.
          </CardHint>
        )}
      </Card>

      {voxVoice && (
        <Card>
          <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 16 }}>
            <div style={{ flex: 1 }}>
              <CardTitle>Interviewer panel</CardTitle>
              <CardDesc>
                Simulate a panel: each question is asked by a different voice,
                rotating through this many of VibeVoice's preset interviewers.
                Set to 1 for a single interviewer.
              </CardDesc>
            </div>
            <div style={{ display: "flex", gap: 4, flexShrink: 0, marginTop: 2 }}>
              {Array.from({ length: maxPanel }, (_, i) => i + 1).map((n) => (
                <button
                  key={n}
                  type="button"
                  onClick={() => onPanelSizeChange(n)}
                  aria-pressed={panelSize === n}
                  style={{
                    width: 28, height: 28, borderRadius: 8, cursor: "pointer",
                    border: `0.5px solid ${T.border}`,
                    background: panelSize === n ? T.accent : T.surface2,
                    color: panelSize === n ? "#fff" : T.text,
                    fontSize: 12, fontWeight: 600,
                  }}
                >
                  {n}
                </button>
              ))}
            </div>
          </div>
          {voiceStatus?.voices?.length ? (
            <CardHint>
              Panel voices: {voiceStatus.voices.slice(0, panelSize).join(", ")}.
            </CardHint>
          ) : null}
        </Card>
      )}
    </div>
  );
};

// ─── API Keys tab body ────────────────────────────────────────────────────

interface CredentialsTabProps {
  credentials: Credentials;
  update: (patch: Partial<Credentials>) => void;
}

const LLM_PROVIDERS: { id: LlmProvider; label: string }[] = [
  { id: "gemini",    label: "Gemini" },
  { id: "openai",    label: "OpenAI" },
  { id: "anthropic", label: "Claude" },
];

const KEY_CARDS: {
  id: LlmProvider;
  title: string;
  desc: string;
  placeholder: string;
  field: keyof Credentials;
}[] = [
  { id: "gemini",    title: "Gemini API Key",    desc: "Get yours from Google AI Studio. Also powers company-research web grounding — worth setting even when another provider is active.", placeholder: "AIza...",   field: "geminiApiKey" },
  { id: "openai",    title: "OpenAI API Key",    desc: "From platform.openai.com → API keys.",            placeholder: "sk-...",      field: "openaiApiKey" },
  { id: "anthropic", title: "Anthropic API Key", desc: "From console.anthropic.com → API keys.",          placeholder: "sk-ant-...",  field: "anthropicApiKey" },
];

const CredentialsTab = ({ credentials, update }: CredentialsTabProps) => {
  const active = (credentials.llmProvider || "gemini") as LlmProvider;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <Card>
        <CardTitle>AI Provider</CardTitle>
        <CardDesc>
          Pick which model powers chat, mock interviews, resume tailoring and
          research. Keys you enter for the other providers are kept and used as
          fallbacks by company research.
        </CardDesc>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {LLM_PROVIDERS.map((p) => (
            <button
              key={p.id}
              type="button"
              onClick={() => update({ llmProvider: p.id })}
              aria-pressed={active === p.id}
              style={{
                padding: "6px 14px", borderRadius: 8, cursor: "pointer",
                border: `0.5px solid ${T.border}`,
                background: active === p.id ? T.accent : T.surface2,
                color: active === p.id ? "#fff" : T.text,
                fontSize: 12, fontWeight: 600,
              }}
            >
              {p.label}
            </button>
          ))}
        </div>
        <CardHint>Stored in Windows Credential Manager — encrypted with your user account.</CardHint>
      </Card>

      {KEY_CARDS.map((card) => (
        <Card key={card.id}>
          <CardTitle>
            {card.title}
            {active === card.id ? " · Active" : ""}
          </CardTitle>
          <CardDesc>{card.desc}</CardDesc>
          <SecretField
            value={credentials[card.field]}
            placeholder={card.placeholder}
            onChange={(v) => update({ [card.field]: v } as Partial<Credentials>)}
          />
        </Card>
      ))}
    </div>
  );
};

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
        const extract = await extractResumeText(file);
        if (!extract.text.trim()) throw new Error("File is empty or unreadable.");
        // Strip the extension so the displayed name stays clean.
        const baseName = file.name.replace(/\.[^.]+$/, "");
        added.push({
          id:       Date.now() + Math.floor(Math.random() * 1000),
          name:     baseName,
          text:     extract.text.trim(),
          docx_b64: extract.docx_b64 ?? null,
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

// ─── Integrations tab ──────────────────────────────────────────────────────

interface BridgeInfo {
  ready: boolean;
  port?: number;
  token?: string;
  pairingCode?: string;
}

const IntegrationsTab = () => {
  const [info, setInfo] = useState<BridgeInfo | null>(null);
  const [copied, setCopied] = useState(false);

  const load = () => {
    invoke<BridgeInfo>("bridge_info")
      .then(setInfo)
      .catch(() => setInfo({ ready: false }));
  };
  useEffect(load, []);

  const copy = async () => {
    if (!info?.pairingCode) return;
    try {
      await navigator.clipboard.writeText(info.pairingCode);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard blocked — user can select the field manually */ }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <Card>
        <CardTitle>InterPrep Autofill (browser extension)</CardTitle>
        <CardDesc>
          Pair the browser extension with this app to auto-fill job applications
          from your tailored resumes and company research. Paste the code below
          into the extension popup, then click Pair.
        </CardDesc>

        {info?.ready ? (
          <>
            <FieldLabel>Pairing code</FieldLabel>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                readOnly
                value={info.pairingCode || ""}
                onFocus={(e) => e.currentTarget.select()}
                style={{ ...fieldInputStyle(), flex: 1, fontFamily: "monospace", fontSize: 11 }}
              />
              <button
                type="button"
                onClick={copy}
                style={{
                  display: "flex", alignItems: "center", gap: 5,
                  padding: "0 12px", borderRadius: 8, flexShrink: 0,
                  background: copied ? T.accent : T.surface2,
                  color: copied ? "#fff" : T.textSecondary,
                  border: `0.5px solid ${T.border}`, cursor: "pointer",
                  fontSize: 12, fontFamily: T.fontBody,
                }}
              >
                <Icon name={copied ? "check" : "copy"} size={13} />
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
            <CardHint>
              Listening on port {info.port}. The code changes every time you
              restart InterPrep — re-pair if the extension says it can't connect.
            </CardHint>
            <div style={{ height: 10 }} />
            <button
              type="button"
              onClick={load}
              style={{
                display: "flex", alignItems: "center", gap: 6,
                padding: "7px 12px", borderRadius: 8, alignSelf: "flex-start",
                background: T.surface2, color: T.textSecondary,
                border: `0.5px solid ${T.border}`, cursor: "pointer",
                fontSize: 12, fontFamily: T.fontBody,
              }}
            >
              <Icon name="refresh" size={12} /> Refresh
            </button>
          </>
        ) : (
          <CardHint>
            Backend isn't ready yet — wait a few seconds after launch, then click
            Refresh.
            <span style={{ display: "block", marginTop: 8 }}>
              <button
                type="button"
                onClick={load}
                style={{
                  display: "inline-flex", alignItems: "center", gap: 6,
                  padding: "7px 12px", borderRadius: 8,
                  background: T.surface2, color: T.textSecondary,
                  border: `0.5px solid ${T.border}`, cursor: "pointer",
                  fontSize: 12, fontFamily: T.fontBody,
                }}
              >
                <Icon name="refresh" size={12} /> Refresh
              </button>
            </span>
          </CardHint>
        )}
      </Card>

      <Card>
        <CardTitle>How to install the extension</CardTitle>
        <CardDesc>
          In Chrome/Edge open the extensions page, enable Developer mode, choose
          “Load unpacked”, and select the <code>extension/</code> folder from the
          InterPrep project. Then open the popup and paste the pairing code above.
        </CardDesc>
        <CardHint>
          The pairing code is a shared secret — anyone with it (and access to this
          machine) can read your saved resumes through the local bridge. Don't
          share it.
        </CardHint>
      </Card>
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
  /// Job awaiting a Mock Interview config before the thread is created.
  const [mockConfigForJob, setMockConfigForJob] = useState<Job | null>(null);
  /// Voice-mode UI state for the interview (spoken Q&A + barge-in).
  const [voiceEnabled, setVoiceEnabled] = useState(false);
  const [voiceListening, setVoiceListening] = useState(false);
  const [voiceStatus, setVoiceStatus] = useState<{ available: boolean; device?: string; detail?: string; vibe_available?: boolean; voices?: string[]; default_speaker?: string } | null>(null);
  const [voiceOverlayOpen, setVoiceOverlayOpen] = useState(false);
  const [voicePhase, setVoicePhase] = useState<string>("idle");
  /// Barge-in (talk over the AI). Off by default — only safe with headphones.
  const [bargeEnabled, setBargeEnabled] = useState(false);
  /// TTS engine: false = Piper (fast default), true = VibeVoice ("vibe-rt",
  /// humanlike). Persisted in localStorage; pushed to Rust on load + toggle.
  const [voxVoice, setVoxVoice] = useState(() => {
    try { return localStorage.getItem("interprep.voxVoice") === "1"; }
    catch { return false; }
  });
  /// Interviewer panel size (1–4). With VibeVoice on, each interviewer turn
  /// rotates through this many distinct preset voices, simulating a panel.
  const [panelSize, setPanelSize] = useState(() => {
    try { return Math.min(4, Math.max(1, parseInt(localStorage.getItem("interprep.panelSize") || "1", 10))); }
    catch { return 1; }
  });
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
  // sends are single-flighted (a new one while a stream runs is queued, not
  // started), so there's only ever one active stream.
  /// Active streams, keyed by a unique stream id that every SSE event carries.
  /// Concurrent streams stay fully separated: each tags its own AI bubble with
  /// the id and every `chat:*` event routes by it, so two streams — even in the
  /// same thread — never cross-contaminate. `tailored` caches the tailored
  /// resume for the Prep→Research chain fired on `done`.
  const streamsRef = useRef<Map<string, { jobId: string; chatId: string; tailored?: string }>>(new Map());
  /// Chat ids with ≥1 active stream — drives the per-thread loader. State (not
  /// just the ref) so the UI re-renders as streams start/stop.
  const [streamingChatIds, setStreamingChatIds] = useState<string[]>([]);
  /// In-progress composer text, kept PER chat (keyed by chat id; "" for a job
  /// with no thread selected yet) so switching chats doesn't clobber a draft.
  const [draftsByChat, setDraftsByChat] = useState<Record<string, string>>({});

  const newStreamId = () => `s-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  // Register a starting stream and mark its chat as streaming.
  const beginStream = (streamId: string, jobId: string, chatId: string) => {
    streamsRef.current.set(streamId, { jobId, chatId });
    setStreamingChatIds((ids) => (ids.includes(chatId) ? ids : [...ids, chatId]));
  };
  // Retire a finished/failed stream; clear its chat's loader only if no OTHER
  // stream is still feeding that same chat.
  const endStream = (streamId: string) => {
    const t = streamsRef.current.get(streamId);
    streamsRef.current.delete(streamId);
    if (!t) return;
    if (![...streamsRef.current.values()].some((s) => s.chatId === t.chatId)) {
      setStreamingChatIds((ids) => ids.filter((c) => c !== t.chatId));
    }
  };
  // Drop every stream targeting a chat (used when that chat is deleted).
  const endStreamsForChat = (chatId: string) => {
    for (const [id, t] of [...streamsRef.current.entries()]) {
      if (t.chatId === chatId) streamsRef.current.delete(id);
    }
    setStreamingChatIds((ids) => ids.filter((c) => c !== chatId));
  };
  /// Set to a freshly-created Mock Interview thread id; an effect fires the
  /// kickoff prompt once that thread is committed + selected, so onSendMessage
  /// reads fresh state (avoids the stale-closure trap of calling it inline).
  const pendingMockKickoffRef = useRef<string | null>(null);
  /// Voice mode for the live interview. Refs mirror state so the one-shot
  /// event listeners read the latest values.
  const voiceEnabledRef = useRef(false);
  /// `{ jobId, chatId }` of the interview the voice loop is driving.
  const voiceTargetRef = useRef<{ jobId: string; chatId: string } | null>(null);
  /// Always points at the latest `onSendMessage` closure (fresh selection).
  const sendMessageRef = useRef<((text: string, opts?: { hidden?: boolean; jobId?: string; chatId?: string }) => void) | null>(null);
  /// Live audio level/pitch for the voice orb (updated ~20×/s; a ref so the
  /// high-frequency events don't trigger React re-renders).
  const voiceLevelRef = useRef<VoiceLevel>({ level: 0, pitch: 0, mode: "idle" });
  /// Streaming-TTS bookkeeping for the current AI turn.
  const voiceSpeakBufferRef = useRef("");   // tokens not yet flushed as a sentence
  const voiceUtteranceTextRef = useRef(""); // full text this turn (marker detection)
  const voiceSkipRef = useRef(false);       // stop speaking this turn (feedback/barge)
  const voiceListenActiveRef = useRef(false); // mic already opened for this answer
  /// Panelist voice for the current interviewer turn (vibe-rt only). Set at
  /// turn start in onSendMessage; read by the chat:token TTS handler. Rotates
  /// across the active panel so successive questions sound like different people.
  const voiceSpeakerRef = useRef("");
  const voiceTurnRef = useRef(0); // turn counter driving the round-robin

  // Refs so listener callbacks can read latest credentials + jobs without
  // re-registering listeners on every state change.
  const credentialsRef = useRef<Credentials>(EMPTY_CREDS);
  const jobsRef        = useRef<Job[]>([]);
  const resumesRef     = useRef<Resume[]>([]);
  /// Populated once `startCompanyResearchFor` is defined further down. Lets
  /// the `chat:done` listener (registered in a one-shot useEffect) call into
  /// the latest closure.
  const startCompanyResearchForRef = useRef<((job: Job, tailoredResume: string) => void) | null>(null);
  /// Latest `startApplicationPrep` closure, so the inbox poller can launch the
  /// full Prep → Research chain with fresh `resumes`.
  const startApplicationPrepRef = useRef<((job: Job) => void) | null>(null);
  /// Guards the job-capture poller so two ticks can't process the same item.
  const captureBusyRef = useRef(false);

  // Poll the extension's job-capture inbox. When the user grabs a JD from the
  // browser, a capture lands here; we create the job + start company research so
  // they can watch it stream in the app. One capture per tick (captureBusyRef);
  // its stream runs concurrently with any others.
  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      if (stopped || captureBusyRef.current) return;
      let items: Array<Record<string, string>> = [];
      try { items = await invoke<Array<Record<string, string>>>("poll_job_inbox"); }
      catch { return; } // backend not ready / unreachable — try again next tick
      if (!items || !items.length) return;

      captureBusyRef.current = true;
      try {
        const item = items[0];
        const id = `job-${Date.now()}`;
        const palette = ["#6366F1", "#10B981", "#F59E0B", "#EC4899", "#8B5CF6"];
        const company = (item.company || "Unknown").trim() || "Unknown";
        // `requirements` is a string[] in the capture (the loose Record type
        // doesn't capture that), so read it through a cast.
        const reqs = (item as unknown as { requirements?: string[] }).requirements;
        const job: Job = {
          id,
          company,
          role: (item.role || "Role").trim() || "Role",
          location: item.location || "",
          url: item.url || "",
          status: "Applied",
          appliedDate: new Date().toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" }),
          currentStage: 0,
          stageNotes: {},
          avatar: company[0]?.toUpperCase() ?? "?",
          avatarColor: palette[jobsRef.current.length % palette.length],
          chats: [],
          jobDescription: item.job_description || undefined,
          // Preserve the extension's parsed display metadata instead of dropping
          // it when the inbox item is acked (and gone).
          employmentType: item.employment_type || undefined,
          workMode: item.work_mode || undefined,
          salary: item.salary || undefined,
          posted: item.posted || undefined,
          requirements: Array.isArray(reqs) && reqs.length ? reqs : undefined,
        };
        flushSync(() => { setJobs((prev) => [...prev, job]); });
        setSelectedJobId(id);
        // Same flow as creating a job in-app: run Application Prep (resume
        // tailoring) first — it auto-chains into Company Research on `chat:done`.
        // Fall back to research-only when there are no master resumes to tailor.
        if (resumesRef.current.length > 0 && startApplicationPrepRef.current) {
          startApplicationPrepRef.current(job);
        } else {
          setSelectedChatId(`c-research-${id}`);
          startCompanyResearchForRef.current?.(job, "");
        }
        await invoke("ack_job_inbox", { ids: [item.id] }).catch(() => {});
      } finally {
        captureBusyRef.current = false;
      }
    };
    const h = setInterval(tick, 4000);
    return () => { stopped = true; clearInterval(h); };
  }, []);

  // Poll the extension's timeline-event queue. After an autofill the extension
  // logs a "mark Applied + note" event here; we apply each to its job's stage
  // timeline (status + a note on the Applied stage), then ack it. These never
  // start a stream, so — unlike the capture poller — this runs anytime.
  useEffect(() => {
    let stopped = false;
    const STAGE_APPLIED = 0; // STAGES[0] === "Applied"
    const apply = (job: Job, ev: { status?: string; note?: string }): Job => {
      const status: JobStatusKey =
        ev.status && ev.status in STATUS_CONFIG ? (ev.status as JobStatusKey) : job.status;
      const prev = job.stageNotes[STAGE_APPLIED];
      const note = (ev.note || "").trim();
      const merged: StageNote = {
        date: prev?.date || new Date().toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" }),
        outcome: prev?.outcome || status,
        notes: [prev?.notes, note].filter(Boolean).join("\n"),
      };
      return { ...job, status, stageNotes: { ...job.stageNotes, [STAGE_APPLIED]: merged } };
    };
    const tick = async () => {
      if (stopped) return;
      let events: Array<{ id: string; job_id: string; status?: string; note?: string }> = [];
      try { events = await invoke("poll_timeline_inbox"); }
      catch { return; } // backend not ready / unreachable — retry next tick
      if (!events || !events.length) return;
      const byJob = new Map<string, typeof events>();
      for (const ev of events) {
        const list = byJob.get(ev.job_id) ?? [];
        list.push(ev);
        byJob.set(ev.job_id, list);
      }
      setJobs((prev) => prev.map((j) => {
        const evs = byJob.get(j.id);
        return evs ? evs.reduce(apply, j) : j;
      }));
      // Ack everything we polled (events for unknown jobs are dropped rather
      // than re-polled forever).
      await invoke("ack_timeline_inbox", { ids: events.map((e) => e.id) }).catch(() => {});
    };
    const h = setInterval(tick, 4000);
    return () => { stopped = true; clearInterval(h); };
  }, []);

  useEffect(() => {
    // StrictMode-safe listener registration. React's dev StrictMode runs
    // effects twice (mount → cleanup → mount). With `listen().then(u =>
    // unsubs.push(u))`, the first cleanup fires BEFORE the async listen()
    // resolves, so its `u` never lands in `unsubs` and the first set of
    // listeners stays registered forever. Two live `chat:done` listeners
    // then double-handle every completion (chaining Prep→Research twice,
    // retiring a stream that another listener already removed, etc.).
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

    // Mutate the AI bubble a specific stream is filling — matched by the
    // message's `streamId`, so concurrent streams in one thread never collide.
    const appendToStream = (streamId: string, mutate: (m: ChatMsg) => ChatMsg) => {
      const target = streamsRef.current.get(streamId);
      if (!target) return;
      updateJob(target.jobId, (j) => ({
        ...j,
        chats: j.chats.map((c) =>
          c.id !== target.chatId
            ? c
            : { ...c, messages: c.messages.map((m) => (m.streamId === streamId ? mutate(m) : m)) },
        ),
      }));
    };

    register<{ streamId: string; content: string }>("chat:token", (e) => {
      const { streamId, content } = e.payload;
      appendToStream(streamId, (m) => ({ ...m, content: m.content + content }));

      // Streaming TTS: as the interviewer's reply streams, speak complete
      // sentences immediately instead of waiting for the whole message.
      if (!voiceEnabledRef.current) return;
      const target = streamsRef.current.get(streamId);
      if (!target) return;
      const job = jobsRef.current.find((j) => j.id === target.jobId);
      const thread = job?.chats.find((c) => c.id === target.chatId);
      if (!thread || !(thread.mode === "interviewer" || thread.title === "Mock Interview")) return;
      voiceTargetRef.current = { jobId: target.jobId, chatId: target.chatId };

      voiceUtteranceTextRef.current += content;
      if (voiceSkipRef.current) return;
      // Don't speak the end-of-interview feedback block.
      if (voiceUtteranceTextRef.current.includes(INTERVIEW_DONE_MARKER)) {
        voiceSkipRef.current = true;
        voiceSpeakBufferRef.current = "";
        return;
      }
      voiceSpeakBufferRef.current += content;
      // Flush each complete sentence (punctuation + trailing space/newline).
      const buf = voiceSpeakBufferRef.current;
      const re = /[^.!?\n]*[.!?\n]+(?:\s|$)/g;
      let lastIdx = 0;
      let m: RegExpExecArray | null;
      const sentences: string[] = [];
      while ((m = re.exec(buf)) !== null) {
        const s = m[0].trim();
        if (s) sentences.push(s);
        lastIdx = re.lastIndex;
      }
      if (sentences.length) {
        voiceSpeakBufferRef.current = buf.slice(lastIdx);
        setVoicePhase("speaking");
        for (const s of sentences) invoke("voice_speak_chunk", { text: s, speaker: voiceSpeakerRef.current }).catch(() => {});
      }
    });

    register<{ streamId: string; content: string }>("chat:log", (e) => {
      appendToStream(e.payload.streamId, (m) => ({ ...m, logs: [...(m.logs ?? []), e.payload.content] }));
    });

    register<{ streamId: string; content: Scorecard }>("chat:scorecard", (e) => {
      const target = streamsRef.current.get(e.payload.streamId);
      if (!target) return;
      updateJob(target.jobId, (j) => ({ ...j, scorecard: e.payload.content }));
    });

    register<{ streamId: string; content: string }>("chat:resume_docx", async (e) => {
      const target = streamsRef.current.get(e.payload.streamId);
      if (!target) return;
      try {
        const path = await invoke<string>("save_resume_docx", {
          jobId: target.jobId,
          b64:   e.payload.content,
        });
        updateJob(target.jobId, (j) => ({ ...j, resumeDocxPath: path }));
      } catch (err) {
        console.error("save_resume_docx failed:", err);
      }
    });

    register<{ streamId: string; content: string }>("chat:tailored_resume", (e) => {
      const target = streamsRef.current.get(e.payload.streamId);
      if (!target) return;
      target.tailored = e.payload.content; // consumed by the Prep→Research chain on done
      updateJob(target.jobId, (j) => ({ ...j, tailoredResume: e.payload.content }));
    });

    // ── Voice loop ─────────────────────────────────────────────────────────
    register<VoiceLevel>("voice:level", (e) => {
      // High-frequency: write to a ref (the orb reads it via rAF), no re-render.
      voiceLevelRef.current = e.payload;
    });
    register<null>("voice:listening", () => {
      setVoiceListening(true);
      setVoicePhase("listening");
    });
    register<{ interrupted: boolean }>("voice:speak_done", () => {
      // Interviewer finished → open the mic, unless voice is off, the mic is
      // already open (barge-in), or the interview just ended (skip).
      if (!voiceEnabledRef.current || !voiceTargetRef.current || voiceSkipRef.current) return;
      if (voiceListenActiveRef.current) return;
      voiceListenActiveRef.current = true;
      invoke("voice_listen").catch((err) => console.error("voice_listen failed:", err));
    });
    register<null>("voice:barge_in", () => {
      // User cut in: stop speaking the rest of this turn and open the mic now.
      voiceSkipRef.current = true;
      if (voiceListenActiveRef.current) return;
      voiceListenActiveRef.current = true;
      setVoicePhase("listening");
      invoke("voice_listen").catch(() => {});
    });
    register<string>("voice:transcript", (e) => {
      voiceListenActiveRef.current = false;
      setVoiceListening(false);
      setVoicePhase("thinking");
      const text = (e.payload || "").trim();
      if (!text || !voiceTargetRef.current) return;
      // Send the transcribed answer; the next question will auto-speak on done.
      sendMessageRef.current?.(text);
    });
    register<string>("voice:error", (e) => {
      setVoiceListening(false);
      console.error("voice error:", e.payload);
    });

    register<{ streamId: string }>("chat:done", (e) => {
      const streamId = e.payload.streamId;
      const target = streamsRef.current.get(streamId);
      appendToStream(streamId, (m) => ({ ...m, streaming: false, streamId: undefined }));
      const tailored = target?.tailored ?? "";
      endStream(streamId);

      // Resolve the just-finished thread from `target` (state may lag).
      if (!target) return;
      const job = jobsRef.current.find((j) => j.id === target.jobId);
      if (!job) return;
      const thread = job.chats.find((c) => c.id === target.chatId);
      if (!thread) return;

      // Mock interview finished with feedback → persist the verdict on the job.
      const isInterview = thread.mode === "interviewer" || thread.title === "Mock Interview";
      if (isInterview) {
        const last = thread.messages[thread.messages.length - 1];
        const content = last?.content ?? "";
        if (content.includes(INTERVIEW_DONE_MARKER)) {
          const m = content.match(/Predicted outcome:?\**\s*(Pass|Borderline|Fail)/i);
          const outcome = m?.[1] ?? "Completed";
          const date = new Date().toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
          setJobs((prev) => prev.map((j) =>
            j.id !== job.id ? j : { ...j, lastInterview: { outcome, date, chatId: thread.id } },
          ));
          return; // interview over — don't speak/listen further
        }
        // Voice mode: flush the trailing partial sentence, then end the
        // utterance (worker emits voice:speak_done → opens the mic). Sentences
        // were already streamed to TTS in the chat:token handler.
        if (voiceEnabledRef.current && content.trim() && !voiceSkipRef.current) {
          voiceTargetRef.current = { jobId: job.id, chatId: thread.id };
          setVoicePhase("speaking");
          const tail = voiceSpeakBufferRef.current.trim();
          voiceSpeakBufferRef.current = "";
          if (tail) invoke("voice_speak_chunk", { text: tail, speaker: voiceSpeakerRef.current }).catch(() => {});
          invoke("voice_speak_flush").catch(() => {});
        }
        return;
      }

      // Chain: Application Prep → Company Research, using the tailored resume
      // as candidate context.
      if (thread.title !== "Application Prep") return;
      startCompanyResearchForRef.current?.(job, tailored || job.tailoredResume || "");
    });

    register<{ streamId: string; content: string }>("chat:error", (e) => {
      const { streamId, content } = e.payload;
      appendToStream(streamId, (m) => ({
        ...m,
        streaming: false,
        streamId: undefined,
        content: m.content || `**Error:** ${content}`,
      }));
      endStream(streamId);
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

  // Persist the voice-engine choice and push it to the Rust voice state. The
  // command only mutates in-process state (no sidecar round-trip), so it's safe
  // to call on mount before the backend is ready. Runs on mount + every toggle.
  useEffect(() => {
    try { localStorage.setItem("interprep.voxVoice", voxVoice ? "1" : "0"); } catch { /* ignore */ }
    invoke("voice_set_engine", { engine: voxVoice ? "vibe-rt" : "piper" }).catch(() => {});
    // VibeVoice has a brutal cold start (~2.5 min: model load + first-synth JIT
    // on a laptop GPU). Kick the warmup the moment the user enables it so that
    // cost lands here, not on the first interview question. Idempotent server-side.
    if (voxVoice) invoke("voice_warm", { engine: "vibe-rt" }).catch(() => {});
  }, [voxVoice]);

  // Persist the panel size so it survives restarts.
  useEffect(() => {
    try { localStorage.setItem("interprep.panelSize", String(panelSize)); } catch { /* ignore */ }
  }, [panelSize]);

  // Fetch voice capability once the sidecar is up so the Settings → Voice tab
  // knows whether VibeVoice is actually installed BEFORE the user toggles it.
  // (Without this, voiceStatus is null until the user first enters voice mode,
  // so the toggle is enabled and the "not installed" hint hidden.) `voice_status`
  // only checks for the deps — it doesn't load any model — so it's cheap.
  // Best-effort: retry until the sidecar answers.
  useEffect(() => {
    let cancelled = false;
    let tries = 0;
    const fetchStatus = () => {
      invoke<{ available: boolean; device?: string; detail?: string; vibe_available?: boolean; voices?: string[]; default_speaker?: string }>("voice_status")
        .then((s) => { if (!cancelled) setVoiceStatus(s); })
        .catch(() => { if (!cancelled && tries++ < 15) setTimeout(fetchStatus, 2000); });
    };
    fetchStatus();
    return () => { cancelled = true; };
  }, []);

  // Keep refs in sync so the one-shot SSE listeners read latest state.
  useEffect(() => { credentialsRef.current = credentials; }, [credentials]);
  useEffect(() => { jobsRef.current        = jobs; },        [jobs]);

  // ── Resume library ─────────────────────────────────────────────────────
  // Load on mount, save on every mutation (cheap; the file is small).
  const [resumes, setResumes] = useState<Resume[]>([]);
  const [resumesLoaded, setResumesLoaded] = useState(false);
  useEffect(() => { resumesRef.current = resumes; }, [resumes]);

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

  const onDeleteChat = (jobId: string, chatId: string) => {
    setJobs((prev) => prev.map((j) =>
      j.id !== jobId ? j : { ...j, chats: j.chats.filter((c) => c.id !== chatId) },
    ));
    // If the open thread was deleted, drop the selection so the empty state shows.
    if (selectedChatId === chatId) setSelectedChatId(null);
    // Drop any streams feeding it — their incoming events have nowhere to land.
    endStreamsForChat(chatId);
  };

  const onUpdateJob = (id: string, updater: (j: Job) => Job) => {
    setJobs((prev) => prev.map((j) => j.id === id ? updater(j) : j));
  };

  const onSendMessage = (text: string, opts?: { hidden?: boolean; jobId?: string; chatId?: string }) => {
    // A queued send carries its own target so it lands in the thread it was
    // composed in; a normal send targets the current selection.
    const sjid = opts?.jobId ?? selectedJobId;
    if (!sjid) return;
    // New turn → reset streaming-TTS buffers for the upcoming AI reply.
    voiceSpeakBufferRef.current = "";
    voiceUtteranceTextRef.current = "";
    voiceSkipRef.current = false;
    // Pick the panelist voice for the upcoming interviewer reply. With VibeVoice
    // on and a panel of 2+, round-robin across the first `panelSize` presets so
    // each question sounds like a different interviewer. Otherwise use the
    // single default voice (empty string → backend default).
    {
      const voices = voiceStatus?.voices ?? [];
      const n = Math.max(1, Math.min(panelSize, voices.length || 1));
      voiceSpeakerRef.current =
        voxVoice && n > 1 && voices.length
          ? voices[voiceTurnRef.current % n]
          : (voxVoice ? (voiceStatus?.default_speaker ?? "") : "");
      voiceTurnRef.current += 1;
    }
    let chatId = opts?.chatId ?? selectedChatId;

    if (!chatId) {
      chatId = `c-${Date.now()}`;
      const newChat: ChatThread = { id: chatId, title: text.slice(0, 30) + "...", preview: text, messages: [] };
      setJobs((prev) => prev.map((j) => j.id === sjid ? { ...j, chats: [...j.chats, newChat] } : j));
      setSelectedChatId(chatId);
    }
    const targetChatId = chatId;

    // Append the user's message AND an empty AI bubble that the SSE stream
    // will fill in tokens-first. Keeping them in one setJobs call avoids a
    // wasted re-render between the two appends.
    const streamId = newStreamId();
    const userMsg: ChatMsg = { role: "user", content: text, ...(opts?.hidden ? { hidden: true } : {}) };
    const aiPlaceholder: ChatMsg = { role: "ai", content: "", streaming: true, logs: [], streamId };
    setJobs((prev) => prev.map((j) =>
      j.id !== sjid
        ? j
        : {
            ...j,
            chats: j.chats.map((c) =>
              c.id !== targetChatId
                ? c
                : { ...c, messages: [...c.messages, userMsg, aiPlaceholder] },
            ),
          },
    ));

    // Context for the backend system prompt. `job` is the pre-append snapshot
    // (history below must exclude the message we just added).
    const job = jobs.find((j) => j.id === sjid);
    const targetThread = job?.chats.find((c) => c.id === targetChatId);
    const isInterview =
      (targetThread?.mode ?? (targetThread?.title === "Mock Interview" ? "interviewer" : "coach"))
        === "interviewer";

    // Prefer the tailored resume the candidate actually submitted; fall back
    // to the most recent master resume. Trimmed so the prompt stays bounded.
    const masterResume = resumes.length > 0 ? resumes[resumes.length - 1] : null;
    const resumeText = (job?.tailoredResume?.trim() || masterResume?.text?.trim() || "");
    const resumeName = job?.tailoredResume?.trim() ? "Tailored Resume" : (masterResume?.name ?? "Resume");

    const jobContext = job
      ? [
          `Company: ${job.company}`,
          `Role: ${job.role}`,
          job.location ? `Location: ${job.location}` : "",
          job.jobDescription ? `\nJob Description:\n${job.jobDescription.slice(0, 1500)}` : "",
          resumeText ? `\nCandidate Resume (${resumeName}):\n${resumeText.slice(0, 4000)}` : "",
          isInterview && targetThread?.interviewConfig
            ? `\n${formatInterviewSetup(targetThread.interviewConfig)}`
            : "",
        ].filter(Boolean).join("\n")
      : "";

    // RAG corpus. For interviewer mode, restrict to resume + company-research
    // dossier — sibling chats (esp. prep where the user rehearsed answers)
    // would leak into a "live" interview and break realism.
    const ragDocs: { source: string; text: string }[] = [];
    if (job) {
      if (resumeText) ragDocs.push({ source: "resume", text: resumeText });
      for (const c of job.chats) {
        if (c.id === targetChatId) continue;
        const isResearch = c.title === "Company Research";
        if (isInterview && !isResearch) continue;
        const body = c.messages
          .filter((m) => !m.streaming && m.content.trim().length > 0)
          .map((m) => `${m.role === "user" ? "Candidate" : "Assistant"}: ${m.content}`)
          .join("\n\n");
        if (!body) continue;
        ragDocs.push({ source: isResearch ? "company_research" : `chat: ${c.title}`, text: body });
      }
    }

    beginStream(streamId, sjid, targetChatId);

    // Chronological history from completed turns BEFORE the message above.
    const history: [string, string][] = targetThread
      ? targetThread.messages
          .filter((m) => !m.streaming && m.content.trim().length > 0)
          .map((m) => [m.role === "user" ? "user" : "assistant", m.content] as [string, string])
      : [];

    const mode = isInterview ? "interviewer" : "coach";

    invoke("start_chat_stream", {
      message: text,
      jobContext,
      history,
      mode,
      llm: llmPayload(credentials),
      documents: ragDocs,
      streamId,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== sjid
          ? j
          : {
              ...j,
              chats: j.chats.map((c) =>
                c.id !== targetChatId
                  ? c
                  : {
                      ...c,
                      messages: c.messages.map((m) =>
                        m.streamId === streamId
                          ? { ...m, streaming: false, streamId: undefined, content: `**Error:** ${errMsg}` }
                          : m,
                      ),
                    },
              ),
            },
      ));
      endStream(streamId);
    });
  };

  // Fire the Mock Interview kickoff once the new thread is committed and
  // selected. Reads fresh state on this render, so onSendMessage routes to the
  // right thread and detects interviewer mode from its title.
  useEffect(() => {
    const id = pendingMockKickoffRef.current;
    if (id && selectedChatId === id && jobs.some((j) => j.chats.some((c) => c.id === id))) {
      pendingMockKickoffRef.current = null;
      // Hidden: it's just the trigger for the interviewer's first question,
      // not something the candidate should see in the transcript.
      onSendMessage(
        "Begin the interview now. Ask your first question.",
        { hidden: true },
      );
    }
  }, [selectedChatId, jobs]);

  /// Regenerate the newest reply: drop the trailing AI message + the user
  /// prompt that produced it, then re-send that prompt through onSendMessage
  /// (which rebuilds history/RAG and re-streams). flushSync commits the
  /// truncation first so onSendMessage reads the trimmed thread, not stale
  /// state that would duplicate the turn.
  const onRegenerate = () => {
    if (!selectedJobId || !selectedChatId || streamingChatIds.includes(selectedChatId)) return;
    const job = jobs.find((j) => j.id === selectedJobId);
    const chat = job?.chats.find((c) => c.id === selectedChatId);
    if (!chat) return;
    let lastUserIdx = -1;
    for (let k = chat.messages.length - 1; k >= 0; k--) {
      if (chat.messages[k].role === "user") { lastUserIdx = k; break; }
    }
    if (lastUserIdx === -1) return;
    const userText = chat.messages[lastUserIdx].content;
    flushSync(() => {
      setJobs((prev) => prev.map((j) =>
        j.id !== selectedJobId ? j : {
          ...j,
          chats: j.chats.map((c) =>
            c.id !== selectedChatId ? c : { ...c, messages: c.messages.slice(0, lastUserIdx) },
          ),
        },
      ));
    });
    onSendMessage(userText);
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
    const streamId = newStreamId();
    const placeholderThread: ChatThread = {
      id: researchThreadId,
      title: "Company Research",
      messages: [{
        role: "ai",
        content: "",
        streaming: true,
        streamId,
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
      beginStream(streamId, job.id, researchThreadId);
    });

    // Don't auto-switch view — user is reading the cover letter + scorecard
    // on the Prep thread; jerking them to Research is jarring. Research is
    // available via the sidebar once it lands.

    invoke("start_company_research_stream", {
      company:        job.company,
      role:           job.role,
      location:       job.location,
      jobDescription: job.jobDescription ?? "",
      tailoredResume,
      llm:            llmPayload(creds),
      streamId,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== researchThreadId ? c : {
            ...c,
            messages: c.messages.map((m) =>
              m.streamId === streamId
                ? { ...m, streaming: false, streamId: undefined, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      endStream(streamId);
    });
  };

  // Publish to ref so the chat:done listener (registered once, outside this
  // closure) can invoke the latest version on completion of Prep.
  startCompanyResearchForRef.current = startCompanyResearchFor;
  // Keep voice refs current for the one-shot voice listeners.
  voiceEnabledRef.current = voiceEnabled;
  sendMessageRef.current = onSendMessage;

  /// Enable voice mode (spoken Q&A). Checks the sidecar voice status so we can
  /// show GPU/CPU and bail with guidance if the stack isn't installed.
  const enableVoice = () => {
    setVoiceEnabled(true);
    voiceEnabledRef.current = true;
    invoke<{ available: boolean; device?: string; detail?: string; vibe_available?: boolean; voices?: string[]; default_speaker?: string }>("voice_status")
      .then((s) => {
        setVoiceStatus(s);
        if (!s.available) {
          setVoiceEnabled(false);
          voiceEnabledRef.current = false;
          setVoiceOverlayOpen(false);
          alert(`Voice unavailable: ${s.detail ?? "voice stack not installed"}\n\nRun: backend\\setup.ps1 -Voice`);
        } else if (voxVoice) {
          // Warm VibeVoice now (idempotent) so its cold start doesn't stall the
          // first question if the user jumped straight into the interview.
          invoke("voice_warm", { engine: "vibe-rt", speaker: s.default_speaker ?? "" }).catch(() => {});
        }
      })
      .catch((e) => {
        setVoiceStatus({ available: false, detail: String(e) });
        setVoiceEnabled(false);
        voiceEnabledRef.current = false;
        setVoiceOverlayOpen(false);
      });
  };

  /// Turn voice off entirely: stop playback + capture, close the overlay.
  const disableVoice = () => {
    setVoiceEnabled(false);
    voiceEnabledRef.current = false;
    setVoiceListening(false);
    setVoiceOverlayOpen(false);
    setVoicePhase("idle");
    invoke("voice_interrupt").catch(() => {});
    invoke("voice_stop_listening").catch(() => {});
  };

  /// Mic button: open the orb overlay, enabling voice if it isn't already.
  const onOpenVoice = () => {
    if (!voiceEnabled) enableVoice();
    setVoiceOverlayOpen(true);
  };

  const onToggleBarge = () => {
    const next = !bargeEnabled;
    setBargeEnabled(next);
    invoke("voice_set_barge", { enabled: next }).catch(() => {});
  };

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
    const streamId = newStreamId();
    setJobs((prev) => prev.map((j) =>
      j.id !== job.id ? j : {
        ...j,
        chats: [...j.chats, {
          id: koThreadId,
          title: "Knockout Screen",
          messages: [{ role: "ai", content: "", streaming: true, logs: [], streamId }],
        }],
      },
    ));
    setSelectedChatId(koThreadId);

    beginStream(streamId, job.id, koThreadId);
    invoke("start_knockout_screen_stream", {
      company:        job.company,
      role:           job.role,
      location:       job.location,
      jobDescription: job.jobDescription ?? "",
      tailoredResume: resume,
      llm:            llmPayload(credentialsRef.current),
      streamId,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== koThreadId ? c : {
            ...c,
            messages: c.messages.map((m) =>
              m.streamId === streamId
                ? { ...m, streaming: false, streamId: undefined, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      endStream(streamId);
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
    const streamId = newStreamId();
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
      // No auto General Prep chat — users create extra chats themselves via
      // the step buttons or by typing. Application Prep still auto-runs: its
      // SSE stream fills the placeholder AI message; on `done`, the listener
      // chains into a Company Research thread automatically.
      chats: [
        {
          id: prepThreadId,
          title: "Application Prep",
          messages: [{ role: "ai", content: "", streaming: true, logs: [], streamId }],
        },
      ],
      jobDescription: jd || undefined,
    };
    setJobs((prev) => [...prev, newJob]);
    setSelectedJobId(id);
    setSelectedChatId(prepThreadId);
    setShowNewJobModal(false);

    // Ship every master resume to the backend — the LLM aggregates evidence
    // across all of them and picks one .docx-source resume as the styling base
    // for the tailored output.
    const masterResumes = resumes.map((r) => ({
      name:     r.name,
      text:     r.text,
      docx_b64: r.docx_b64 ?? null,
    }));

    beginStream(streamId, id, prepThreadId);
    invoke("start_application_tailor_stream", {
      company:        newJob.company,
      role:           newJob.role,
      location:       newJob.location,
      jobDescription: jd,
      masterResumes,
      llm:            llmPayload(credentials),
      streamId,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== prepThreadId ? c : {
            ...c,
            messages: c.messages.map((m) =>
              m.streamId === streamId
                ? { ...m, streaming: false, streamId: undefined, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      endStream(streamId);
    });
  };

  /// Step launcher: (re-)run Application Prep for a job. Creates a fresh
  /// "Application Prep" thread and streams the tailored resume + cover letter
  /// + scorecard. The `chat:done` listener chains into Company Research, same
  /// as the auto-run on job create. flushSync so the SSE listener can resolve
  /// the thread by id before the first event lands.
  const startApplicationPrep = (job: Job) => {
    if (resumes.length === 0) {
      alert("Add at least one master resume in Settings → Resume before running Application Prep.");
      return;
    }
    const prepThreadId = `c-prep-${job.id}-${Date.now()}`;
    const streamId = newStreamId();
    flushSync(() => {
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: [...j.chats, {
            id: prepThreadId,
            title: "Application Prep",
            messages: [{ role: "ai", content: "", streaming: true, logs: [], streamId }],
          }],
        },
      ));
      setSelectedJobId(job.id);
      setSelectedChatId(prepThreadId);
      beginStream(streamId, job.id, prepThreadId);
    });

    const masterResumes = resumes.map((r) => ({
      name:     r.name,
      text:     r.text,
      docx_b64: r.docx_b64 ?? null,
    }));
    invoke("start_application_tailor_stream", {
      company:        job.company,
      role:           job.role,
      location:       job.location,
      jobDescription: job.jobDescription ?? "",
      masterResumes,
      llm:            llmPayload(credentialsRef.current),
      streamId,
    }).catch((e) => {
      const errMsg = String(e);
      setJobs((prev) => prev.map((j) =>
        j.id !== job.id ? j : {
          ...j,
          chats: j.chats.map((c) => c.id !== prepThreadId ? c : {
            ...c,
            messages: c.messages.map((m) =>
              m.streamId === streamId
                ? { ...m, streaming: false, streamId: undefined, content: `**Error:** ${errMsg}` }
                : m,
            ),
          }),
        },
      ));
      endStream(streamId);
    });
  };
  startApplicationPrepRef.current = startApplicationPrep;

  /// Step launcher: start a Mock Interview. Creates an empty "Mock Interview"
  /// thread and selects it; the interviewer persona kicks in on the first
  /// message (sendMessage maps that title to `mode: "interviewer"`).
  const startMockInterview = (job: Job, config: InterviewConfig) => {
    const mockThreadId = `c-mock-${job.id}-${Date.now()}`;
    setJobs((prev) => prev.map((j) =>
      j.id !== job.id ? j : {
        ...j,
        chats: [...j.chats, {
          id: mockThreadId, title: "Mock Interview", messages: [],
          mode: "interviewer", interviewConfig: config,
        }],
      },
    ));
    setSelectedJobId(job.id);
    setSelectedChatId(mockThreadId);
    // Auto-ask the first question. Deferred to an effect (see above) so it
    // runs after the new thread is committed + selected — otherwise
    // onSendMessage would read stale state and mis-route / default to coach.
    pendingMockKickoffRef.current = mockThreadId;
  };

  /// End the active mock interview and request structured feedback. Stops any
  /// in-flight voice (the question being spoken / the mic) and suppresses
  /// speaking the feedback block.
  const onEndInterview = () => {
    invoke("voice_interrupt").catch(() => {});       // stop current TTS + drain queue
    invoke("voice_stop_listening").catch(() => {});  // close the mic if open
    voiceListenActiveRef.current = false;
    setVoiceListening(false);
    onSendMessage("That's the end. Please end the interview now and give me my full structured feedback.");
    // onSendMessage reset voiceSkipRef; re-set so the feedback isn't read aloud
    // and the drained-queue speak_done doesn't re-open the mic.
    voiceSkipRef.current = true;
    setVoicePhase("idle");
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
        onDeleteChat={onDeleteChat}
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
              onStartApplicationPrep={startApplicationPrep}
              onStartMockInterview={(job) => setMockConfigForJob(job)}
            />
            {selectedJob ? (
              <>
                <ChatArea
                  chat={selectedChat}
                  job={selectedJob}
                  onSendMessage={onSendMessage}
                  streamingChatIds={streamingChatIds}
                  onOpenResumeDocx={(path) => {
                    invoke("open_path", { path }).catch((e) =>
                      console.error("open_path failed:", e),
                    );
                  }}
                  onSimulateKnockout={() => {
                    if (selectedJob) startKnockoutScreen(selectedJob);
                  }}
                  onRegenerate={onRegenerate}
                  onEndInterview={onEndInterview}
                  voiceEnabled={voiceEnabled}
                  voiceListening={voiceListening}
                  voiceStatus={voiceStatus}
                  onOpenVoice={onOpenVoice}
                />
                <InputComposer
                  value={draftsByChat[selectedChatId ?? ""] ?? ""}
                  onChange={(v) => setDraftsByChat((d) => ({ ...d, [selectedChatId ?? ""]: v }))}
                  onSend={(t) => { onSendMessage(t); setDraftsByChat((d) => ({ ...d, [selectedChatId ?? ""]: "" })); }}
                  disabled={false}
                  hint={(() => {
                    // Each send streams concurrently, so you can keep chatting
                    // here or in any other thread while a reply generates.
                    const key = selectedChatId ?? "";
                    if (streamingChatIds.includes(key)) return "Generating a reply… you can send again or switch chats.";
                    if (streamingChatIds.length > 0) return `${streamingChatIds.length} other chat${streamingChatIds.length > 1 ? "s" : ""} generating — send here anytime.`;
                    return undefined;
                  })()}
                />
              </>
            ) : (
              <EmptyState onNewJob={() => setShowNewJobModal(true)} />
            )}
          </>
        )}
      </div>

      {showNewJobModal && <NewJobModal onClose={() => setShowNewJobModal(false)} onSubmit={onCreateJob} />}
      {mockConfigForJob && (
        <MockInterviewModal
          job={mockConfigForJob}
          onClose={() => setMockConfigForJob(null)}
          onStart={(config) => { const j = mockConfigForJob; setMockConfigForJob(null); startMockInterview(j, config); }}
        />
      )}
      {voiceOverlayOpen && (
        <VoiceOverlay
          levelRef={voiceLevelRef}
          phase={voicePhase}
          subtitle={selectedJob ? `${selectedJob.role} · ${selectedJob.company}` : undefined}
          bargeEnabled={bargeEnabled}
          onToggleBarge={onToggleBarge}
          onMinimize={() => setVoiceOverlayOpen(false)}
          onStop={disableVoice}
          onEnd={() => { onEndInterview(); setVoiceOverlayOpen(false); }}
        />
      )}
      {showSettings && (
        <SettingsModal
          credentials={credentials}
          onCredentialsChange={setCredentials}
          resumes={resumes}
          onResumesChange={setResumes}
          voxVoice={voxVoice}
          onVoxVoiceChange={setVoxVoice}
          panelSize={panelSize}
          onPanelSizeChange={setPanelSize}
          voiceStatus={voiceStatus}
          onClose={() => {
            setShowSettings(false);
            // Flush credentials to the OS keychain. Don't block close on it.
            if (credentialsLoaded) {
              invoke("save_credentials", { credentials })
                // Push the (possibly changed) provider config to the running
                // sidecar so the browser-extension autofill keeps working
                // without an app restart. Best-effort — the backend may not
                // be up yet.
                .then(() => invoke("reseed_backend_key").catch(() => {}))
                .catch((e) => console.error("save_credentials failed:", e));
            }
          }}
        />
      )}
    </div>
  );
};

export default App;
