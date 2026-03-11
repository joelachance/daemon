use crate::path;
use crate::session::{Change, ChangeLineRange, DraftCommit, DraftStatus, ToolCall};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use time::OffsetDateTime;

static CONN: Mutex<Option<Connection>> = Mutex::new(None);

fn with_conn<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&Connection) -> Result<T, String>,
{
    let mut guard = CONN.lock().unwrap();
    if guard.is_none() {
        *guard = Some(open_inner()?);
    }
    f(guard.as_ref().unwrap())
}

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
    pub last_seen_at: Option<i64>,
    pub source_status: Option<String>,
}

pub fn init() -> Result<(), String> {
    with_conn(|conn| {
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
            last_seen_at INTEGER,
            source_status TEXT,
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
        CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT);
        "#,
    )
    .map_err(|err| err.to_string())?;
        ensure_sessions_columns(conn)?;
        migrate_normalize_repo_paths(conn)?;
        Ok(())
    })
}

fn open_inner() -> Result<Connection, String> {
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

fn refresh_signal_path() -> Result<PathBuf, String> {
    db_path().map(|p| {
        p.parent()
            .map(|parent| parent.join("sessions_updated"))
            .unwrap_or_else(|| p.with_file_name("sessions_updated"))
    })
}

pub fn touch_refresh_signal() {
    let path = match refresh_signal_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, OffsetDateTime::now_utc().unix_timestamp().to_string());
}

pub fn refresh_signal_mtime() -> Option<SystemTime> {
    let path = refresh_signal_path().ok()?;
    fs::metadata(&path).ok()?.modified().ok()
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
    let repo_path = path::normalize_repo_path(repo_path);
    with_conn(|conn| {
        conn.execute(
        "INSERT INTO sessions (id, ide, repo_path, base_commit_sha, suggested_branch, first_prompt, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            ide = excluded.ide,
            repo_path = excluded.repo_path,
            base_commit_sha = excluded.base_commit_sha,
            suggested_branch = excluded.suggested_branch,
            first_prompt = COALESCE(sessions.first_prompt, excluded.first_prompt),
            last_seen_at = ?8,
            ended_at = NULL",
        params![
            session_id,
            ide,
            repo_path,
            base_commit_sha,
            suggested_branch,
            first_prompt.unwrap_or(""),
            now,
            now
        ],
    )
    .map_err(|err| err.to_string())?;
        Ok(())
    })?;
    touch_refresh_signal();
    Ok(())
}

pub fn touch_session(session_id: &str, seen_at: i64) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET last_seen_at = ?2 WHERE id = ?1",
            params![session_id, seen_at],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })?;
    touch_refresh_signal();
    Ok(())
}

pub fn set_session_source_status(session_id: &str, status: Option<&str>) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET source_status = ?2 WHERE id = ?1",
            params![session_id, normalize_optional(status.map(str::to_string))],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })?;
    touch_refresh_signal();
    Ok(())
}

pub fn set_session_branch(session_id: &str, branch: &str) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET confirmed_branch = ?2 WHERE id = ?1",
            params![session_id, branch],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn set_session_ticket(session_id: &str, ticket: Option<&str>) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET ticket = ?2 WHERE id = ?1",
            params![session_id, ticket.unwrap_or("")],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn mark_session_ended(session_id: &str) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            params![session_id, now_ts()],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })?;
    touch_refresh_signal();
    Ok(())
}

