use crate::session::Change;
use aws_config;
use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, Message, SystemContentBlock};
use aws_sdk_bedrockruntime::Client;
use aws_types::region::Region;
use serde::Deserialize;
use std::env;

const MAX_DIFF_BYTES: usize = 50 * 1024;

const DEFAULT_MODEL_ID: &str = "amazon.nova-micro-v1:0";
const DEFAULT_REGION: &str = "us-west-2";

#[derive(Debug, Deserialize, Default)]
pub struct CommitMessage {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

pub struct BedrockClient {
    client: Client,
    model_id: String,
}

impl BedrockClient {
    pub async fn new() -> Result<Self, String> {
        let config = aws_config::from_env()
            .region(Region::new(DEFAULT_REGION))
            .load()
            .await;
        let client = Client::new(&config);
        let model_id = env::var("GG_BEDROCK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_ID.to_string());
        Ok(Self { client, model_id })
    }

    pub async fn infer_commit_message(
        &self,
        turns: &[(String, String)],
        changes: &[Change],
    ) -> Result<CommitMessage, String> {
        let system_prompt = "You are a git commit message assistant. Output only a JSON object with keys: subject, body. subject must be conventional commit format (type(scope): description), max 72 chars. Infer a summarization of the code changes based on the session conversation. The subject must describe the code changes. Never quote or paraphrase the conversation (e.g. no 'Ok, here's...', 'we need to follow', 'let me...'). The body must describe the intent and what changed in the code. Use the full conversation context, not just the first line. Wrap body lines at 72 chars. Do not include explanations outside JSON.";
        let user_prompt = build_commit_prompt(turns, changes);

        let message = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(user_prompt))
            .build()
            .map_err(|err| err.to_string())?;

        eprintln!("daemon: bedrock commit inference: model={}", self.model_id);
        let response = self
            .client
            .converse()
            .model_id(&self.model_id)
            .system(SystemContentBlock::Text(system_prompt.to_string()))
            .messages(message)
            .send()
            .await
            .map_err(|err| {
                eprintln!("daemon: bedrock commit inference failed: {err}");
                err.to_string()
            })?;

        let output = response
            .output()
            .and_then(|output| output.as_message().ok())
            .ok_or_else(|| "bedrock: missing output message".to_string())?;

        let text = extract_text(output.content());
        if text.trim().is_empty() {
            eprintln!("daemon: bedrock commit inference failed: empty response");
            return Err("bedrock: empty response".to_string());
        }

        let msg = parse_commit_message(&text);
        if msg.is_ok() {
            eprintln!("daemon: bedrock commit inference: ok");
        } else if let Err(ref e) = msg {
            eprintln!("daemon: bedrock commit inference failed: {e}");
        }
        msg
    }
}

fn build_commit_prompt(turns: &[(String, String)], changes: &[Change]) -> String {
    let mut out = String::from("Conversation:\n---\n");
    for (prompt, response) in turns {
        let p = prompt.trim();
        let r = response.trim();
        if !p.is_empty() {
            out.push_str("User: ");
            out.push_str(p);
            out.push_str("\n---\n");
        }
        if !r.is_empty() {
            out.push_str("Assistant: ");
            out.push_str(r);
            out.push_str("\n---\n");
        }
    }
    out.push_str("\nCode changes (diffs):\n---\n");
    let mut total_bytes = 0;
    for change in changes {
        if total_bytes >= MAX_DIFF_BYTES {
            break;
        }
        out.push_str("File: ");
        out.push_str(&change.file_path);
        out.push_str("\n");
        let remaining = MAX_DIFF_BYTES - total_bytes;
        if change.diff.len() <= remaining {
            out.push_str(&change.diff);
            total_bytes += change.diff.len();
        } else {
            let end = remaining.min(change.diff.len());
            out.push_str(&change.diff[..end]);
            out.push_str("\n... (truncated)");
            total_bytes = MAX_DIFF_BYTES;
        }
        out.push_str("\n---\n");
    }
    out.push_str("\nGenerate a git commit subject and body. Subject must be conventional commit format (type(scope): description). Body describes intent and changes.");
    out
}

fn parse_commit_message(text: &str) -> Result<CommitMessage, String> {
    let trimmed = text.trim();
    let cleaned = strip_code_fences(trimmed);
    if let Ok(msg) = serde_json::from_str::<CommitMessage>(cleaned) {
        return Ok(msg);
    }

    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    let err = match (start, end) {
        (Some(start), Some(end)) if end > start => {
            let slice = &cleaned[start..=end];
            serde_json::from_str::<CommitMessage>(slice)
                .map_err(|err| format!("bedrock: invalid json ({err})"))
        }
        _ => Err("bedrock: could not find json object".to_string()),
    };
    if let Err(ref e) = err {
        let preview: String = text.chars().take(500).collect();
        eprintln!("daemon: bedrock parse failed: {}; response preview: {:?}", e, preview);
    }
    err
}

pub fn infer_commit_message_blocking(
    turns: &[(String, String)],
    changes: &[Change],
) -> Result<CommitMessage, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = BedrockClient::new().await?;
        client.infer_commit_message(turns, changes).await
    })
}

fn extract_text(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in blocks {
        if let ContentBlock::Text(value) = block {
            text.push_str(value);
        }
    }
    text
}

fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        let without_start = trimmed.trim_start_matches("```");
        let without_lang = without_start.trim_start_matches("json").trim_start();
        if let Some(end) = without_lang.rfind("```") {
            return without_lang[..end].trim();
        }
    }
    trimmed
}
