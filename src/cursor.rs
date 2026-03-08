use crate::daemon;
use crate::session::TokenUsage;
use crate::store;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use sysinfo::System;
use time::OffsetDateTime;

const DEFAULT_GLOBAL_DB: &str =
    "~/Library/Application Support/Cursor/User/globalStorage/state.vscdb";

#[derive(Debug, Clone)]
struct CursorSession {
    composer_id: String,
    status: String,
    last_updated_at: Option<i64>,
    created_at: Option<i64>,
    name: Option<String>,
    subtitle: Option<String>,
    model_name: Option<String>,
    context_tokens_used: Option<i64>,
    context_token_limit: Option<i64>,
    is_archived: Option<bool>,
    bubble_count: Option<usize>,
    bubble_ids: Vec<String>,
    attached_files: Vec<String>,
}

#[derive(Debug, Default)]
struct CursorFetchResult {
    sessions: Vec<CursorSession>,
    scanned: usize,
    rejected: usize,
    workspace_candidates: usize,
    hydrated_found: usize,
    hydrated_missing: usize,
    fallback_used: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorState {
    #[serde(default)]
    completed: HashMap<String, i64>,
    #[serde(default)]
    last_bubble: HashMap<String, String>,
}

pub fn cursor_running() -> bool {
    let mut system = System::new_all();
    system.refresh_processes();
    system.processes().values().any(|process| {
        let name = process.name().to_ascii_lowercase();
        if name.contains("cursor") {
            return true;
        }
        process
            .cmd()
            .iter()
            .any(|part| part.to_ascii_lowercase().contains("cursor"))
    })
}

pub fn poll_completed_sessions(
    root: &Path,
    git_stdout: bool,
    compact: bool,
) -> Result<usize, String> {
    let root = root.canonicalize().map_err(|err| err.to_string())?;
    let mut state = load_state(&root);
    let conn = open_cursor_db()?;
    let fetch = fetch_sessions_for_repo(&conn, &root)?;
    let sessions = fetch.sessions;
    let mut emitted = 0usize;

    for session in sessions {
        let session_id = session.composer_id.clone();
        daemon::upsert_session_presence(&session_id, "cursor", &root, None)?;
        store::set_session_source_status(&session_id, Some(&session.status))?;
        let seen_at = session
            .last_updated_at
            .unwrap_or_else(|| OffsetDateTime::now_utc().unix_timestamp());
        store::touch_session(&session_id, seen_at)?;
        let mut last_prompt: Option<PromptSnapshot> = None;
        let new_bubbles = bubbles_after(
            state.last_bubble.get(&session_id).map(String::as_str),
            &session.bubble_ids,
        );

        for bubble_id in new_bubbles.iter() {
            let bubble_value = match fetch_bubble_json(&conn, &session_id, bubble_id)? {
                Some(value) => value,
                None => continue,
            };

            let bubble_type = bubble_value.get("type").and_then(|v| v.as_i64());
            let bubble_text = bubble_value.get("text").and_then(|v| v.as_str());
            let bubble_thinking = bubble_value
                .get("thinking")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str());
            let bubble_created_at = bubble_value
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let bubble_id_value = bubble_value
                .get("bubbleId")
                .and_then(|v| v.as_str())
                .unwrap_or(bubble_id)
                .to_string();
            let bubble_rich_text = bubble_value
                .get("richText")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            if bubble_type == Some(1) {
                last_prompt = Some(PromptSnapshot {
                    bubble_id: bubble_id_value,
                    text: bubble_text.map(str::to_string),
                    rich_text: bubble_rich_text,
                    created_at: bubble_created_at,
                });
                continue;
            }

            if bubble_type != Some(2) {
                continue;
            }

            let token_input = bubble_value
                .get("tokenCount")
                .and_then(|v| v.get("inputTokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let token_output = bubble_value
                .get("tokenCount")
                .and_then(|v| v.get("outputTokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let tokens = if token_input > 0 || token_output > 0 {
                Some(TokenUsage {
                    input: token_input as u64,
                    output: token_output as u64,
                    total: (token_input + token_output) as u64,
                })
            } else {
                None
            };

            let tool_former_data = bubble_value.get("toolFormerData").cloned();
            let tool_results = bubble_value.get("toolResults").cloned();
            let tool_name = tool_former_data
                .as_ref()
                .and_then(|value| value.get("name"))
                .and_then(|value| value.as_str());

            let summary = summarize_bubble(bubble_text, bubble_thinking, tool_name);

            let meta = json!({
                "source": "cursor",
                "composer_id": &session_id,
                "status": &session.status,
                "last_updated_at": session.last_updated_at,
                "created_at": session.created_at,
                "name": &session.name,
                "subtitle": &session.subtitle,
                "model_name": &session.model_name,
                "context_tokens_used": session.context_tokens_used,
                "context_token_limit": session.context_token_limit,
                "is_archived": session.is_archived,
                "bubble_count": session.bubble_count,
                "attached_files": &session.attached_files,
                "bubble_id": bubble_id_value,
                "bubble_type": bubble_type,
                "role": "assistant",
                "bubble_created_at": bubble_created_at,
                "prompt": last_prompt.as_ref(),
                "response": {
                    "text": bubble_text,
                    "thinking": bubble_thinking,
                },
                "response_text": bubble_text,
                "response_thinking": bubble_thinking,
                "token_count": bubble_value.get("tokenCount"),
                "toolFormerData": tool_former_data,
                "toolResults": tool_results,
            });

            daemon::send_event(
                &session_id,
                &summary,
                &[],
                tokens,
                Vec::new(),
                git_stdout,
                compact,
                Some(meta),
                Some(root.to_string_lossy().to_string()),
            )?;

            emitted += 1;
        }

        if let Some(last_bubble) = new_bubbles.last() {
            state
                .last_bubble
                .insert(session_id.clone(), last_bubble.clone());
        }

        if session.status == "completed" {
            let last_updated = session.last_updated_at.unwrap_or(0);
            if state
                .completed
                .get(&session_id)
                .map(|previous| *previous < last_updated)
                .unwrap_or(true)
            {
                let summary = session
                    .name
                    .clone()
                    .or(session.subtitle.clone())
                    .unwrap_or_else(|| "Cursor session complete".to_string());

                let tokens = session.context_tokens_used.map(|total| TokenUsage {
                    input: 0,
                    output: 0,
                    total: total as u64,
                });

                let meta = json!({
                    "source": "cursor",
                    "composer_id": &session_id,
                    "status": &session.status,
                    "last_updated_at": session.last_updated_at,
                    "created_at": session.created_at,
                    "name": &session.name,
                    "subtitle": &session.subtitle,
                    "model_name": &session.model_name,
                    "context_tokens_used": session.context_tokens_used,
                    "context_token_limit": session.context_token_limit,
                    "is_archived": session.is_archived,
                    "bubble_count": session.bubble_count,
                    "attached_files": &session.attached_files,
                    "session_complete": true,
                    "end": true,
                });

                daemon::send_event(
                    &session_id,
                    &summary,
                    &[],
                    tokens,
                    Vec::new(),
                    git_stdout,
                    compact,
                    Some(meta),
                    Some(root.to_string_lossy().to_string()),
                )?;

                state.completed.insert(session_id, last_updated);
                emitted += 1;
            }
        }

        save_state(&root, &state)?;
    }

    Ok(emitted)
}

fn open_cursor_db() -> Result<Connection, String> {
    let db_path = env::var("GG_CURSOR_DB").unwrap_or_else(|_| DEFAULT_GLOBAL_DB.to_string());
    let db_path = expand_tilde(&db_path);
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|err| err.to_string())?;
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|err| err.to_string())?;
    Ok(conn)
}

fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = env::var("HOME") {
            let trimmed = path.trim_start_matches('~');
            return format!("{home}{trimmed}");
        }
    }
    path.to_string()
}

