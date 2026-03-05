use crate::claude;
use crate::cursor;
use crate::git;
use crate::grouping;
use crate::opencode;
use crate::session::{Change, ChangeLineRange, TokenUsage, ToolCall, ToolTokenUsage};
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use time::OffsetDateTime;

const DEFAULT_SOCKET: &str = "/tmp/vibe-commits.sock";

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    kind: String,
    session_id: Option<String>,
    summary: Option<String>,
    paths: Option<Vec<String>>,
    tokens: Option<TokenUsage>,
    tool_tokens: Option<Vec<ToolTokenUsage>>,
    meta: Option<serde_json::Value>,
    cwd: Option<String>,
    git_stdout: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    ok: bool,
    message: String,
    summary: Option<String>,
    staged_paths: Vec<String>,
    commit_hash: Option<String>,
    git_stdout: Option<String>,
    git_stderr: Option<String>,
}

#[derive(Debug)]
struct EventResult {
    message: String,
    summary: Option<String>,
    staged_paths: Vec<String>,
}

pub fn run_daemon(_start_stdin: bool) -> Result<(), String> {
    store::init()?;
    let socket = socket_path();
    if Path::new(&socket).exists() {
        fs::remove_file(&socket).map_err(|err| err.to_string())?;
    }
    let listener = UnixListener::bind(&socket).map_err(|err| err.to_string())?;
    start_cursor_poll_thread();
    start_claude_poll_thread();
    start_opencode_poll_thread();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_stream(stream) {
                    eprintln!("daemon: {err}");
                }
            }
            Err(err) => eprintln!("daemon: {err}"),
        }
    }
    Ok(())
}

