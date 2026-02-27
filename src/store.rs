use crate::git;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub last_event: Option<String>,
    pub event_count: usize,
    pub end_status: Option<EndStatus>,
    pub display_name: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndStatus {
    Explicit,
    Soft,
}

#[derive(Debug, Clone)]
pub struct SessionCommit {
    pub commit: String,
    pub summary: String,
}

#[derive(Debug, Deserialize)]
struct SessionEventRecord {
    commit: Option<String>,
    summary: Option<String>,
    timestamp: Option<String>,
    meta: Option<Value>,
}

pub fn write_event(root: &str, session_id: &str, payload: &str) -> Result<(), String> {
    let events_dir = session_events_dir(root, session_id)?;
    fs::create_dir_all(&events_dir).map_err(|err| err.to_string())?;
    let filename = event_filename(payload).unwrap_or_else(|| format!("event-{}.json", now_ts()));
    let path = events_dir.join(filename);
    fs::write(path, payload).map_err(|err| err.to_string())
}

pub fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    let root = git::repo_root()?;
    let sessions_dir = sessions_root(&root);
    let entries = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    let mut sessions = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        if session_id.trim().is_empty() {
            continue;
        }
        let (event_count, last_event, end_status, display_name, source) =
            session_event_stats(&root, &session_id)?;
        sessions.push(SessionInfo {
            id: session_id,
            last_event,
            event_count,
            end_status,
            display_name,
            source,
        });
    }

    sessions.sort_by(|a, b| b.last_event.cmp(&a.last_event));
    Ok(sessions)
}

pub fn session_ended(session_id: &str) -> Result<bool, String> {
    let root = git::repo_root()?;
    let events = list_event_files(&root, session_id)?;
    let last = match events.last() {
        Some(value) => value,
        None => return Ok(false),
    };
    let payload = fs::read_to_string(last).map_err(|err| err.to_string())?;
    let record: SessionEventRecord =
        serde_json::from_str(&payload).map_err(|err| err.to_string())?;
    Ok(matches!(
        extract_end_status(record.meta.as_ref()),
        Some(EndStatus::Explicit | EndStatus::Soft)
    ))
}

pub fn session_end_status(session_id: &str) -> Result<Option<EndStatus>, String> {
    let root = git::repo_root()?;
    let events = list_event_files(&root, session_id)?;
    let last = match events.last() {
        Some(value) => value,
        None => return Ok(None),
    };
    let payload = fs::read_to_string(last).map_err(|err| err.to_string())?;
    let record: SessionEventRecord =
        serde_json::from_str(&payload).map_err(|err| err.to_string())?;
    Ok(extract_end_status(record.meta.as_ref()))
}

pub fn list_session_commits(session_id: &str) -> Result<Vec<SessionCommit>, String> {
    let root = git::repo_root()?;
    let events = list_event_files(&root, session_id)?;
    let mut seen = HashSet::new();
    let mut commits = Vec::new();

    for event_path in events {
        let payload = fs::read_to_string(&event_path).map_err(|err| err.to_string())?;
        let record: SessionEventRecord =
            serde_json::from_str(&payload).map_err(|err| err.to_string())?;
        let commit_hash = match record.commit {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => continue,
        };
        if !seen.insert(commit_hash.clone()) {
            continue;
        }
        let summary = match git::commit_subject(&commit_hash) {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => record
                .summary
                .unwrap_or_else(|| "commit".to_string())
                .trim()
                .to_string(),
        };
        commits.push(SessionCommit {
            commit: commit_hash,
            summary,
        });
    }

    Ok(commits)
}

fn sessions_root(root: &str) -> PathBuf {
    Path::new(root).join(".git").join("gg").join("sessions")
}

fn session_events_dir(root: &str, session_id: &str) -> Result<PathBuf, String> {
    if session_id.trim().is_empty() {
        return Err("missing session id".to_string());
    }
    Ok(sessions_root(root).join(session_id).join("events"))
}

fn session_event_stats(
    root: &str,
    session_id: &str,
) -> Result<
    (
        usize,
        Option<String>,
        Option<EndStatus>,
        Option<String>,
        Option<String>,
    ),
    String,
> {
    let events = list_event_files(root, session_id)?;
    let event_count = events.len();
    let (last_event, end_status, display_name, source) = match events.last() {
        Some(path) => {
            let payload = fs::read_to_string(path).map_err(|err| err.to_string())?;
            let record: SessionEventRecord =
                serde_json::from_str(&payload).map_err(|err| err.to_string())?;
            let end_status = extract_end_status(record.meta.as_ref());
            let display_name = extract_session_name(record.meta.as_ref());
            let source = extract_source(record.meta.as_ref());
            let timestamp = record.timestamp.or_else(|| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });
            (timestamp, end_status, display_name, source)
        }
        None => (None, None, None, None),
    };
    Ok((event_count, last_event, end_status, display_name, source))
}

fn list_event_files(root: &str, session_id: &str) -> Result<Vec<PathBuf>, String> {
    let events_dir = session_events_dir(root, session_id)?;
    let entries = match fs::read_dir(&events_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    let mut events = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        events.push(path);
    }
    events.sort();
    Ok(events)
}

fn event_filename(payload: &str) -> Option<String> {
    let record: SessionEventRecord = serde_json::from_str(payload).ok()?;
    let timestamp = record.timestamp?.trim().to_string();
    if timestamp.is_empty() {
        return None;
    }
    let safe = timestamp.replace(':', "-");
    Some(format!("{safe}.json"))
}

fn now_ts() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn extract_end_status(meta: Option<&Value>) -> Option<EndStatus> {
    let meta = meta?;
    if meta
        .get("end")
        .and_then(|val| val.as_bool())
        .unwrap_or(false)
    {
        return Some(EndStatus::Explicit);
    }
    if meta
        .get("soft_end")
        .and_then(|val| val.as_bool())
        .unwrap_or(false)
    {
        return Some(EndStatus::Soft);
    }
    None
}

fn extract_source(meta: Option<&Value>) -> Option<String> {
    meta.and_then(|value| value.get("source"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn extract_session_name(meta: Option<&Value>) -> Option<String> {
    let meta = meta?;
    for key in ["name", "session_title", "title", "subtitle", "slug"] {
        if let Some(value) = meta.get(key).and_then(|value| value.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(dir) = meta
        .get("session_directory")
        .and_then(|value| value.as_str())
    {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            if let Some(name) = Path::new(trimmed).file_name().and_then(|val| val.to_str()) {
                let name_trimmed = name.trim();
                if !name_trimmed.is_empty() {
                    return Some(name_trimmed.to_string());
                }
            }
        }
    }
    if let Some(dir) = meta.get("path").and_then(|value| value.as_str()) {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            if let Some(name) = Path::new(trimmed).file_name().and_then(|val| val.to_str()) {
                let name_trimmed = name.trim();
                if !name_trimmed.is_empty() {
                    return Some(name_trimmed.to_string());
                }
            }
        }
    }
    if let Some(dir) = meta.get("cwd").and_then(|value| value.as_str()) {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            if let Some(name) = Path::new(trimmed).file_name().and_then(|val| val.to_str()) {
                let name_trimmed = name.trim();
                if !name_trimmed.is_empty() {
                    return Some(name_trimmed.to_string());
                }
            }
        }
    }
    None
}