fn fetch_sessions_for_repo(conn: &Connection, root: &Path) -> Result<CursorFetchResult, String> {
    let repo_str = root.to_string_lossy().to_string();
    let repo_norm = normalize_path_like(&repo_str);
    let workspace_ids = discover_workspace_composer_ids(root)?;

    let mut result = CursorFetchResult::default();
    result.workspace_candidates = workspace_ids.len();

    if !workspace_ids.is_empty() {
        hydrate_sessions_by_ids(conn, &workspace_ids, &mut result)?;
        if !result.sessions.is_empty() {
            return Ok(result);
        }
    }

    result.fallback_used = true;
    fetch_sessions_by_global_match(conn, &repo_norm, &mut result)?;
    Ok(result)
}

fn hydrate_sessions_by_ids(
    conn: &Connection,
    composer_ids: &[String],
    result: &mut CursorFetchResult,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare("select cast(value as text) from cursorDiskKV where key = ?1")
        .map_err(|err| err.to_string())?;

    for composer_id in composer_ids {
        result.scanned += 1;
        let key = format!("composerData:{composer_id}");
        let payload: Option<String> = stmt
            .query_row([key], |row| row.get(0))
            .optional()
            .map_err(|err| err.to_string())?;
        let payload = match payload {
            Some(value) if !value.trim().is_empty() => value,
            _ => {
                result.hydrated_missing += 1;
                result.rejected += 1;
                continue;
            }
        };
        let json: Value = match serde_json::from_str(&payload) {
            Ok(value) => value,
            Err(_) => {
                result.hydrated_missing += 1;
                result.rejected += 1;
                continue;
            }
        };
        result.hydrated_found += 1;
        result
            .sessions
            .push(build_cursor_session(composer_id, &json));
    }

    Ok(())
}

