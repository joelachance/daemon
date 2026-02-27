use crate::daemon;
use crate::session::TokenUsage;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::OffsetDateTime;

const DEFAULT_DB_PATH: &str = "/Users/joe/.local/share/opencode/opencode.db";

#[derive(Debug, Clone)]
struct OpenCodeRow {
    session_id: String,
    session_title: String,
    session_directory: String,
    project_id: String,
    message_id: String,
    message_data: String,
    part_id: String,
    part_time_created: i64,
    part_data: String,
}

#[derive(Debug, Clone)]
struct OpenCodeAssistantPart {
    session_id: String,
    session_title: String,
    session_directory: String,
    project_id: String,
    message_id: String,
    part_id: String,
    part_time_created: i64,
    text: String,
    prompt_text: Option<String>,
    message_json: Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OpenCodeState {
    #[serde(default)]
    last_seen: HashMap<String, i64>,
    #[serde(default)]
    explicit_end: HashMap<String, bool>,
    #[serde(default)]
    soft_end: HashMap<String, bool>,
}

pub fn poll_assistant_messages(
    root: &Path,
    git_stdout: bool,
    compact: bool,
) -> Result<usize, String> {
    let root = root.canonicalize().map_err(|err| err.to_string())?;
    let root_str = root.to_string_lossy().to_string();
    let mut state = load_state(&root);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let timeout_secs = env::var("GG_OPENCODE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(600);

    let conn = match open_opencode_db()? {
        Some(conn) => conn,
        None => return Ok(0),
    };
    let mut parts = fetch_assistant_parts_for_repo(&conn, &root_str)?;
    parts.sort_by_key(|part| part.part_time_created);

    let mut emitted = 0usize;
    for part in parts {
        let part_time = to_seconds(part.part_time_created);
        let last_seen = state.last_seen.get(&part.session_id).copied().unwrap_or(0);
        if part_time <= last_seen {
            continue;
        }

        let summary =
            summarize_text(&part.text).unwrap_or_else(|| "OpenCode assistant response".to_string());
        let tokens = parse_tokens(&part.message_json);

        let meta = json!({
            "source": "opencode",
            "session_id": &part.session_id,
            "session_title": &part.session_title,
            "session_directory": &part.session_directory,
            "project_id": &part.project_id,
            "message_id": &part.message_id,
            "part_id": &part.part_id,
            "part_time_created": part.part_time_created,
            "role": "assistant",
            "prompt": {
                "text": &part.prompt_text,
            },
            "response": {
                "text": &part.text,
            },
            "model_id": part.message_json.get("modelID"),
            "provider_id": part.message_json.get("providerID"),
            "mode": part.message_json.get("mode"),
            "agent": part.message_json.get("agent"),
            "path": part.message_json.get("path"),
            "finish": part.message_json.get("finish"),
        });

        daemon::send_event(
            &part.session_id,
            &summary,
            &[],
            tokens,
            Vec::new(),
            git_stdout,
            compact,
            Some(meta),
            Some(root_str.clone()),
        )?;

        state.last_seen.insert(part.session_id.clone(), part_time);
        state.explicit_end.remove(&part.session_id);
        state.soft_end.remove(&part.session_id);
        save_state(&root, &state)?;
        emitted += 1;

        if is_exit_prompt(part.prompt_text.as_deref())
            && !state
                .explicit_end
                .get(&part.session_id)
                .copied()
                .unwrap_or(false)
        {
            let meta = json!({
                "source": "opencode",
                "session_id": &part.session_id,
                "end": true,
                "exit_command": true,
            });
            daemon::send_event(
                &part.session_id,
                "session end",
                &[],
                None,
                Vec::new(),
                git_stdout,
                compact,
                Some(meta),
                Some(root_str.clone()),
            )?;
            state.explicit_end.insert(part.session_id.clone(), true);
            save_state(&root, &state)?;
        }
    }

    if timeout_secs > 0 {
        let session_ids: Vec<String> = state.last_seen.keys().cloned().collect();
        for session_id in session_ids {
            if state
                .explicit_end
                .get(&session_id)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            if state.soft_end.get(&session_id).copied().unwrap_or(false) {
                continue;
            }
            let last_seen = state.last_seen.get(&session_id).copied().unwrap_or(0);
            if last_seen == 0 {
                continue;
            }
            if now - last_seen >= timeout_secs {
                let meta = json!({
                    "source": "opencode",
                    "session_id": &session_id,
                    "soft_end": true,
                    "timeout_secs": timeout_secs,
                });
                daemon::send_event(
                    &session_id,
                    "session timeout",
                    &[],
                    None,
                    Vec::new(),
                    git_stdout,
                    compact,
                    Some(meta),
                    Some(root_str.clone()),
                )?;
                state.soft_end.insert(session_id, true);
                save_state(&root, &state)?;
                emitted += 1;
            }
        }
    }

    Ok(emitted)
}

fn open_opencode_db() -> Result<Option<Connection>, String> {
    let db_path = env::var("GG_OPENCODE_DB")
        .ok()
        .or_else(|| env::var("OPENCODE_DB").ok())
        .unwrap_or_else(|| DEFAULT_DB_PATH.to_string());
    if !Path::new(&db_path).exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| err.to_string())?;
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|err| err.to_string())?;
    Ok(Some(conn))
}

