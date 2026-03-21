//! Unified LLM abstraction for commit message inference.
//! Provider priority: OpenAI > Anthropic > Ollama (default). Store override takes precedence.

use crate::bedrock;
use crate::daemon_log;
use crate::session::Change;
use crate::store;

#[cfg(feature = "llama-embedded")]
use crate::{llama, model};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::env;

const MAX_DIFF_BYTES: usize = 50 * 1024;
const MAX_DIFF_BYTES_LLAMA: usize = 8 * 1024;
use std::sync::Mutex;

static RUNTIME: Mutex<Option<tokio::runtime::Runtime>> = Mutex::new(None);

fn with_runtime<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&tokio::runtime::Runtime) -> Result<T, String>,
{
    let mut guard = RUNTIME.lock().unwrap();
    if guard.is_none() {
        *guard = Some(tokio::runtime::Runtime::new().map_err(|e| e.to_string())?);
    }
    f(guard.as_ref().unwrap())
}

/// Run an async function on the shared runtime. Used for parallel draft refresh.
pub fn block_on_async<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let mut guard = RUNTIME.lock().unwrap();
    if guard.is_none() {
        *guard = Some(tokio::runtime::Runtime::new().expect("tokio runtime"));
    }
    guard.as_ref().unwrap().block_on(f)
}

const COMMIT_SYSTEM_PROMPT: &str = "You are a git commit message assistant. Output ONLY a JSON object: {\"subject\": \"...\", \"body\": \"...\"}. Subject: short, clear summary of what changed, max 72 chars. Do NOT use type(scope): prefix in the subject. Body: required. First line of body must be conventional commit (type(scope): description). Then 1-3 sentences describing the specific code changes and why. Use the conversation context. Wrap at 72 chars. Do not use generic phrases like 'resolve null pointer' or 'fix bug'. Base both on the actual code diffs and conversation. Never quote the conversation.";

/// Llama-specific system prompt. Avoids "..." placeholder which small models copy literally.
const COMMIT_SYSTEM_PROMPT_LLAMA: &str = "You are a git commit message assistant. Output ONLY valid JSON. Subject: short, clear summary, max 72 chars. No type(scope): in subject. Body: first line conventional (type(scope): description), then 1-3 sentences on the code changes. Base on the actual diffs and conversation. Never quote the conversation.";

