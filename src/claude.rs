use crate::daemon;
use crate::session::TokenUsage;
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use time::OffsetDateTime;

const DEFAULT_CLAUDE_DIR: &str = ".claude";

#[derive(Debug, Default, Serialize, Deserialize)]
struct ClaudeState {
    offsets: HashMap<String, u64>,
}

pub fn poll_assistant_responses(
    root: &Path,
    git_stdout: bool,
    compact: bool,
) -> Result<usize, String> {
    let root = root.canonicalize().map_err(|err| err.to_string())?;
    let projects_dir = claude_projects_dir()?;
    let project_dir = project_dir_for_root(&root, &projects_dir);
    if !project_dir.exists() {
        return Ok(0);
    }

    let mut state = load_state(&root);
    let mut emitted = 0usize;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let active_window_secs = active_window_secs();

    let entries = fs::read_dir(&project_dir).map_err(|err| err.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        let session_id = match path.file_stem().and_then(|name| name.to_str()) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => continue,
        };
        let is_recent = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|dur| dur.as_secs() as i64 >= now - active_window_secs)
            .unwrap_or(false);
        if !is_recent {
            continue;
        }

        daemon::upsert_session_presence(&session_id, "claude", &root, None)?;
        store::touch_session(&session_id, now)?;

        let count =
            process_session_file(&root, &path, &session_id, &mut state, git_stdout, compact)?;
        emitted += count;
    }

    save_state(&root, &state)?;

    Ok(emitted)
}

fn active_window_secs() -> i64 {
    env::var("GG_ACTIVE_WINDOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(900)
}

fn claude_projects_dir() -> Result<PathBuf, String> {
    if let Ok(value) = env::var("GG_CLAUDE_PROJECTS_DIR") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }

    if let Ok(value) = env::var("GG_CLAUDE_DIR") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value).join("projects"));
        }
    }

    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home)
        .join(DEFAULT_CLAUDE_DIR)
        .join("projects"))
}

fn project_dir_for_root(root: &Path, projects_dir: &Path) -> PathBuf {
    projects_dir.join(encode_project_dir_name(root))
}

fn encode_project_dir_name(root: &Path) -> String {
    root.to_string_lossy().replace('/', "-")
}

fn process_session_file(
    root: &Path,
    path: &Path,
    session_id: &str,
    state: &mut ClaudeState,
    git_stdout: bool,
    compact: bool,
) -> Result<usize, String> {
    let mut emitted = 0usize;
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let file_len = file.metadata().map_err(|err| err.to_string())?.len();

    let offset = state.offsets.get(session_id).copied().unwrap_or(0);
    let offset = if offset > file_len { 0 } else { offset };
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| err.to_string())?;

    let mut reader = BufReader::new(file);
    let mut bytes_read_total = offset;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if bytes == 0 {
            break;
        }
        bytes_read_total += bytes as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if let Some(entry) = parse_assistant_entry(&value, session_id) {
            daemon::send_event(
                &entry.session_id,
                &entry.summary,
                &[],
                entry.tokens,
                Vec::new(),
                git_stdout,
                compact,
                Some(entry.meta),
                Some(root.to_string_lossy().to_string()),
            )?;
            emitted += 1;
        }
    }

    state
        .offsets
        .insert(session_id.to_string(), bytes_read_total);

    Ok(emitted)
}

struct AssistantEntry {
    session_id: String,
    summary: String,
    tokens: Option<TokenUsage>,
    meta: Value,
}

fn parse_assistant_entry(value: &Value, fallback_session_id: &str) -> Option<AssistantEntry> {
    let entry_type = value.get("type").and_then(|v| v.as_str());
    let message = value.get("message")?;
    let role = message.get("role").and_then(|v| v.as_str());
    if entry_type != Some("assistant") && role != Some("assistant") {
        return None;
    }

    let mut text_parts: Vec<String> = Vec::new();
    let mut content_types: Vec<String> = Vec::new();
    let mut tool_use_names: Vec<String> = Vec::new();

    if let Some(content) = message.get("content") {
        if let Some(items) = content.as_array() {
            for item in items {
                if let Some(item_type) = item.get("type").and_then(|v| v.as_str()) {
                    content_types.push(item_type.to_string());
                    if item_type == "text" {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    } else if item_type == "tool_use" {
                        if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                            tool_use_names.push(name.to_string());
                        }
                    }
                }
            }
        } else if let Some(text) = content.as_str() {
            content_types.push("text".to_string());
            text_parts.push(text.to_string());
        }
    }

    let text = text_parts.join("\n");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let summary = trimmed
        .lines()
        .next()
        .unwrap_or("Claude response")
        .trim()
        .to_string();

    let tokens = tokens_from_message(message);
    let session_id = value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_session_id)
        .to_string();

    let meta = json!({
        "source": "claude",
        "session_id": session_id.clone(),
        "entry_uuid": value.get("uuid").and_then(|v| v.as_str()),
        "message_id": message.get("id").and_then(|v| v.as_str()),
        "request_id": value.get("requestId").and_then(|v| v.as_str()),
        "timestamp": value.get("timestamp").and_then(|v| v.as_str()),
        "cwd": value.get("cwd").and_then(|v| v.as_str()),
        "git_branch": value.get("gitBranch").and_then(|v| v.as_str()),
        "version": value.get("version").and_then(|v| v.as_str()),
        "slug": value.get("slug").and_then(|v| v.as_str()),
        "model": message.get("model").and_then(|v| v.as_str()),
        "is_api_error": value.get("isApiErrorMessage").and_then(|v| v.as_bool()),
        "content_types": content_types,
        "tool_use_names": tool_use_names,
        "text": text,
    });

    Some(AssistantEntry {
        session_id,
        summary,
        tokens,
        meta,
    })
}

fn tokens_from_message(message: &Value) -> Option<TokenUsage> {
    let usage = message.get("usage")?;
    let input = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    Some(TokenUsage {
        input,
        output,
        total: input + output,
    })
}

fn state_path(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(".git").join("gg");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join("claude.json"))
}

fn load_state(root: &Path) -> ClaudeState {
    let path = match state_path(root) {
        Ok(path) => path,
        Err(_) => return ClaudeState::default(),
    };
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return ClaudeState::default(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_state(root: &Path, state: &ClaudeState) -> Result<(), String> {
    let path = state_path(root)?;
    let data = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}