fn fetch_assistant_parts_for_repo(
    conn: &Connection,
    root: &str,
) -> Result<Vec<OpenCodeAssistantPart>, String> {
    let mut prompt_stmt = conn
        .prepare_cached(
            "select json_extract(p.data, '$.text') \
             from part p \
             join message m on p.message_id = m.id \
             where m.session_id = ?1 \
               and json_extract(m.data, '$.role') = 'user' \
               and json_extract(p.data, '$.type') = 'text' \
               and p.time_created <= ?2 \
             order by p.time_created desc \
             limit 1",
        )
        .map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "select s.id, s.title, s.directory, s.project_id, m.id, m.data, p.id, p.time_created, p.data \
             from session s \
             join project pr on pr.id = s.project_id \
             join message m on m.session_id = s.id \
             join part p on p.message_id = m.id \
             where s.directory = ?1 or pr.worktree = ?1",
        )
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map([root], |row| {
            Ok(OpenCodeRow {
                session_id: row.get(0)?,
                session_title: row.get(1)?,
                session_directory: row.get(2)?,
                project_id: row.get(3)?,
                message_id: row.get(4)?,
                message_data: row.get(5)?,
                part_id: row.get(6)?,
                part_time_created: row.get(7)?,
                part_data: row.get(8)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut parts = Vec::new();
    for row in rows {
        let row = row.map_err(|err| err.to_string())?;
        let message_json: Value = match serde_json::from_str(&row.message_data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if message_json.get("role").and_then(|value| value.as_str()) != Some("assistant") {
            continue;
        }

        let part_json: Value = match serde_json::from_str(&row.part_data) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if part_json.get("type").and_then(|value| value.as_str()) != Some("text") {
            continue;
        }
        let text = match part_json.get("text").and_then(|value| value.as_str()) {
            Some(value) => value.to_string(),
            None => continue,
        };
        let prompt_text: Option<String> = prompt_stmt
            .query_row(
                rusqlite::params![&row.session_id, row.part_time_created],
                |prompt_row| prompt_row.get(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;

        parts.push(OpenCodeAssistantPart {
            session_id: row.session_id,
            session_title: row.session_title,
            session_directory: row.session_directory,
            project_id: row.project_id,
            message_id: row.message_id,
            part_id: row.part_id,
            part_time_created: row.part_time_created,
            text,
            prompt_text,
            message_json,
        });
    }

    Ok(parts)
}

fn parse_tokens(message_json: &Value) -> Option<TokenUsage> {
    let tokens = message_json.get("tokens")?;
    let input = tokens.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
    let output = tokens.get("output").and_then(|v| v.as_i64()).unwrap_or(0);
    let total = tokens.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
    if input == 0 && output == 0 && total == 0 {
        return None;
    }
    let total = if total == 0 { input + output } else { total };
    Some(TokenUsage {
        input: input as u64,
        output: output as u64,
        total: total as u64,
    })
}

fn summarize_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return None;
    }
    let max_len = 120usize;
    let mut summary = first_line.to_string();
    if summary.chars().count() > max_len {
        summary = summary.chars().take(max_len).collect::<String>();
        summary.push_str("...");
    }
    Some(summary)
}

fn is_exit_prompt(text: Option<&str>) -> bool {
    let text = match text {
        Some(value) => value,
        None => return false,
    };
    text.lines()
        .any(|line| line.trim().eq_ignore_ascii_case("/exit"))
}

fn to_seconds(value: i64) -> i64 {
    if value > 1_000_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn state_path(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(".git").join("gg");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join("opencode.json"))
}

fn load_state(root: &Path) -> OpenCodeState {
    let path = match state_path(root) {
        Ok(path) => path,
        Err(_) => return OpenCodeState::default(),
    };
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return OpenCodeState::default(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_state(root: &Path, state: &OpenCodeState) -> Result<(), String> {
    let path = state_path(root)?;
    let data = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}