/// True if body is a known placeholder that small models copy from prompts.
fn is_placeholder_body(body: &str) -> bool {
    let b = body.trim().to_lowercase();
    b.is_empty()
        || b == "your description here"
        || b == "added validation for user input."
        || b == "describe the code changes"
        || b == "describe the changes"
        || b == "describe the changes. no other text."
        || b.starts_with("1-3 sentences describing the code changes")
        || b.starts_with("describe the code changes.")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommitMessage {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

fn build_commit_prompt(
    turns: &[(String, String)],
    changes: &[Change],
    max_diff_bytes: usize,
    instruction_at_start: bool,
) -> String {
    let mut out = String::new();
    if instruction_at_start {
        out.push_str(
            "IMPORTANT: Respond with ONLY a JSON object: {\"subject\": \"...\", \"body\": \"...\"}. No other text. Body is required. Describe the specific code changes.\n\n",
        );
    }
    out.push_str("Conversation:\n---\n");
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
    out.push_str("\nCode changes (diffs) for this commit only. Write a distinct message (do not repeat the same wording as other commits):\n---\n");
    let mut total_bytes = 0;
    for change in changes {
        if total_bytes >= max_diff_bytes {
            break;
        }
        out.push_str("File: ");
        out.push_str(&change.file_path);
        out.push_str("\n");
        let remaining = max_diff_bytes - total_bytes;
        if change.diff.len() <= remaining {
            out.push_str(&change.diff);
            total_bytes += change.diff.len();
        } else {
            let end = remaining.min(change.diff.len());
            out.push_str(&change.diff[..end]);
            out.push_str("\n... (truncated)");
            total_bytes = max_diff_bytes;
        }
        out.push_str("\n---\n");
    }
    out.push_str("\nOutput ONLY a JSON object: {\"subject\": \"...\", \"body\": \"...\"}. Subject = plain summary (no type(scope):). Body = first line type(scope): description, then details.");
    out
}

/// Build prompt for Llama subject-only call (first of two).
fn build_llama_subject_prompt(turns: &[(String, String)], changes: &[Change]) -> String {
    let mut out = String::new();
    out.push_str(
        "IMPORTANT: Respond with ONLY a JSON object with key \"subject\". Subject must be a short, clear summary. Do NOT use type(scope): prefix. No other text.\n",
    );
    out.push_str("Write a SPECIFIC subject for the actual code changes in the diffs below (e.g. Add JSON extraction for commit subject). Do not use a generic phrase like 'add validation'.\n\n");
    out.push_str("Conversation:\n---\n");
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
        if total_bytes >= MAX_DIFF_BYTES_LLAMA {
            break;
        }
        out.push_str("File: ");
        out.push_str(&change.file_path);
        out.push_str("\n");
        let remaining = MAX_DIFF_BYTES_LLAMA - total_bytes;
        if change.diff.len() <= remaining {
            out.push_str(&change.diff);
            total_bytes += change.diff.len();
        } else {
            let end = remaining.min(change.diff.len());
            out.push_str(&change.diff[..end]);
            out.push_str("\n... (truncated)");
            total_bytes = MAX_DIFF_BYTES_LLAMA;
        }
        out.push_str("\n---\n");
    }
    out.push_str("\nOutput ONLY a JSON object with key \"subject\". Plain summary only, max 72 chars. No type(scope):. Be specific to the diffs above.");
    out
}

/// Build prompt for Llama body-only call (second of two).
fn build_llama_body_prompt(
    turns: &[(String, String)],
    changes: &[Change],
    subject: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Subject: {}\n\n", subject));
    out.push_str("Body must start with a conventional commit line: type(scope): description (e.g. fix(llm): add JSON extraction). Then 1-3 sentences describing the code changes. Keep this commit distinct from others.\n\n");
    out.push_str("Conversation:\n---\n");
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
        if total_bytes >= MAX_DIFF_BYTES_LLAMA {
            break;
        }
        out.push_str("File: ");
        out.push_str(&change.file_path);
        out.push_str("\n");
        let remaining = MAX_DIFF_BYTES_LLAMA - total_bytes;
        if change.diff.len() <= remaining {
            out.push_str(&change.diff);
            total_bytes += change.diff.len();
        } else {
            let end = remaining.min(change.diff.len());
            out.push_str(&change.diff[..end]);
            out.push_str("\n... (truncated)");
            total_bytes = MAX_DIFF_BYTES_LLAMA;
        }
        out.push_str("\n---\n");
    }
    out.push_str("\nBody (1-3 sentences): ");
    out
}

fn parse_commit_message(text: &str) -> Result<CommitMessage, String> {
    let normalized = text.replace("\r\n", "\n");
    let trimmed = normalized.trim();
    for candidate in extract_json_candidates(trimmed) {
        if let Ok(msg) = serde_json::from_str::<CommitMessage>(candidate) {
            return Ok(msg);
        }
    }
    if let Some(msg) = extract_subject_body_fallback(trimmed) {
        daemon_log::log(&format!("daemon: llm parse used fallback extractor subject={:?}", msg.subject));
        return Ok(msg);
    }
    if let Some(msg) = extract_subject_body_regex(trimmed) {
        daemon_log::log(&format!("daemon: llm parse used regex extractor subject={:?}", msg.subject));
        return Ok(msg);
    }
    daemon_log::log(&format!("daemon: llm parse failed; full response: {:?}", text));
    Err("llm: could not find json object".to_string())
}

