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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatThread {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub messages: Vec<ChatMsg>,
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
}
