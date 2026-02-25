use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTokenUsage {
    pub tool: String,
    pub tool_type: Option<String>,
    pub input: u64,
    pub output: u64,
    pub total: u64,
}