/// Fallback: extract "subject":"..." and "body":"..." when JSON is malformed.
fn extract_subject_body_fallback(text: &str) -> Option<CommitMessage> {
    let subject = extract_json_string_value(text, "subject")
        .or_else(|| extract_json_string_value_quoted(text, "subject", '\''))?;
    let body = extract_json_string_value(text, "body")
        .or_else(|| extract_json_string_value_quoted(text, "body", '\''))
        .unwrap_or_default();
    if subject.is_empty() {
        return None;
    }
    Some(CommitMessage {
        subject: subject.trim().to_string(),
        body: body.trim().to_string(),
    })
}

/// Regex-based last resort for "subject"\s*:\s*"..." and "body"\s*:\s*"..."
fn extract_subject_body_regex(text: &str) -> Option<CommitMessage> {
    let re = Regex::new(r#""subject"\s*:\s*"((?:[^"\\]|\\.)*)""#).ok()?;
    let subject = re.captures(text)?.get(1)?.as_str();
    let subject = unescape_json_string(subject);
    if subject.trim().is_empty() {
        return None;
    }
    let body_re = Regex::new(r#""body"\s*:\s*"((?:[^"\\]|\\.)*)""#).ok()?;
    let body = body_re
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| unescape_json_string(m.as_str()))
        .unwrap_or_default();
    Some(CommitMessage {
        subject: subject.trim().to_string(),
        body: body.trim().to_string(),
    })
}

fn unescape_json_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(match n {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '"' => '"',
                    '\\' => '\\',
                    _ => n,
                });
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn extract_json_string_value(text: &str, key: &str) -> Option<String> {
    extract_json_string_value_impl(text, key, '"', '"')
}

fn extract_json_string_value_quoted(text: &str, key: &str, key_quote: char) -> Option<String> {
    let needle = format!("{}{}{}", key_quote, key, key_quote);
    let start = text.find(&needle)?;
    let after_key = &text[start + needle.len()..];
    let after_colon = after_key.trim_start_matches(|c| c == ':' || c == ' ').trim_start();
    let value_quote = if after_colon.starts_with('"') {
        '"'
    } else if after_colon.starts_with(key_quote) {
        key_quote
    } else {
        return None;
    };
    extract_json_string_value_impl(&text[start..], key, key_quote, value_quote)
}

fn extract_json_string_value_impl(text: &str, key: &str, key_quote: char, value_quote: char) -> Option<String> {
    let needle = format!("{}{}{}", key_quote, key, key_quote);
    let start = text.find(&needle)?;
    let after_key = &text[start + needle.len()..];
    let after_colon = after_key.trim_start_matches(|c| c == ':' || c == ' ').trim_start();
    if !after_colon.starts_with(value_quote) {
        return None;
    }
    let after = &after_colon[1..];
    let mut out = String::new();
    let mut chars = after.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(if n == 'n' { '\n' } else if n == '"' { '"' } else { n });
                }
            }
            c if c == value_quote => break,
            _ => out.push(c),
        }
    }
    Some(out)
}

fn extract_json_candidates(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    out.push(text);
    if let Some(s) = strip_code_fences_at_start(text) {
        out.push(s);
    }
    for s in find_code_blocks(text) {
        out.push(s);
    }
    if let Some(s) = extract_first_json_object(text) {
        out.push(s);
    }
    out
}

fn strip_code_fences_at_start(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let without_start = trimmed.trim_start_matches("```");
    let without_lang = without_start.trim_start_matches("json").trim_start();
    let end = without_lang.rfind("```")?;
    Some(without_lang[..end].trim())
}

fn find_code_blocks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut search = text;
    while let Some(open) = search.find("```") {
        let after_open = &search[open + 3..];
        let content = after_open
            .trim_start_matches("json")
            .trim_start_matches('\n')
            .trim_start();
        let Some(close) = content.find("```") else { break };
        let candidate = content[..close].trim();
        if candidate.contains('{') {
            out.push(candidate);
        }
        search = &content[close + 3..];
    }
    out
}