fn fetch_sessions_by_global_match(
    conn: &Connection,
    repo_norm: &str,
    result: &mut CursorFetchResult,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "select key, cast(value as text) from cursorDiskKV where key like 'composerData:%'",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: Option<String> = row.get(1)?;
            Ok((key, value))
        })
        .map_err(|err| err.to_string())?;

    for row in rows {
        let (key, value) = row.map_err(|err| err.to_string())?;
        result.scanned += 1;
        let value = match value {
            Some(value) => value,
            None => {
                result.rejected += 1;
                continue;
            }
        };
        let composer_id = match key.split_once(':') {
            Some((_, id)) => id.to_string(),
            None => {
                result.rejected += 1;
                continue;
            }
        };
        let json: Value = match serde_json::from_str(&value) {
            Ok(value) => value,
            Err(_) => {
                result.rejected += 1;
                continue;
            }
        };
        if !session_matches_repo(&json, &value, repo_norm) {
            result.rejected += 1;
            continue;
        }
        result
            .sessions
            .push(build_cursor_session(&composer_id, &json));
    }

    Ok(())
}

fn build_cursor_session(composer_id: &str, json: &Value) -> CursorSession {
    let status = json
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let bubble_ids = extract_bubble_ids(json);

    CursorSession {
        composer_id: composer_id.to_string(),
        status,
        last_updated_at: json.get("lastUpdatedAt").and_then(|v| v.as_i64()),
        created_at: json.get("createdAt").and_then(|v| v.as_i64()),
        name: json
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        subtitle: json
            .get("subtitle")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        model_name: json
            .get("modelConfig")
            .and_then(|v| v.get("modelName"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        context_tokens_used: json.get("contextTokensUsed").and_then(|v| v.as_i64()),
        context_token_limit: json.get("contextTokenLimit").and_then(|v| v.as_i64()),
        is_archived: json.get("isArchived").and_then(|v| v.as_bool()),
        bubble_count: Some(bubble_ids.len()),
        bubble_ids,
        attached_files: json
            .get("allAttachedFileCodeChunksUris")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default(),
    }
}

fn fetch_bubble_json(
    conn: &Connection,
    composer_id: &str,
    bubble_id: &str,
) -> Result<Option<Value>, String> {
    let key = format!("bubbleId:{composer_id}:{bubble_id}");
    let mut stmt = conn
        .prepare("select cast(value as text) from cursorDiskKV where key = ?1")
        .map_err(|err| err.to_string())?;
    let row: Option<String> = stmt
        .query_row([key], |row| row.get(0))
        .optional()
        .map_err(|err| err.to_string())?;
    let value = match row {
        Some(value) => value,
        None => return Ok(None),
    };
    let json: Value = serde_json::from_str(&value).map_err(|err| err.to_string())?;
    Ok(Some(json))
}

fn extract_bubble_ids(json: &Value) -> Vec<String> {
    let mut bubble_ids = Vec::new();
    let entries = match json
        .get("fullConversationHeadersOnly")
        .and_then(|v| v.as_array())
    {
        Some(value) => value,
        None => return bubble_ids,
    };

    for entry in entries {
        match entry {
            Value::String(value) => bubble_ids.push(value.to_string()),
            Value::Object(map) => {
                let id = map
                    .get("bubbleId")
                    .or_else(|| map.get("bubble_id"))
                    .or_else(|| map.get("id"))
                    .and_then(|v| v.as_str());
                if let Some(value) = id {
                    bubble_ids.push(value.to_string());
                }
            }
            _ => {}
        }
    }

    bubble_ids
}

fn bubbles_after(last: Option<&str>, bubble_ids: &[String]) -> Vec<String> {
    match last {
        Some(last_id) => match bubble_ids.iter().position(|id| id == last_id) {
            Some(index) => bubble_ids.iter().skip(index + 1).cloned().collect(),
            None => bubble_ids.to_vec(),
        },
        None => bubble_ids.to_vec(),
    }
}

fn summarize_bubble(text: Option<&str>, thinking: Option<&str>, tool_name: Option<&str>) -> String {
    if let Some(value) = text {
        let summary = summarize_text(value);
        if !summary.is_empty() {
            return summary;
        }
    }
    if let Some(value) = thinking {
        let summary = summarize_text(value);
        if !summary.is_empty() {
            return summary;
        }
    }
    if let Some(name) = tool_name {
        return format!("Cursor tool call: {name}");
    }
    "Cursor assistant response".to_string()
}

fn summarize_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    let mut summary: String = first_line.trim().to_string();
    let max_len = 120usize;
    if summary.chars().count() > max_len {
        summary = summary.chars().take(max_len).collect::<String>();
        summary.push_str("...");
    }
    summary
}

#[derive(Debug, Serialize)]
struct PromptSnapshot {
    bubble_id: String,
    text: Option<String>,
    rich_text: Option<String>,
    created_at: Option<String>,
}

fn state_path(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(".git").join("gg");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join("cursor.json"))
}

