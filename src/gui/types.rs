use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use crate::gui::theme::*;

// ─── Data types ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus { Applied, Screening, Technical, Offer, Rejected }

impl JobStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Applied   => "Applied",
            Self::Screening => "Screening",
            Self::Technical => "Technical",
            Self::Offer     => "Offer",
            Self::Rejected  => "Rejected",
        }
    }
    pub fn color(self) -> Color32 {
        match self {
            Self::Applied   => C_APPLIED,
            Self::Screening => C_SCREENING,
            Self::Technical => C_TECHNICAL,
            Self::Offer     => C_OFFER,
            Self::Rejected  => C_REJECTED,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageNote {
    pub day_offset: i32,
    pub outcome: String,
    pub notes: String,
}
impl StageNote {
    pub fn new(day_offset: i32, outcome: &str, notes: &str) -> Self {
        Self { day_offset, outcome: outcome.to_owned(), notes: notes.to_owned() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MsgRole { User, Ai }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: MsgRole,
    pub text: String,
    /// Streaming flag is transient UI state — never persisted as `true`.
    #[serde(default, skip)]
    pub streaming: bool,
    /// Agent progress/tool-call logs rendered above the bubble in a disclosure.
    #[serde(default)]
    pub logs: Vec<String>,
}
impl ChatMsg {
    pub fn user(t: &str) -> Self {
        Self { role: MsgRole::User, text: t.to_owned(), streaming: false, logs: Vec::new() }
    }
    pub fn ai(t: &str) -> Self {
        Self { role: MsgRole::Ai, text: t.to_owned(), streaming: false, logs: Vec::new() }
    }
    pub fn ai_streaming() -> Self {
        Self { role: MsgRole::Ai, text: String::new(), streaming: true, logs: Vec::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatThread {
    pub id: usize,
    pub title: String,
    pub messages: Vec<ChatMsg>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: usize,
    pub company: String,
    pub role: String,
    pub location: String,
    #[serde(default)]
    pub job_description: String,
    #[serde(default = "default_research_status")]
    pub research_status: String,
    pub status: JobStatus,
    pub current_stage: usize,
    pub stage_notes: Vec<StageNote>,
    pub chats: Vec<ChatThread>,
    #[serde(with = "color32_rgba")]
    pub avatar_color: Color32,
    pub sidebar_expanded: bool,
    /// Archived jobs are hidden from the main sidebar list but kept on disk.
    /// Surfaced via the "Archived" disclosure at the bottom of the sidebar.
    #[serde(default)]
    pub archived: bool,
    /// Plain-text tailored resume returned by the Application-Prep step.
    /// Fed into the Company-Research prompt as candidate context.
    #[serde(default)]
    pub tailored_resume: Option<String>,
    /// Absolute path to the generated `resume.docx`. None until prep finishes.
    #[serde(default)]
    pub resume_docx_path: Option<String>,
    /// Scorecard returned by the tailoring step (verbatim match %, etc.).
    /// Rendered as a card above the cover-letter bubble.
    #[serde(default)]
    pub scorecard: Option<serde_json::Value>,
}

fn default_research_status() -> String { "Not started".to_owned() }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen { Chat, Timeline }

/// serde helper to round-trip `Color32` as a 4-byte `[r, g, b, a]` array.
mod color32_rgba {
    use eframe::egui::Color32;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(c: &Color32, s: S) -> Result<S::Ok, S::Error> {
        [c.r(), c.g(), c.b(), c.a()].serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let [r, g, b, a] = <[u8; 4]>::deserialize(d)?;
        Ok(Color32::from_rgba_premultiplied(r, g, b, a))
    }
}