fn extract_first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0u32;
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Copy)]
enum Provider {
    OpenAI,
    Anthropic,
    Ollama,
    #[cfg(feature = "llama-embedded")]
    Llama,
    Bedrock,
}

fn ollama_base_url() -> String {
    env::var("GG_OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string())
}

fn ollama_model() -> String {
    store::get_ollama_model()
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            env::var("GG_OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string())
        })
}

fn select_provider() -> Option<Provider> {
    if let Ok(Some(override_provider)) = store::get_llm_provider() {
        let p = override_provider.to_lowercase();
        match p.as_str() {
            "openai" if env::var("OPENAI_API_KEY").is_ok() => return Some(Provider::OpenAI),
            "anthropic" if env::var("ANTHROPIC_API_KEY").is_ok() => return Some(Provider::Anthropic),
            "ollama" => return Some(Provider::Ollama),
            #[cfg(feature = "llama-embedded")]
            "llama" => return Some(Provider::Llama),
            _ => {}
        }
    }

    if env::var("OPENAI_API_KEY").is_ok() {
        return Some(Provider::OpenAI);
    }
    if env::var("ANTHROPIC_API_KEY").is_ok() {
        return Some(Provider::Anthropic);
    }
    // Prefer Ollama when running (same model is much faster than embedded path, which loads model and does 2 completions per draft).
    return Some(Provider::Ollama);
}

async fn infer_openai(turns: &[(String, String)], changes: &[Change]) -> Result<CommitMessage, String> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY not set")?;
    let model = env::var("GG_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let user_prompt = build_commit_prompt(turns, changes, MAX_DIFF_BYTES, false);

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": COMMIT_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt}
        ],
        "max_tokens": 500
    });

    let res = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("openai: request failed: {e}"))?;

    let status = res.status();
    let text = res.text().await.map_err(|e| format!("openai: read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("openai: api error {}: {}", status, text));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("openai: invalid json: {e}"))?;
    let content = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "openai: missing content in response".to_string())?;

    parse_commit_message(content)
}

async fn infer_anthropic(turns: &[(String, String)], changes: &[Change]) -> Result<CommitMessage, String> {
    let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| "ANTHROPIC_API_KEY not set")?;
    let model = env::var("GG_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-3-5-haiku-20241022".to_string());
    let user_prompt = build_commit_prompt(turns, changes, MAX_DIFF_BYTES, false);

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 500,
        "system": COMMIT_SYSTEM_PROMPT,
        "messages": [
            {"role": "user", "content": user_prompt}
        ]
    });

    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("anthropic: request failed: {e}"))?;

    let status = res.status();
    let text = res.text().await.map_err(|e| format!("anthropic: read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("anthropic: api error {}: {}", status, text));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("anthropic: invalid json: {e}"))?;
    let content = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| "anthropic: missing content in response".to_string())?;

    parse_commit_message(content)
}

async fn infer_ollama(turns: &[(String, String)], changes: &[Change]) -> Result<CommitMessage, String> {
    let base = ollama_base_url();
    let model = ollama_model();
    daemon_log::log(&format!("daemon: llm using Ollama model={} url={}", model, base));
    let user_prompt = build_commit_prompt(turns, changes, MAX_DIFF_BYTES, false);

    let client = reqwest::Client::new();
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": COMMIT_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt}
        ],
        "stream": false
    });

    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama: request failed: {e}"))?;

    let status = res.status();
    let text = res.text().await.map_err(|e| format!("ollama: read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("ollama: api error {}: {}", status, text));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("ollama: invalid json: {e}"))?;
    let content = json
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "ollama: missing content in response".to_string())?;

    parse_commit_message(content)
}