pub fn insert_turn(
    turn_id: &str,
    session_id: &str,
    prompt: &str,
    response: &str,
    tool_calls: &[ToolCall],
) -> Result<(), String> {
    let tool_json = serde_json::to_string(tool_calls).map_err(|err| err.to_string())?;
    with_conn(|conn| {
        conn.execute(
        "INSERT OR REPLACE INTO turns (id, session_id, prompt, response, tool_calls_json, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![turn_id, session_id, prompt, response, tool_json, now_ts()],
    )
    .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn get_last_snapshot(session_id: &str) -> Result<String, String> {
    with_conn(|conn| {
        let value = conn
            .query_row(
                "SELECT diff_snapshot_u0 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<usize, String>(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        Ok(value.unwrap_or_default())
    })
}

pub fn set_last_snapshot(session_id: &str, snapshot: &str) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET diff_snapshot_u0 = ?2 WHERE id = ?1",
            params![session_id, snapshot],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn insert_change(change: &Change) -> Result<(), String> {
    with_conn(|conn| {
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
    })
}

pub fn create_draft(
    draft_id: &str,
    session_id: &str,
    message: &str,
    auto_approved: bool,
) -> Result<(), String> {
    with_conn(|conn| {
        let order = next_draft_order_with_conn(conn, session_id)?;
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
    })
}

/// Returns true if the change is already assigned to any draft.
pub fn change_already_assigned(change_id: &str) -> Result<bool, String> {
    with_conn(|conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM draft_changes WHERE change_id = ?1",
                params![change_id],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;
        Ok(count > 0)
    })
}

pub fn add_change_to_draft(draft_id: &str, change_id: &str) -> Result<(), String> {
    with_conn(|conn| {
        let order = next_draft_change_order_with_conn(conn, draft_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO draft_changes (draft_id, change_id, item_order) VALUES (?1, ?2, ?3)",
        params![draft_id, change_id, order],
    )
    .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn list_drafts(session_id: &str) -> Result<Vec<DraftCommit>, String> {
    with_conn(|conn| {
        let mut stmt = conn
        .prepare(
            "SELECT id, session_id, message, status, created_at, draft_order, auto_approved
             FROM drafts d
             WHERE d.session_id = ?1
               AND EXISTS (SELECT 1 FROM draft_changes dc WHERE dc.draft_id = d.id)
             ORDER BY d.draft_order ASC",
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
    })
}

pub fn delete_drafts_for_session(session_id: &str) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute("DELETE FROM drafts WHERE session_id = ?1", params![session_id])
            .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn draft_change_ids(draft_id: &str) -> Result<Vec<String>, String> {
    with_conn(|conn| {
        let mut stmt = conn
        .prepare("SELECT change_id FROM draft_changes WHERE draft_id = ?1 ORDER BY item_order ASC")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![draft_id], |row| row.get::<usize, String>(0))
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
        Ok(out)
    })
}

pub fn list_turns_for_draft(draft_id: &str) -> Result<Vec<(String, String)>, String> {
    with_conn(|conn| {
        let mut stmt = conn
        .prepare(
            "SELECT t.prompt, t.response FROM turns t
             JOIN changes c ON c.prompt_id = t.id
             JOIN draft_changes dc ON dc.change_id = c.id
             WHERE dc.draft_id = ?1
             ORDER BY t.timestamp ASC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![draft_id], |row| Ok((row.get::<usize, String>(0)?, row.get::<usize, String>(1)?)))
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
        Ok(out)
    })
}

pub fn get_change(change_id: &str) -> Result<Option<Change>, String> {
    with_conn(|conn| {
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
    })
}

pub fn update_draft_status(draft_id: &str, status: DraftStatus) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE drafts SET status = ?2 WHERE id = ?1",
            params![draft_id, status.as_str()],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn update_draft_message(draft_id: &str, message: &str) -> Result<(), String> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE drafts SET message = ?2 WHERE id = ?1",
            params![draft_id, message.trim()],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    })
}

pub fn list_unassigned_changes(session_id: &str) -> Result<Vec<Change>, String> {
    with_conn(|conn| {
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
    })
}

pub fn list_open_sessions_for_repo(repo_path: &str) -> Result<Vec<SessionInfo>, String> {
    let repo_path = path::normalize_repo_path(repo_path);
    with_conn(|conn| {
        let mut stmt = conn
        .prepare(
            "SELECT id, ide, repo_path, base_commit_sha, suggested_branch, confirmed_branch, ticket, started_at, ended_at
             FROM sessions WHERE repo_path = ?1 AND ended_at IS NULL ORDER BY started_at DESC",
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
                ticket: normalize_optional(row.get::<usize, Option<String>>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_seen_at: None,
                source_status: None,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
        Ok(out)
    })
}

pub fn list_sessions_for_repo(repo_path: &str) -> Result<Vec<SessionInfo>, String> {
    with_conn(|conn| {
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
                ticket: normalize_optional(row.get::<usize, Option<String>>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_seen_at: None,
                source_status: None,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
        Ok(out)
    })
}

pub fn list_active_sessions(
    now_ts: i64,
    window_secs: i64,
) -> Result<Vec<SessionInfo>, String> {
    let cutoff = now_ts.saturating_sub(window_secs);
    with_conn(|conn| {
    let mut stmt = conn
        .prepare(
            "SELECT id, ide, repo_path, base_commit_sha, suggested_branch, confirmed_branch, ticket, started_at, ended_at, last_seen_at, source_status
             FROM sessions
             WHERE COALESCE(last_seen_at, started_at) >= ?1
               AND ended_at IS NULL
               AND lower(COALESCE(source_status, '')) NOT IN ('ended', 'completed', 'aborted')
             ORDER BY COALESCE(last_seen_at, started_at) DESC, started_at DESC",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![cutoff], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                ide: row.get(1)?,
                repo_path: row.get(2)?,
                base_commit_sha: row.get(3)?,
                suggested_branch: row.get(4)?,
                confirmed_branch: row.get(5)?,
                ticket: normalize_optional(row.get::<usize, Option<String>>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_seen_at: row.get(9)?,
                source_status: normalize_optional(row.get::<usize, Option<String>>(10)?),
            })
        })
        .map_err(|err| err.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|err| err.to_string())?);
    }
        Ok(out)
    })
}

pub fn get_session(session_id: &str) -> Result<Option<SessionInfo>, String> {
    with_conn(|conn| {
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
                ticket: normalize_optional(row.get::<usize, Option<String>>(6)?),
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_seen_at: None,
                source_status: None,
            })
        })
        .optional()
        .map_err(|err| err.to_string())?;
        Ok(value)
    })
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

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|item| {
        if item.trim().is_empty() {
            None
        } else {
            Some(item)
        }
    })
}

