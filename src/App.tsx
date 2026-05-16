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
  | "interview" | "panel" | "archive";

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

// ─── Sidebar ───────────────────────────────────────────────────────────────

interface SidebarProps {
  jobs: Job[];
  selectedJobId: string | null;
  selectedChatId: string | null;
  onSelectJob: (id: string) => void;
  onSelectChat: (id: string | null) => void;
  onNewJob: () => void;
  onSettings: () => void;
  collapsed: boolean;
  activeScreen: Screen;
  onSetScreen: (s: Screen) => void;
}

const Sidebar = (p: SidebarProps) => {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({ job1: true, job2: false });
  const [hovered, setHovered] = useState<string | null>(null);

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
            {p.jobs.map((job) => (
              <div key={job.id} style={{ marginBottom: 1 }}>
                <div
                  onClick={() => { toggle(job.id); p.onSelectJob(job.id); }}
                  onMouseEnter={() => setHovered(`f-${job.id}`)}
                  onMouseLeave={() => setHovered(null)}
                  style={{
                    display: "flex", alignItems: "center", gap: 7,
                    padding: "7px 10px", borderRadius: 8, cursor: "pointer",
                    background:
                      p.selectedJobId === job.id && !expanded[job.id]
                        ? T.surface
                        : hovered === `f-${job.id}` ? T.surfaceHover : "transparent",
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
                  <span style={{
                    fontSize: 10, fontWeight: 500,
                    padding: "2px 7px", borderRadius: 100,
                    background: STATUS_CONFIG[job.status].bg,
                    color: STATUS_CONFIG[job.status].color,
                    flexShrink: 0, letterSpacing: "-0.1px",
                  }}>{job.status}</span>
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

interface WorkspaceHeaderProps {
  job: Job | null;
  onToggleSidebar: () => void;
}

const WorkspaceHeader = ({ job, onToggleSidebar }: WorkspaceHeaderProps) => {
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

// ─── Chat area ────────────────────────────────────────────────────────────

interface ChatAreaProps {
  chat: ChatThread | null;
  onSendMessage: (text: string) => void;
  isLoading: boolean;
}

const ChatArea = ({ chat, onSendMessage, isLoading }: ChatAreaProps) => {
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
              <div>
                <div style={{
                  padding: "12px 16px",
                  borderRadius: "6px 20px 20px 20px",
                  background: T.surface, boxShadow: T.shadowMd,
                }}>
                  <MarkdownText content={msg.content} />
                </div>
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
      {isLoading && (
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
  notes: string;
}

interface NewJobModalProps {
  onClose: () => void;
  onSubmit: (form: NewJobFormState) => void;
}

const NewJobModal = ({ onClose, onSubmit }: NewJobModalProps) => {
  const [form, setForm] = useState<NewJobFormState>({
    company: "", role: "", url: "", location: "", status: "Applied", notes: "",
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

// ─── Settings modal (slimmed initial port) ────────────────────────────────

interface SettingsModalProps { onClose: () => void; }

const SettingsModal = ({ onClose }: SettingsModalProps) => {
  const [section, setSection] = useState("account");
  const sections = [
    { id: "account",       label: "Account",        icon: "user"      as IconName },
    { id: "appearance",    label: "Appearance",     icon: "sun"       as IconName },
    { id: "resume",        label: "Resume",         icon: "upload"    as IconName },
    { id: "notifications", label: "Notifications",  icon: "bell"      as IconName },
    { id: "data",          label: "Data & Privacy", icon: "briefcase" as IconName },
  ];

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
          width: 660, height: 460,
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
          <p style={{ fontSize: 12, color: T.textTertiary, letterSpacing: "-0.12px" }}>
            Settings content is being migrated to Tauri. Backend wiring (resume library, API keys, account) comes next.
          </p>
        </div>
      </div>
    </div>
  );
};

// ─── Main app ─────────────────────────────────────────────────────────────

const App = () => {
  const [jobs, setJobs] = useState<Job[]>(INITIAL_JOBS);
  const [selectedJobId, setSelectedJobId] = useState<string | null>("job1");
  const [selectedChatId, setSelectedChatId] = useState<string | null>("c1");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [showNewJobModal, setShowNewJobModal] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [activeScreen, setActiveScreen] = useState<Screen>("chat");

  const selectedJob  = jobs.find((j) => j.id === selectedJobId) ?? null;
  const selectedChat = selectedJob?.chats.find((c) => c.id === selectedChatId) ?? null;

  const onSelectJob = (jobId: string) => {
    setSelectedJobId(jobId);
    const j = jobs.find((x) => x.id === jobId);
    setSelectedChatId(j?.chats[0]?.id ?? null);
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

    setJobs((prev) => prev.map((j) =>
      j.id !== selectedJobId
        ? j
        : { ...j, chats: j.chats.map((c) => c.id !== chatId ? c : { ...c, messages: [...c.messages, { role: "user", content: text }] }) }
    ));

    setIsLoading(true);
    // TODO: replace with Tauri IPC streaming once backend is wired.
    window.setTimeout(() => {
      setJobs((prev) => prev.map((j) =>
        j.id !== selectedJobId
          ? j
          : { ...j, chats: j.chats.map((c) => c.id !== chatId ? c : { ...c, messages: [...c.messages, { role: "ai", content: "*(Backend not yet wired — this is a mock response. The Python sidecar will be plumbed through Tauri IPC in the next migration phase.)*" }] }) }
      ));
      setIsLoading(false);
    }, 600);
  };

  const onCreateJob = (form: NewJobFormState) => {
    const id = `job-${Date.now()}`;
    const palette = ["#6366F1", "#10B981", "#F59E0B", "#EC4899", "#8B5CF6"];
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
      ],
    };
    setJobs((prev) => [...prev, newJob]);
    setSelectedJobId(id);
    setSelectedChatId(`c-new-${id}`);
    setShowNewJobModal(false);
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
        collapsed={sidebarCollapsed}
        activeScreen={activeScreen}
        onSetScreen={setActiveScreen}
      />

      <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", minWidth: 0 }}>
        {activeScreen === "timeline" ? (
          <div style={{
            flex: 1, display: "flex", alignItems: "center", justifyContent: "center",
            color: T.textTertiary, fontSize: 14,
          }}>
            Timeline view — Gantt port is the next milestone.
          </div>
        ) : (
          <>
            <WorkspaceHeader
              job={selectedJob}
              onToggleSidebar={() => setSidebarCollapsed((c) => !c)}
            />
            {selectedJob ? (
              <>
                <ChatArea
                  chat={selectedChat}
                  onSendMessage={onSendMessage}
                  isLoading={isLoading}
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
      {showSettings    && <SettingsModal onClose={() => setShowSettings(false)} />}
    </div>
  );
};

export default App;