fn start_cursor_poll_thread() {
    if parse_bool_env("GG_CURSOR_POLL") == Some(false) {
        return;
    }
    let interval_secs = env::var("GG_CURSOR_POLL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    std::thread::spawn(move || loop {
        if cursor::cursor_running() {
            let root = env::var("GG_CURSOR_REPO")
                .ok()
                .or_else(|| git::repo_root().ok());
            if let Some(root) = root {
                let git_stdout = parse_bool_env("GG_GIT_STDOUT").unwrap_or(false);
                let compact = parse_bool_env("GG_COMPACT").unwrap_or(false);
                if let Err(err) =
                    cursor::poll_completed_sessions(Path::new(&root), git_stdout, compact)
                {
                    eprintln!("daemon: cursor poll error: {err}");
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    });
}

fn start_claude_poll_thread() {
    if parse_bool_env("GG_CLAUDE_POLL") == Some(false) {
        return;
    }
    let interval_secs = env::var("GG_CLAUDE_POLL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    std::thread::spawn(move || loop {
        let root = env::var("GG_CLAUDE_REPO")
            .ok()
            .or_else(|| git::repo_root().ok());
        if let Some(root) = root {
            let git_stdout = parse_bool_env("GG_GIT_STDOUT").unwrap_or(false);
            let compact = parse_bool_env("GG_COMPACT").unwrap_or(false);
            if let Err(err) = claude::poll_assistant_responses(Path::new(&root), git_stdout, compact)
            {
                eprintln!("daemon: claude poll error: {err}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    });
}

fn start_opencode_poll_thread() {
    if parse_bool_env("GG_OPENCODE_POLL") == Some(false) {
        return;
    }
    let interval_secs = env::var("GG_OPENCODE_POLL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    std::thread::spawn(move || loop {
        let root = env::var("GG_OPENCODE_REPO")
            .ok()
            .or_else(|| git::repo_root().ok());
        if let Some(root) = root {
            let git_stdout = parse_bool_env("GG_GIT_STDOUT").unwrap_or(false);
            let compact = parse_bool_env("GG_COMPACT").unwrap_or(false);
            if let Err(err) = opencode::poll_assistant_messages(Path::new(&root), git_stdout, compact)
            {
                eprintln!("daemon: opencode poll error: {err}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    });
}

pub fn send_event(
    session_id: &str,
    summary: &str,
    paths: &[String],
    tokens: Option<TokenUsage>,
    tool_tokens: Vec<ToolTokenUsage>,
    git_stdout: bool,
    compact: bool,
    meta: Option<serde_json::Value>,
    cwd_override: Option<String>,
) -> Result<(), String> {
    ensure_daemon_running()?;
    let cwd = match cwd_override {
        Some(value) => value,
        None => env::current_dir()
            .map_err(|err| err.to_string())?
            .to_string_lossy()
            .to_string(),
    };
    let request = Request {
        kind: "event".to_string(),
        session_id: Some(session_id.to_string()),
        summary: Some(summary.to_string()),
        paths: Some(paths.to_vec()),
        tokens,
        tool_tokens: Some(tool_tokens),
        meta,
        cwd: Some(cwd),
        git_stdout: Some(git_stdout),
    };
    let socket = socket_path();
    let mut stream = UnixStream::connect(&socket).map_err(|err| format!("connect: {err}"))?;
    let payload = serde_json::to_vec(&request).map_err(|err| err.to_string())?;
    stream.write_all(&payload).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;
    let mut response_buf = String::new();
    stream
        .read_to_string(&mut response_buf)
        .map_err(|err| err.to_string())?;
    let response: Response = serde_json::from_str(&response_buf).map_err(|err| err.to_string())?;
    if response.ok {
        if !compact {
            println!("{}", response.message);
        }
        Ok(())
    } else {
        Err(response.message)
    }
}

fn handle_stream(mut stream: UnixStream) -> Result<(), String> {
    let mut buffer = String::new();
    stream
        .read_to_string(&mut buffer)
        .map_err(|err| err.to_string())?;
    if buffer.trim().is_empty() {
        return Ok(());
    }
    let request: Request = serde_json::from_str(&buffer).map_err(|err| err.to_string())?;
    let response = match request.kind.as_str() {
        "event" => handle_event(&request),
        "ping" | "end_all" => Ok(EventResult {
            message: "ok".to_string(),
            summary: None,
            staged_paths: Vec::new(),
        }),
        _ => Err("unsupported request".to_string()),
    };
    let response = match response {
        Ok(result) => Response {
            ok: true,
            message: result.message,
            summary: result.summary,
            staged_paths: result.staged_paths,
            commit_hash: None,
            git_stdout: None,
            git_stderr: None,
        },
        Err(message) => Response {
            ok: false,
            message,
            summary: None,
            staged_paths: Vec::new(),
            commit_hash: None,
            git_stdout: None,
            git_stderr: None,
        },
    };
    let response_json = serde_json::to_vec(&response).map_err(|err| err.to_string())?;
    stream.write_all(&response_json).map_err(|err| err.to_string())?;
    Ok(())
}

fn handle_event(request: &Request) -> Result<EventResult, String> {
    let session_id = request.session_id.as_ref().ok_or("missing session_id")?;
    let cwd = request
        .cwd
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let root = git::repo_root_from(&cwd).or_else(|_| git::repo_root())?;
    let base_commit_sha = git::head_commit_in_root(&root)?;
    let meta = request.meta.as_ref();
    let ide = meta
        .and_then(|value| value.get("source"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let prompt = meta
        .and_then(extract_prompt_text)
        .unwrap_or_default();
    let response = meta
        .and_then(extract_response_text)
        .or_else(|| request.summary.clone())
        .unwrap_or_else(|| "assistant response".to_string());
    let suggested = suggest_branch_name(&prompt);
    store::upsert_session(
        session_id,
        ide,
        &root,
        &base_commit_sha,
        &suggested,
        if prompt.is_empty() {
            None
        } else {
            Some(prompt.as_str())
        },
    )?;
    let turn_id = stable_id(&format!(
        "{session_id}:{}:{response}",
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    store::insert_turn(&turn_id, session_id, &prompt, &response, &Vec::<ToolCall>::new())?;
    let changes = capture_changes_for_turn(&root, session_id, &turn_id, &base_commit_sha)?;
    if !changes.is_empty() {
        assign_changes_to_draft(session_id, &prompt, &changes)?;
    }
    if meta
        .and_then(|value| value.get("end"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        store::mark_session_ended(session_id)?;
    }
    let changed_paths = changes.into_iter().map(|item| item.file_path).collect();
    Ok(EventResult {
        message: "draft changes captured".to_string(),
        summary: Some("captured".to_string()),
        staged_paths: changed_paths,
    })
}

pub fn approve_drafts(
    session_id: &str,
    draft_ids: Option<Vec<String>>,
    branch_override: Option<String>,
) -> Result<Vec<String>, String> {
    let session = store::get_session(session_id)?.ok_or("session not found")?;
    let branch = branch_override
        .or(session.confirmed_branch.clone())
        .unwrap_or(session.suggested_branch.clone());
    store::set_session_branch(session_id, &branch)?;
    git::checkout_new_branch_from(&session.repo_path, &branch, &session.base_commit_sha)?;
    let drafts = store::list_drafts(session_id)?;
    let selected: Vec<_> = match draft_ids {
        Some(ids) => drafts
            .into_iter()
            .filter(|draft| ids.contains(&draft.id))
            .collect(),
        None => drafts,
    };
    let mut commits = Vec::new();
    for draft in selected {
        let change_ids = store::draft_change_ids(&draft.id)?;
        let mut files = HashSet::new();
        for change_id in change_ids {
            if let Some(change) = store::get_change(&change_id)? {
                git::apply_patch_in_root(&session.repo_path, &change.diff)?;
                files.insert(change.file_path);
            }
        }
        let file_list = files.into_iter().collect::<Vec<_>>();
        git::add_files_in_root(&session.repo_path, &file_list)?;
        let commit = git::commit_message_in_root(&session.repo_path, &draft.message)?;
        store::update_draft_status(&draft.id, crate::session::DraftStatus::Approved)?;
        commits.push(commit);
    }
    store::mark_session_ended(session_id)?;
    write_session_ref(session_id)?;
    Ok(commits)
}

fn write_session_ref(session_id: &str) -> Result<(), String> {
    let session = store::get_session(session_id)?.ok_or("session not found")?;
    let drafts = store::list_drafts(session_id)?;
    let draft_messages = drafts
        .into_iter()
        .map(|item| item.message)
        .collect::<Vec<_>>();
    let body = json!({
        "session_id": session.id,
        "ide": session.ide,
        "repo": session.repo_path,
        "branch": session.confirmed_branch.unwrap_or(session.suggested_branch),
        "ticket": session.ticket,
        "base_commit": session.base_commit_sha,
        "started_at": session.started_at,
        "ended_at": session.ended_at.unwrap_or_else(|| OffsetDateTime::now_utc().unix_timestamp()),
        "draft_commits": draft_messages,
    });
    let payload = serde_json::to_string(&body).map_err(|err| err.to_string())?;
    let ref_name = format!("refs/vibe/sessions/{session_id}");
    git::write_ref_blob_in_root(&session.repo_path, &ref_name, &payload)
}

pub fn ensure_daemon_running() -> Result<(), String> {
    let socket = socket_path();
    if UnixStream::connect(&socket).is_ok() {
        return Ok(());
    }
    let exe = env::current_exe().map_err(|err| err.to_string())?;
    Command::new(exe)
        .env("GG_DAEMON", "1")
        .spawn()
        .map_err(|err| err.to_string())?;
    for _ in 0..20 {
        if UnixStream::connect(&socket).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("daemon failed to start".to_string())
}

pub fn end_all_sessions() -> Result<(), String> {
    let socket = socket_path();
    let mut stream = UnixStream::connect(&socket).map_err(|err| format!("connect: {err}"))?;
    let request = Request {
        kind: "end_all".to_string(),
        session_id: None,
        summary: None,
        paths: None,
        tokens: None,
        tool_tokens: None,
        meta: None,
        cwd: None,
        git_stdout: None,
    };
    let payload = serde_json::to_vec(&request).map_err(|err| err.to_string())?;
    stream.write_all(&payload).map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn socket_path() -> String {
    env::var("GG_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string())
}

fn parse_bool_env(key: &str) -> Option<bool> {
    env::var(key).ok().and_then(|value| parse_bool(&value))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn extract_prompt_text(meta: &serde_json::Value) -> Option<String> {
    let prompt = meta.get("prompt")?;
    match prompt {
        serde_json::Value::String(value) => Some(value.to_string()),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        _ => None,
    }
}

fn extract_response_text(meta: &serde_json::Value) -> Option<String> {
    if let Some(response) = meta.get("response") {
        match response {
            serde_json::Value::String(value) => return Some(value.to_string()),
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(|value| value.as_str()) {
                    return Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    meta.get("response_text")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn suggest_branch_name(prompt: &str) -> String {
    let slug = prompt
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "session".to_string()
    } else {
        slug.chars().take(40).collect::<String>()
    };
    format!("feature/{slug}")
}

fn capture_changes_for_turn(
    repo_root: &str,
    session_id: &str,
    turn_id: &str,
    base_commit_sha: &str,
) -> Result<Vec<Change>, String> {
    let snapshot = git::diff_u0_in_root(repo_root)?;
    let previous = store::get_last_snapshot(session_id)?;
    if snapshot == previous {
        return Ok(Vec::new());
    }
    let prev_blocks: HashSet<String> = parse_blocks(&previous)
        .into_iter()
        .map(|item| item.raw)
        .collect();
    let blocks = parse_blocks(&snapshot);
    let mut out = Vec::new();
    for block in blocks {
        if prev_blocks.contains(&block.raw) {
            continue;
        }
        let id = stable_id(&format!("{}:{}:{}", block.file_path, block.raw, base_commit_sha));
        let change = Change {
            id,
            session_id: session_id.to_string(),
            prompt_id: turn_id.to_string(),
            file_path: block.file_path.clone(),
            base_commit_sha: base_commit_sha.to_string(),
            diff: block.raw,
            line_range: ChangeLineRange {
                old_start: block.old_start,
                old_count: block.old_count,
                new_start: block.new_start,
                new_count: block.new_count,
            },
            captured_at: OffsetDateTime::now_utc().unix_timestamp(),
            change_type: block.change_type,
        };
        store::insert_change(&change)?;
        out.push(change);
    }
    store::set_last_snapshot(session_id, &snapshot)?;
    Ok(out)
}

fn assign_changes_to_draft(session_id: &str, prompt: &str, changes: &[Change]) -> Result<(), String> {
    let lockfiles = [
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Cargo.lock",
        "poetry.lock",
        "Gemfile.lock",
        "go.sum",
        "composer.lock",
    ];
    let mut normal = Vec::new();
    let mut lock = Vec::new();
    for change in changes {
        if lockfiles.iter().any(|name| change.file_path.ends_with(name)) {
            lock.push(change.clone());
        } else {
            normal.push(change.clone());
        }
    }
    if !normal.is_empty() {
        let message = grouping::infer_message(prompt, &normal);
        let draft_id = ensure_draft(session_id, &message, false)?;
        for change in normal {
            store::add_change_to_draft(&draft_id, &change.id)?;
        }
    }
    if !lock.is_empty() {
        let message = "chore: update lockfiles".to_string();
        let draft_id = ensure_draft(session_id, &message, true)?;
        for change in lock {
            store::add_change_to_draft(&draft_id, &change.id)?;
        }
    }
    Ok(())
}

fn ensure_draft(session_id: &str, message: &str, auto_approved: bool) -> Result<String, String> {
    let drafts = store::list_drafts(session_id)?;
    if let Some(existing) = drafts
        .into_iter()
        .find(|item| item.message.eq_ignore_ascii_case(message))
    {
        return Ok(existing.id);
    }
    let id = stable_id(&format!(
        "{session_id}:{message}:{}",
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    store::create_draft(&id, session_id, message, auto_approved)?;
    Ok(id)
}

struct DiffBlock {
    file_path: String,
    raw: String,
    old_start: i64,
    old_count: i64,
    new_start: i64,
    new_count: i64,
    change_type: String,
}

fn parse_blocks(diff: &str) -> Vec<DiffBlock> {
    let mut blocks = Vec::new();
    let mut current_file = String::new();
    let mut current = Vec::new();
    let mut old_start = 0i64;
    let mut old_count = 0i64;
    let mut new_start = 0i64;
    let mut new_count = 0i64;
    for line in diff.lines() {
        if line.starts_with("diff --git") {
            if !current.is_empty() && !current_file.is_empty() {
                blocks.push(DiffBlock {
                    file_path: current_file.clone(),
                    raw: current.join("\n") + "\n",
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    change_type: "edit".to_string(),
                });
            }
            current.clear();
            current.push(line.to_string());
            current_file = parse_file_from_diff_header(line).unwrap_or_default();
            old_start = 0;
            old_count = 0;
            new_start = 0;
            new_count = 0;
            continue;
        }
        if line.starts_with("@@") {
            let (os, oc, ns, nc) = parse_hunk_header(line);
            old_start = os;
            old_count = oc;
            new_start = ns;
            new_count = nc;
        }
        if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() && !current_file.is_empty() {
        blocks.push(DiffBlock {
            file_path: current_file,
            raw: current.join("\n") + "\n",
            old_start,
            old_count,
            new_start,
            new_count,
            change_type: "edit".to_string(),
        });
    }
    blocks
}

fn parse_file_from_diff_header(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    parts
        .get(3)
        .map(|part| part.trim_start_matches("b/").to_string())
}

fn parse_hunk_header(line: &str) -> (i64, i64, i64, i64) {
    let mut old_start = 0;
    let mut old_count = 0;
    let mut new_start = 0;
    let mut new_count = 0;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 3 {
        let old = parts[1].trim_start_matches('-');
        let new = parts[2].trim_start_matches('+');
        let old_parts: Vec<&str> = old.split(',').collect();
        let new_parts: Vec<&str> = new.split(',').collect();
        old_start = old_parts
            .first()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        old_count = old_parts
            .get(1)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);
        new_start = new_parts
            .first()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        new_count = new_parts
            .get(1)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);
    }
    (old_start, old_count, new_start, new_count)
}

fn stable_id(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")
}