fn now_ts() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn ensure_sessions_columns(conn: &Connection) -> Result<(), String> {
    ensure_column(
        conn,
        "sessions",
        "last_seen_at",
        "ALTER TABLE sessions ADD COLUMN last_seen_at INTEGER",
    )?;
    ensure_column(
        conn,
        "sessions",
        "source_status",
        "ALTER TABLE sessions ADD COLUMN source_status TEXT",
    )?;
    Ok(())
}

fn migrate_normalize_repo_paths(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT id, repo_path FROM sessions")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<usize, String>(0)?, row.get::<usize, String>(1)?)))
        .map_err(|err| err.to_string())?;
    let mut update = conn
        .prepare("UPDATE sessions SET repo_path = ?2 WHERE id = ?1")
        .map_err(|err| err.to_string())?;
    for row in rows {
        let (id, repo_path) = row.map_err(|err| err.to_string())?;
        let normalized = path::normalize_repo_path(&repo_path);
        if normalized != repo_path {
            update
                .execute(params![id, normalized])
                .map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn ensure_column(conn: &Connection, table: &str, column: &str, sql: &str) -> Result<(), String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<usize, String>(1))
        .map_err(|err| err.to_string())?;
    for row in rows {
        let name = row.map_err(|err| err.to_string())?;
        if name == column {
            return Ok(());
        }
    }
    conn.execute(sql, []).map_err(|err| err.to_string())?;
    Ok(())
}

fn get_config(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    let value = conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key],
            |row| row.get::<usize, String>(0),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    Ok(value.and_then(|v| if v.trim().is_empty() { None } else { Some(v) }))
}

fn set_config(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn get_llm_provider() -> Result<Option<String>, String> {
    with_conn(|conn| get_config(conn, "llm_provider"))
}

pub fn set_llm_provider(provider: &str) -> Result<(), String> {
    with_conn(|conn| set_config(conn, "llm_provider", provider))
}

pub fn get_ollama_model() -> Result<Option<String>, String> {
    with_conn(|conn| get_config(conn, "ollama_model"))
}

pub fn set_ollama_model(model: &str) -> Result<(), String> {
    with_conn(|conn| set_config(conn, "ollama_model", model))
}

pub fn get_embedded_model() -> Result<Option<String>, String> {
    with_conn(|conn| get_config(conn, "embedded_model"))
}

pub fn set_embedded_model(model_id: &str) -> Result<(), String> {
    with_conn(|conn| set_config(conn, "embedded_model", model_id))
}