#[cfg(feature = "llama-embedded")]
fn infer_llama_blocking(turns: &[(String, String)], changes: &[Change]) -> Result<CommitMessage, String> {
    daemon_log::log("daemon: llm infer_llama_blocking start (two-call: subject, then body)...");
    let model_path = model::default_model_path().ok();
    let max_tokens = env::var("GG_LLAMA_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512);
    let timeout_ms = env::var("GG_LLAMA_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);

    // Call 1: subject only
    let subject_prompt = build_llama_subject_prompt(turns, changes);
    let prompt1 = format!(
        "<|im_start|>system\n{}\n<|im_end|>\n<|im_start|>user\n{}\n<|im_end|>\n<|im_start|>assistant\n{{\"subject\":\"",
        COMMIT_SYSTEM_PROMPT_LLAMA,
        subject_prompt
    );
    daemon_log::log(&format!("daemon: llm llama call 1 (subject) prompt_len={}...", prompt1.len()));
    let out1 = llama::run_completion(&prompt1, max_tokens, timeout_ms, model_path.as_deref())?;
    let value1 = out1.trim().strip_suffix("\"}").unwrap_or(out1.trim()).trim_matches('"');
    let to_parse1 = format!(
        "{{\"subject\":\"{}\"}}",
        value1.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
    );
    let mut subject = extract_json_string_value(&to_parse1, "subject")
        .or_else(|| extract_subject_body_fallback(&to_parse1).map(|m| m.subject))
        .unwrap_or_else(|| value1.to_string());
    // Model may output both subject and body in one go; take only the subject part
    if let Some(pos) = subject.find("\",\"body\"") {
        subject = subject[..pos].to_string();
    }
    let subject = subject.trim().to_string();
    daemon_log::log(&format!("daemon: llm llama subject={:?}", subject));

    if subject.is_empty() {
        return Err("llm: could not extract subject from llama output".to_string());
    }

    // Call 2: body only (given subject)
    let body_prompt = build_llama_body_prompt(turns, changes, &subject);
    let prompt2 = format!(
        "<|im_start|>system\n{}\n<|im_end|>\n<|im_start|>user\n{}\n<|im_end|>\n<|im_start|>assistant\n{{\"body\":\"",
        COMMIT_SYSTEM_PROMPT_LLAMA,
        body_prompt
    );
    daemon_log::log(&format!("daemon: llm llama call 2 (body) prompt_len={}...", prompt2.len()));
    let out2 = llama::run_completion(&prompt2, max_tokens, timeout_ms, model_path.as_deref())?;
    let value2 = out2.trim().strip_suffix("\"}").unwrap_or(out2.trim()).trim_matches('"');
    let to_parse2 = format!(
        "{{\"body\":\"{}\"}}",
        value2.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
    );
    let mut body = extract_json_string_value(&to_parse2, "body")
        .unwrap_or_else(|| value2.to_string());
    // Model may append extra JSON; take only the first value
    if let Some(pos) = body.find("\",\"") {
        body = body[..pos].to_string();
    }
    let body = body.trim().to_string();
    // Small models often copy prompt placeholders; treat as empty
    let body = if is_placeholder_body(&body) {
        daemon_log::log("daemon: llm body is placeholder, using empty (try GG_LLAMA_MODEL with larger model for descriptions)");
        String::new()
    } else {
        body
    };
    daemon_log::log(&format!("daemon: llm llama body len={}", body.len()));

    Ok(CommitMessage { subject, body })
}

fn infer_bedrock_blocking(turns: &[(String, String)], changes: &[Change]) -> Result<CommitMessage, String> {
    let msg = bedrock::infer_commit_message_blocking(turns, changes)?;
    Ok(CommitMessage {
        subject: msg.subject,
        body: msg.body,
    })
}

pub fn infer_commit_message_blocking(
    turns: &[(String, String)],
    changes: &[Change],
) -> Result<CommitMessage, String> {
    with_runtime(|rt| rt.block_on(infer_commit_message_async(turns, changes)))
}

/// Async inference for parallel draft refresh. API providers run concurrently; Llama/Bedrock use spawn_blocking.
pub async fn infer_commit_message_async(
    turns: &[(String, String)],
    changes: &[Change],
) -> Result<CommitMessage, String> {
    daemon_log::log(&format!(
        "daemon: llm infer turns={} changes={}",
        turns.len(),
        changes.len()
    ));
    if turns.is_empty() && changes.is_empty() {
        daemon_log::log("daemon: llm skip inference (no turns or changes)");
        return Ok(CommitMessage {
            subject: "chore: update".to_string(),
            body: String::new(),
        });
    }
    let provider = select_provider().ok_or_else(|| {
        daemon_log::log("daemon: llm no provider available (need OPENAI_API_KEY, ANTHROPIC_API_KEY, or Ollama)");
        "No LLM provider available. Set OPENAI_API_KEY, ANTHROPIC_API_KEY, or run Ollama locally.".to_string()
    })?;

    match provider {
        Provider::OpenAI => {
            daemon_log::log("daemon: llm commit inference: using OpenAI");
            infer_openai(turns, changes).await
        }
        Provider::Anthropic => {
            daemon_log::log("daemon: llm commit inference: using Anthropic");
            infer_anthropic(turns, changes).await
        }
        Provider::Ollama => {
            let model = ollama_model();
            daemon_log::log(&format!("daemon: llm commit inference: using Ollama (model={})", model));
            infer_ollama(turns, changes).await
        }
        #[cfg(feature = "llama-embedded")]
        Provider::Llama => {
            daemon_log::log("daemon: llm commit inference: using Llama");
            let turns = turns.to_vec();
            let changes = changes.to_vec();
            tokio::task::spawn_blocking(move || infer_llama_blocking(&turns, &changes))
                .await
                .map_err(|e| format!("llama task join: {e}"))?
        }
        Provider::Bedrock => {
            daemon_log::log("daemon: llm commit inference: using Bedrock");
            let turns = turns.to_vec();
            let changes = changes.to_vec();
            tokio::task::spawn_blocking(move || infer_bedrock_blocking(&turns, &changes))
                .await
                .map_err(|e| format!("bedrock task join: {e}"))?
        }
    }
}

/// Groups changes into logical commits. Returns Vec of (subject_placeholder, Vec<change_indices>).
pub async fn infer_grouping_async(changes: &[Change]) -> Result<Vec<(String, Vec<usize>)>, String> {
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    if changes.len() == 1 {
        return Ok(vec![("fix: (generating...)".to_string(), vec![0])]);
    }

    let provider = select_provider().ok_or_else(|| {
        daemon_log::log("daemon: llm grouping: no provider available");
        "No LLM provider available for grouping.".to_string()
    })?;

    daemon_log::log(&format!("daemon: llm grouping: {} changes, provider={:?}", changes.len(), provider));

    match provider {
        Provider::OpenAI => infer_grouping_openai(changes).await,
        Provider::Anthropic => infer_grouping_anthropic(changes).await,
        Provider::Ollama => infer_grouping_ollama(changes).await,
        #[cfg(feature = "llama-embedded")]
        Provider::Llama => {
            let changes = changes.to_vec();
            tokio::task::spawn_blocking(move || infer_grouping_llama_blocking(&changes))
                .await
                .map_err(|e| format!("llama grouping task join: {e}"))?
        }
        Provider::Bedrock => Err("Grouping not supported for Bedrock provider.".to_string()),
    }
}

const GROUPING_SYSTEM_PROMPT: &str = "You are a git commit assistant. Given a list of code changes (each with an index 0, 1, 2...), group them into logical commits. Output ONLY a JSON array. Each element: {\"subject\": \"short plain description\", \"indices\": [0, 2, 5]}. Subject = clear summary only, no type(scope): prefix. Each index must appear exactly once. Subject max 72 chars.";

fn build_grouping_prompt(changes: &[Change], max_bytes: usize) -> String {
    let mut out = String::new();
    out.push_str("Code changes to group (index : file : diff):\n---\n");
    let mut total_bytes = 0;
    for (i, change) in changes.iter().enumerate() {
        if total_bytes >= max_bytes {
            break;
        }
        out.push_str(&format!("[{}] File: {}\n", i, change.file_path));
        let remaining = max_bytes - total_bytes;
        if change.diff.len() <= remaining {
            out.push_str(&change.diff);
            total_bytes += change.diff.len();
        } else {
            let end = remaining.min(change.diff.len());
            out.push_str(&change.diff[..end]);
            out.push_str("\n... (truncated)");
            total_bytes = max_bytes;
        }
        out.push_str("\n---\n");
    }
    out.push_str("\nOutput ONLY a JSON array. Each subject = plain short description (no type(scope):). Example: [{\"subject\":\"Handle empty input in parser\",\"indices\":[0,2]},{\"subject\":\"Add auth middleware to API\",\"indices\":[1]}]");
    out
}

#[derive(Debug, serde::Deserialize)]
struct GroupingItem {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    indices: Vec<usize>,
}

fn parse_grouping_response(text: &str, n_changes: usize) -> Result<Vec<(String, Vec<usize>)>, String> {
    let trimmed = text.trim();
    let cleaned = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let start = cleaned.find('[').ok_or_else(|| "llm: no JSON array in grouping response".to_string())?;
    let end = cleaned.rfind(']').ok_or_else(|| "llm: no JSON array in grouping response".to_string())?;
    if end <= start {
        return Err("llm: invalid JSON array in grouping response".to_string());
    }
    let arr_str = &cleaned[start..=end];
    let items: Vec<GroupingItem> = serde_json::from_str(arr_str)
        .map_err(|e| format!("llm: parse grouping JSON: {}", e))?;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let subject = if item.subject.trim().is_empty() {
            "fix: (generating...)".to_string()
        } else {
            item.subject.trim().to_string()
        };
        let mut indices: Vec<usize> = item
            .indices
            .into_iter()
            .filter(|&i| i < n_changes && seen.insert(i))
            .collect();
        indices.sort();
        if !indices.is_empty() {
            out.push((subject, indices));
        }
    }
    let assigned: usize = out.iter().map(|(_, idx)| idx.len()).sum();
    if assigned < n_changes {
        let missing: Vec<usize> = (0..n_changes).filter(|i| !seen.contains(i)).collect();
        daemon_log::log(&format!("daemon: llm grouping: {} indices not assigned, adding as single group: {:?}", n_changes - assigned, missing));
        out.push(("fix: (generating...)".to_string(), missing));
    }
    if out.is_empty() {
        return Err("llm: grouping produced no groups".to_string());
    }
    Ok(out)
}

async fn infer_grouping_openai(changes: &[Change]) -> Result<Vec<(String, Vec<usize>)>, String> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY not set")?;
    let model = env::var("GG_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let user_prompt = build_grouping_prompt(changes, MAX_DIFF_BYTES);

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": GROUPING_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt}
        ],
        "max_tokens": 1000
    });

    let res = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("openai grouping: {}", e))?;

    let text = res.text().await.map_err(|e| format!("openai grouping: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("openai grouping: {}", e))?;
    let content = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "openai grouping: missing content".to_string())?;

    parse_grouping_response(content, changes.len())
}

