//! Shared domain types serialized over Tauri IPC and persisted to disk.
//!
//! The struct layout deliberately matches the React frontend's `Job` /
//! `ChatThread` / `ChatMsg` interfaces. `#[serde(rename_all = "camelCase")]`
//! handles the snake_case ↔ camelCase translation so both sides see their
//! native idioms. IDs are `String` because the frontend mints them with
//! `Date.now()` and JS can't safely round-trip `u64`.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Applied,
    Screening,
    Technical,
    Offer,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StageNote {
    pub date: String,
    pub outcome: String,
    pub notes: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgRole {
    User,
    Ai,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: MsgRole,
    pub content: String,
    /// Agent progress log lines collected while the stream was running.
    /// Rendered in the "Thinking" disclosure above the bubble.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    /// True while tokens are still streaming into `content`. Persisted so a
    /// reload doesn't drop the streaming cursor mid-response (in which case
    /// the frontend treats it as an interrupted message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Sent to the backend but not rendered (e.g. the Mock Interview kickoff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatThread {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub messages: Vec<ChatMsg>,
    /// "interviewer" drives the live mock-interview persona; default "coach".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Pre-start config for Mock Interview threads (opaque to Rust).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interview_config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub company: String,
    pub role: String,
    pub location: String,
    #[serde(default)]
    pub url: String,
    pub status: JobStatus,
    pub applied_date: String,
    pub current_stage: u32,
    pub stage_notes: HashMap<u32, StageNote>,
    pub avatar: String,
    pub avatar_color: String,
    pub chats: Vec<ChatThread>,
    #[serde(default)]
    pub job_description: Option<String>,
    #[serde(default)]
    pub archived: bool,
    /// Plain-text tailored resume from the Application-Prep step.
    /// Fed into Company Research as candidate background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailored_resume: Option<String>,
    /// Absolute path to the generated `resume.docx`. Set after Prep finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_docx_path: Option<String>,
    /// ATS scorecard returned by the tailoring step (verbatim match %, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorecard: Option<serde_json::Value>,
    /// Outcome of the most recent completed mock interview (opaque to Rust).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_interview: Option<serde_json::Value>,
}