fn load_state(root: &Path) -> CursorState {
    let path = match state_path(root) {
        Ok(path) => path,
        Err(_) => return CursorState::default(),
    };
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return CursorState::default(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_state(root: &Path, state: &CursorState) -> Result<(), String> {
    let path = state_path(root)?;
    let data = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}

fn discover_workspace_composer_ids(root: &Path) -> Result<Vec<String>, String> {
    let workspace_root = workspace_storage_root()?;
    let entries = match fs::read_dir(&workspace_root) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    let repo_norm = normalize_path_like(&root.to_string_lossy());
    let mut ids = HashSet::new();

    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let ws_dir = entry.path();
        let ws_json = ws_dir.join("workspace.json");
        let ws_db = ws_dir.join("state.vscdb");
        if !ws_json.exists() || !ws_db.exists() {
            continue;
        }

        let workspace_folder = match fs::read_to_string(&ws_json)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|json| {
                json.get("folder")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }) {
            Some(value) => value,
            None => continue,
        };
        if normalize_path_like(&workspace_folder) != repo_norm {
            continue;
        }

        let conn = match Connection::open_with_flags(&ws_db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let raw: Option<String> = conn
            .query_row(
                "select cast(value as text) from ItemTable where key = 'composer.composerData'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        let raw = match raw {
            Some(value) => value,
            None => continue,
        };
        let json: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let all = match json.get("allComposers").and_then(|v| v.as_array()) {
            Some(value) => value,
            None => continue,
        };
        for item in all {
            if let Some(id) = item
                .get("composerId")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
            {
                if !id.trim().is_empty() {
                    ids.insert(id.to_string());
                }
            }
        }
    }

    let mut out: Vec<String> = ids.into_iter().collect();
    out.sort();
    Ok(out)
}

fn workspace_storage_root() -> Result<PathBuf, String> {
    if let Ok(value) = env::var("GG_CURSOR_WORKSPACE_STORAGE") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(expand_tilde(&value)));
        }
    }
    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Cursor")
        .join("User")
        .join("workspaceStorage"))
}

fn session_matches_repo(json: &Value, raw: &str, repo_norm: &str) -> bool {
    if repo_norm.is_empty() {
        return false;
    }
    if string_matches_repo(raw, repo_norm) {
        return true;
    }
    let mut candidates = Vec::new();
    collect_json_strings(json, &mut candidates);
    candidates
        .iter()
        .any(|candidate| string_matches_repo(candidate, repo_norm))
}

fn collect_json_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !text.trim().is_empty() {
                out.push(text.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_json_strings(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_json_strings(value, out);
            }
        }
        _ => {}
    }
}

fn string_matches_repo(candidate: &str, repo_norm: &str) -> bool {
    let normalized = normalize_path_like(candidate);
    if normalized.is_empty() {
        return false;
    }
    if normalized == repo_norm {
        return true;
    }
    let prefix = format!("{repo_norm}/");
    if normalized.starts_with(&prefix) {
        return true;
    }
    let normalized_lower = normalized.to_ascii_lowercase();
    let repo_lower = repo_norm.to_ascii_lowercase();
    if normalized_lower == repo_lower {
        return true;
    }
    let prefix_lower = format!("{repo_lower}/");
    if normalized_lower.starts_with(&prefix_lower) {
        return true;
    }
    false
}

fn normalize_path_like(value: &str) -> String {
    let mut text = value.trim().to_string();
    if text.is_empty() {
        return String::new();
    }
    if let Some(stripped) = text.strip_prefix("file://") {
        text = stripped.to_string();
    }
    if let Some((left, _)) = text.split_once('?') {
        text = left.to_string();
    }
    if let Some((left, _)) = text.split_once('#') {
        text = left.to_string();
    }
    text = text.replace("%20", " ").replace('\\', "/");
    while text.ends_with('/') && text.len() > 1 {
        text.pop();
    }
    text
}
