use crate::session::{Change, ChangeLineRange, DraftCommit, DraftStatus, ToolCall};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub ide: String,
    pub repo_path: String,
    pub base_commit_sha: String,
    pub suggested_branch: String,
    pub confirmed_branch: Option<String>,
    pub ticket: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

pub fn init() -> Result<(), String> {
    let conn = open()?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            ide TEXT NOT NULL,
            repo_path TEXT NOT NULL,
            base_commit_sha TEXT NOT NULL,
            suggested_branch TEXT NOT NULL,
            confirmed_branch TEXT,
            ticket TEXT,
            first_prompt TEXT,
            diff_snapshot_u0 TEXT NOT NULL DEFAULT '',
            started_at INTEGER NOT NULL,
            ended_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS turns (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            prompt TEXT NOT NULL,
            response TEXT NOT NULL,
            tool_calls_json TEXT NOT NULL,
            timestamp INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS changes (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            prompt_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            base_commit_sha TEXT NOT NULL,
            diff TEXT NOT NULL,
            old_start INTEGER NOT NULL,
            old_count INTEGER NOT NULL,
            new_start INTEGER NOT NULL,
            new_count INTEGER NOT NULL,
            change_type TEXT NOT NULL DEFAULT 'edit',
            captured_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS drafts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            message TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            draft_order INTEGER NOT NULL,
            auto_approved INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS draft_changes (
            draft_id TEXT NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
            change_id TEXT NOT NULL REFERENCES changes(id) ON DELETE CASCADE,
            item_order INTEGER NOT NULL,
            PRIMARY KEY (draft_id, change_id)
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_repo_path ON sessions(repo_path);
        CREATE INDEX IF NOT EXISTS idx_turns_session_ts ON turns(session_id, timestamp);
        CREATE INDEX IF NOT EXISTS idx_changes_session_capture ON changes(session_id, captured_at);
        CREATE INDEX IF NOT EXISTS idx_drafts_session_order ON drafts(session_id, draft_order);
        "#,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn open() -> Result<Connection, String> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    Ok(conn)
}

pub fn db_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("VIBE_DB_PATH") {
        if !path.trim().is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    let home = env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home).join(".vibe-commits").join("db.sqlite"))
}

pub fn upsert_session(
    session_id: &str,
    ide: &str,
    repo_path: &str,
    base_commit_sha: &str,
    suggested_branch: &str,
    first_prompt: Option<&str>,
) -> Result<(), String> {
    let now = now_ts();
    let conn = open()?;
    conn.execute(
        "INSERT INTO sessions (id, ide, repo_path, base_commit_sha, suggested_branch, first_prompt, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            ide = excluded.ide,
            repo_path = excluded.repo_path,
            base_commit_sha = excluded.base_commit_sha,
            suggested_branch = excluded.suggested_branch,
            first_prompt = COALESCE(sessions.first_prompt, excluded.first_prompt)",
        params![
            session_id,
            ide,
            repo_path,
            base_commit_sha,
            suggested_branch,
            first_prompt.unwrap_or(""),
            now
        ],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn set_session_branch(session_id: &str, branch: &str) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE sessions SET confirmed_branch = ?2 WHERE id = ?1",
        params![session_id, branch],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn set_session_ticket(session_id: &str, ticket: Option<&str>) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE sessions SET ticket = ?2 WHERE id = ?1",
        params![session_id, ticket.unwrap_or("")],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn mark_session_ended(session_id: &str) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
        params![session_id, now_ts()],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn insert_turn(
    turn_id: &str,
    session_id: &str,
    prompt: &str,
    response: &str,
    tool_calls: &[ToolCall],
) -> Result<(), String> {
    let conn = open()?;
    let tool_json = serde_json::to_string(tool_calls).map_err(|err| err.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO turns (id, session_id, prompt, response, tool_calls_json, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![turn_id, session_id, prompt, response, tool_json, now_ts()],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn get_last_snapshot(session_id: &str) -> Result<String, String> {
    let conn = open()?;
    let value = conn
        .query_row(
            "SELECT diff_snapshot_u0 FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<usize, String>(0),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    Ok(value.unwrap_or_default())
}

pub fn set_last_snapshot(session_id: &str, snapshot: &str) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE sessions SET diff_snapshot_u0 = ?2 WHERE id = ?1",
        params![session_id, snapshot],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn insert_change(change: &Change) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "INSERT OR IGNORE INTO changes
         (id, session_id, prompt_id, file_path, base_commit_sha, diff, old_start, old_count, new_start, new_count, change_type, captured_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            change.id,
            change.session_id,
            change.prompt_id,
            change.file_path,
            change.base_commit_sha,
            change.diff,
            change.line_range.old_start,
            change.line_range.old_count,
            change.line_range.new_start,
            change.line_range.new_count,
            change.change_type,
            change.captured_at
        ],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

#[allow(dead_code)]
pub fn list_changes_for_turn(turn_id: &str) -> Result<Vec<Change>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, prompt_id, file_path, base_commit_sha, diff, old_start, old_count, new_start, new_count, change_type, captured_at
             FROM changes WHERE prompt_id = ?1 ORDER BY captured_at ASC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![turn_id], |row| {
            Ok(Change {
                id: row.get(0)?,
                session_id: row.get(1)?,
                prompt_id: row.get(2)?,
                file_path: row.get(3)?,
                base_commit_sha: row.get(4)?,
                diff: row.get(5)?,
                line_range: ChangeLineRange {
                    old_start: row.get(6)?,
                    old_count: row.get(7)?,
                    new_start: row.get(8)?,
                    new_count: row.get(9)?,
                },
                change_type: row.get(10)?,
                captured_at: row.get(11)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

pub fn create_draft(
    draft_id: &str,
    session_id: &str,
    message: &str,
    auto_approved: bool,
) -> Result<(), String> {
    let conn = open()?;
    let order = next_draft_order_with_conn(&conn, session_id)?;
    conn.execute(
        "INSERT INTO drafts (id, session_id, message, status, created_at, draft_order, auto_approved)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            draft_id,
            session_id,
            message,
            DraftStatus::Draft.as_str(),
            now_ts(),
            order,
            if auto_approved { 1 } else { 0 }
        ],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn add_change_to_draft(draft_id: &str, change_id: &str) -> Result<(), String> {
    let conn = open()?;
    let order = next_draft_change_order_with_conn(&conn, draft_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO draft_changes (draft_id, change_id, item_order) VALUES (?1, ?2, ?3)",
        params![draft_id, change_id, order],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn list_drafts(session_id: &str) -> Result<Vec<DraftCommit>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, message, status, created_at, draft_order, auto_approved
             FROM drafts WHERE session_id = ?1 ORDER BY draft_order ASC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            let status: String = row.get(3)?;
            Ok(DraftCommit {
                id: row.get(0)?,
                session_id: row.get(1)?,
                message: row.get(2)?,
                status: match status.as_str() {
                    "approved" => DraftStatus::Approved,
                    "rejected" => DraftStatus::Rejected,
                    _ => DraftStatus::Draft,
                },
                created_at: row.get(4)?,
                order: row.get(5)?,
                auto_approved: row.get::<usize, i64>(6)? == 1,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

pub fn draft_change_ids(draft_id: &str) -> Result<Vec<String>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT change_id FROM draft_changes WHERE draft_id = ?1 ORDER BY item_order ASC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![draft_id], |row| row.get::<usize, String>(0))
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

pub fn get_change(change_id: &str) -> Result<Option<Change>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, prompt_id, file_path, base_commit_sha, diff, old_start, old_count, new_start, new_count, change_type, captured_at
             FROM changes WHERE id = ?1",
        )
        .map_err(|err| err.to_string())?;
    let value = stmt
        .query_row(params![change_id], |row| {
            Ok(Change {
                id: row.get(0)?,
                session_id: row.get(1)?,
                prompt_id: row.get(2)?,
                file_path: row.get(3)?,
                base_commit_sha: row.get(4)?,
                diff: row.get(5)?,
                line_range: ChangeLineRange {
                    old_start: row.get(6)?,
                    old_count: row.get(7)?,
                    new_start: row.get(8)?,
                    new_count: row.get(9)?,
                },
                change_type: row.get(10)?,
                captured_at: row.get(11)?,
            })
        })
        .optional()
        .map_err(|err| err.to_string())?;
    Ok(value)
}

#[allow(dead_code)]
pub fn update_draft_message(draft_id: &str, message: &str) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE drafts SET message = ?2 WHERE id = ?1",
        params![draft_id, message],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn update_draft_status(draft_id: &str, status: DraftStatus) -> Result<(), String> {
    let conn = open()?;
    conn.execute(
        "UPDATE drafts SET status = ?2 WHERE id = ?1",
        params![draft_id, status.as_str()],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn list_unassigned_changes(session_id: &str) -> Result<Vec<Change>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.session_id, c.prompt_id, c.file_path, c.base_commit_sha, c.diff, c.old_start, c.old_count, c.new_start, c.new_count, c.change_type, c.captured_at
             FROM changes c
             WHERE c.session_id = ?1
               AND NOT EXISTS (SELECT 1 FROM draft_changes dc WHERE dc.change_id = c.id)
             ORDER BY c.captured_at ASC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(Change {
                id: row.get(0)?,
                session_id: row.get(1)?,
                prompt_id: row.get(2)?,
                file_path: row.get(3)?,
                base_commit_sha: row.get(4)?,
                diff: row.get(5)?,
                line_range: ChangeLineRange {
                    old_start: row.get(6)?,
                    old_count: row.get(7)?,
                    new_start: row.get(8)?,
                    new_count: row.get(9)?,
                },
                change_type: row.get(10)?,
                captured_at: row.get(11)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

pub fn list_sessions_for_repo(repo_path: &str) -> Result<Vec<SessionInfo>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, ide, repo_path, base_commit_sha, suggested_branch, confirmed_branch, ticket, started_at, ended_at
             FROM sessions WHERE repo_path = ?1 ORDER BY started_at DESC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![repo_path], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                ide: row.get(1)?,
                repo_path: row.get(2)?,
                base_commit_sha: row.get(3)?,
                suggested_branch: row.get(4)?,
                confirmed_branch: row.get(5)?,
                ticket: normalize_optional(row.get::<usize, String>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
    Ok(out)
}

pub fn get_session(session_id: &str) -> Result<Option<SessionInfo>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, ide, repo_path, base_commit_sha, suggested_branch, confirmed_branch, ticket, started_at, ended_at
             FROM sessions WHERE id = ?1",
        )
        .map_err(|err| err.to_string())?;
    let value = stmt
        .query_row(params![session_id], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                ide: row.get(1)?,
                repo_path: row.get(2)?,
                base_commit_sha: row.get(3)?,
                suggested_branch: row.get(4)?,
                confirmed_branch: row.get(5)?,
                ticket: normalize_optional(row.get::<usize, String>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
            })
        })
        .optional()
        .map_err(|err| err.to_string())?;
    Ok(value)
}

fn next_draft_order_with_conn(conn: &Connection, session_id: &str) -> Result<i64, String> {
    let max_order = conn
        .query_row(
            "SELECT COALESCE(MAX(draft_order), 0) FROM drafts WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<usize, i64>(0),
        )
        .map_err(|err| err.to_string())?;
    Ok(max_order + 1)
}

fn next_draft_change_order_with_conn(conn: &Connection, draft_id: &str) -> Result<i64, String> {
    let max_order = conn
        .query_row(
            "SELECT COALESCE(MAX(item_order), 0) FROM draft_changes WHERE draft_id = ?1",
            params![draft_id],
            |row| row.get::<usize, i64>(0),
        )
        .map_err(|err| err.to_string())?;
    Ok(max_order + 1)
}

fn normalize_optional(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn now_ts() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}