async fn infer_grouping_anthropic(changes: &[Change]) -> Result<Vec<(String, Vec<usize>)>, String> {
    let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| "ANTHROPIC_API_KEY not set")?;
    let model = env::var("GG_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-3-5-haiku-20241022".to_string());
    let user_prompt = build_grouping_prompt(changes, MAX_DIFF_BYTES);

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1000,
        "messages": [
            {"role": "user", "content": user_prompt}
        ],
        "system": GROUPING_SYSTEM_PROMPT
    });

    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("anthropic grouping: {}", e))?;

    let text = res.text().await.map_err(|e| format!("anthropic grouping: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("anthropic grouping: {}", e))?;
    let content = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| "anthropic grouping: missing content".to_string())?;

    parse_grouping_response(content, changes.len())
}

async fn infer_grouping_ollama(changes: &[Change]) -> Result<Vec<(String, Vec<usize>)>, String> {
    let base = ollama_base_url();
    let model = ollama_model();
    let user_prompt = build_grouping_prompt(changes, MAX_DIFF_BYTES);

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": GROUPING_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt}
        ]
    });

    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let res = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama grouping: {}", e))?;

    let text = res.text().await.map_err(|e| format!("ollama grouping: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("ollama grouping: {}", e))?;
    let content = json
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "ollama grouping: missing content".to_string())?;

    parse_grouping_response(content, changes.len())
}

