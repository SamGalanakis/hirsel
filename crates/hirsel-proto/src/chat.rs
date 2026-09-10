//! Chat rows, their authors, and the blobs/tool summaries they carry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatAuthor {
    Owner,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub thread_id: u64,
    #[serde(default)]
    pub mentions: Vec<u64>,
    pub id: u64,
    pub author: ChatAuthor,
    pub body: String,
    #[serde(rename = "ref")]
    pub r#ref: Option<u64>,
    pub ts: DateTime<Utc>,
    #[serde(default)]
    pub attachments: Vec<Blob>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    pub ok: bool,
}
