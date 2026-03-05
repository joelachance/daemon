use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub output: String,
    pub timestamp: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub response: String,
    pub tool_calls: Vec<ToolCall>,
    pub change_ids: Vec<String>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeLineRange {
    pub old_start: i64,
    pub old_count: i64,
    pub new_start: i64,
    pub new_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub session_id: String,
    pub prompt_id: String,
    pub file_path: String,
    pub base_commit_sha: String,
    pub diff: String,
    pub line_range: ChangeLineRange,
    pub captured_at: i64,
    pub change_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DraftStatus {
    Draft,
    Approved,
    Rejected,
}

impl DraftStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftCommit {
    pub id: String,
    pub session_id: String,
    pub message: String,
    pub status: DraftStatus,
    pub created_at: i64,
    pub order: i64,
    pub auto_approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTokenUsage {
    pub tool: String,
    pub tool_type: Option<String>,
    pub input: u64,
    pub output: u64,
    pub total: u64,
}