#[cfg(feature = "llama-embedded")]
fn infer_grouping_llama_blocking(changes: &[Change]) -> Result<Vec<(String, Vec<usize>)>, String> {
    daemon_log::log("daemon: llm infer_grouping_llama_blocking start...");
    let model_path = model::default_model_path().ok();
    let max_tokens = env::var("GG_LLAMA_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512);
    let timeout_ms = env::var("GG_LLAMA_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);

    let user_prompt = build_grouping_prompt(changes, MAX_DIFF_BYTES_LLAMA);
    let prompt = format!(
        "<|im_start|>system\n{}\n<|im_end|>\n<|im_start|>user\n{}\n<|im_end|>\n<|im_start|>assistant\n",
        GROUPING_SYSTEM_PROMPT,
        user_prompt
    );

    daemon_log::log(&format!("daemon: llm grouping llama prompt_len={}...", prompt.len()));
    let out = llama::run_completion(&prompt, max_tokens, timeout_ms, model_path.as_deref())?;
    parse_grouping_response(out.trim(), changes.len())
}

/// Fetch list of installed Ollama models. Used by dashboard for model selection.
pub fn list_ollama_models_blocking() -> Result<Vec<String>, String> {
    block_on_async(list_ollama_models_async())
}

async fn list_ollama_models_async() -> Result<Vec<String>, String> {
    let base = env::var("GG_OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| format!("ollama client: {e}"))?;

    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("ollama: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("ollama: {}", res.status()));
    }

    let json: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("ollama: invalid json: {e}"))?;

    let models = json
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "ollama: missing models array".to_string())?;

    let names: Vec<String> = models
        .iter()
        .filter_map(|m| m.get("name").or(m.get("model")).and_then(|n| n.as_str()))
        .map(str::to_string)
        .collect();

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_explanatory_response_with_code_block() {
        let response = "You've provided a JSON object with the subject and body of the commit message. The JSON object should be formatted as follows:\n\n```json\n{\n  \"subject\": \"fix: update deps\",\n  \"body\": \"Updated Cargo.toml\"\n}\n```\n\nThe JSON object should be formatted as follows:\n\n```json\n{\n  \"subject\": \"...\"\n}\n```";
        let result = parse_commit_message(response);
        assert!(result.is_ok(), "expected ok, got {:?}", result);
        let msg = result.unwrap();
        assert_eq!(msg.subject, "fix: update deps");
        assert_eq!(msg.body, "Updated Cargo.toml");
    }

    #[test]
    fn test_parse_template_only_json() {
        let response = "You've provided a JSON object with the subject and body of the commit message. The JSON object should be formatted as follows:\n\n```json\n{\n  \"subject\": \"...\"\n}\n```\n\nThe JSON object should be formatted as follows:\n\n```json\n{\n  \"subject\": \"...\"\n}\n```";
        let result = parse_commit_message(response);
        assert!(result.is_ok(), "expected ok, got {:?}", result);
        let msg = result.unwrap();
        assert_eq!(msg.subject, "...");
        assert_eq!(msg.body, "");
    }
}
