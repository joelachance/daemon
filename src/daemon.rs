use crate::claude;
use crate::cursor;
use crate::git::{self, GitOutput};
use crate::opencode;
use crate::session::{TokenUsage, ToolTokenUsage};
use crate::store;
use console::{style, Color};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const DEFAULT_SOCKET: &str = "/tmp/ggd.sock";

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
    #[serde(default)]
    staged_paths: Vec<String>,
    commit_hash: Option<String>,
    git_stdout: Option<String>,
    git_stderr: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionEventPayload {
    schema: u32,
    session_id: String,
    summary: String,
    commit: Option<String>,
    paths: Vec<String>,
    timestamp: String,
    tokens: Option<TokenUsage>,
    tool_tokens: Vec<ToolTokenUsage>,
    meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionState {
    tool: String,
    session_id: String,
    root: String,
    last_activity: i64,
    explicit_end: bool,
    soft_end: bool,
    active: bool,
    auto_push_attempted: bool,
    auto_push_last_attempt: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionRegistry {
    sessions: HashMap<String, SessionState>,
}

pub fn run_daemon(start_stdin: bool) -> Result<(), String> {
    crate::cli::print_banner();
    let socket = socket_path();
    if Path::new(&socket).exists() {
        fs::remove_file(&socket).map_err(|err| err.to_string())?;
    }

    let listener = UnixListener::bind(&socket).map_err(|err| err.to_string())?;
    eprintln!("daemon: listening on {socket}");

    let registry = Arc::new(Mutex::new(SessionRegistry::default()));
    if start_stdin {
        start_stdin_thread(registry.clone());
    }
    check_auto_push_on_startup();
    start_cursor_poll_thread();
    start_claude_poll_thread();
    start_opencode_poll_thread();
    start_auto_push_thread(registry.clone());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_stream(stream, registry.clone()) {
                    eprintln!("daemon: {err}");
                }
            }
            Err(err) => {
                eprintln!("daemon: {err}");
            }
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
                let git_stdout = parse_bool_env("GG_GIT_STDOUT")
                    .or_else(|| {
                        git::get_local_config_in_root(&root, "gg.git-stdout")
                            .ok()
                            .flatten()
                            .as_deref()
                            .and_then(parse_bool)
                    })
                    .unwrap_or(false);
                let compact = parse_bool_env("GG_COMPACT")
                    .or_else(|| {
                        git::get_local_config_in_root(&root, "gg.compact")
                            .ok()
                            .flatten()
                            .as_deref()
                            .and_then(parse_bool)
                    })
                    .unwrap_or(false);

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
            let git_stdout = parse_bool_env("GG_GIT_STDOUT")
                .or_else(|| {
                    git::get_local_config_in_root(&root, "gg.git-stdout")
                        .ok()
                        .flatten()
                        .as_deref()
                        .and_then(parse_bool)
                })
                .unwrap_or(false);
            let compact = parse_bool_env("GG_COMPACT")
                .or_else(|| {
                    git::get_local_config_in_root(&root, "gg.compact")
                        .ok()
                        .flatten()
                        .as_deref()
                        .and_then(parse_bool)
                })
                .unwrap_or(false);

            if let Err(err) =
                claude::poll_assistant_responses(Path::new(&root), git_stdout, compact)
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
            let git_stdout = parse_bool_env("GG_GIT_STDOUT")
                .or_else(|| {
                    git::get_local_config_in_root(&root, "gg.git-stdout")
                        .ok()
                        .flatten()
                        .as_deref()
                        .and_then(parse_bool)
                })
                .unwrap_or(false);
            let compact = parse_bool_env("GG_COMPACT")
                .or_else(|| {
                    git::get_local_config_in_root(&root, "gg.compact")
                        .ok()
                        .flatten()
                        .as_deref()
                        .and_then(parse_bool)
                })
                .unwrap_or(false);

            if let Err(err) =
                opencode::poll_assistant_messages(Path::new(&root), git_stdout, compact)
            {
                eprintln!("daemon: opencode poll error: {err}");
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    });
}

fn start_auto_push_thread(registry: Arc<Mutex<SessionRegistry>>) {
    if parse_bool_env("GG_AUTO_PUSH") == Some(false) {
        return;
    }

    let interval_secs = env::var("GG_AUTO_PUSH_POLL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    let timeout_secs = env::var("GG_AUTO_PUSH_AFTER_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3600);
    let retry_secs = env::var("GG_AUTO_PUSH_RETRY_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(300);

    std::thread::spawn(move || loop {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut branches = Vec::new();

        {
            let mut guard = match registry.lock() {
                Ok(value) => value,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_secs(interval_secs));
                    continue;
                }
            };

            for session in guard.sessions.values_mut() {
                if session.active {
                    continue;
                }
                if !session.explicit_end && !session.soft_end {
                    continue;
                }
                if now - session.last_activity < timeout_secs {
                    continue;
                }
                if session.auto_push_attempted {
                    continue;
                }
                if session.auto_push_last_attempt > 0
                    && now - session.auto_push_last_attempt < retry_secs
                {
                    continue;
                }
                session.auto_push_last_attempt = now;
                branches.push((
                    session.root.clone(),
                    session_branch_name(&session.session_id),
                ));
            }
        }

        branches.sort();
        branches.dedup();

        for (root, branch) in branches {
            match auto_push_branch(&root, &branch) {
                Ok(AutoPushOutcome::Pushed) => {
                    mark_auto_push_attempted(&registry, &root, now, timeout_secs)
                }
                Ok(AutoPushOutcome::Skipped) => {}
                Err(err) => eprintln!("daemon: auto push error: {err}"),
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(interval_secs));
    });
}

enum AutoPushOutcome {
    Pushed,
    Skipped,
}

fn auto_push_branch(root: &str, branch: &str) -> Result<AutoPushOutcome, String> {
    if !git::has_remote_in_root(root)? {
        return Ok(AutoPushOutcome::Skipped);
    }
    if !git::branch_exists_in_root(root, branch)? {
        return Ok(AutoPushOutcome::Skipped);
    }
    if !git::working_tree_clean_in_root(root)? {
        return Ok(AutoPushOutcome::Skipped);
    }
    git::push_branch_in_root(root, branch)?;
    Ok(AutoPushOutcome::Pushed)
}

fn mark_auto_push_attempted(
    registry: &Arc<Mutex<SessionRegistry>>,
    root: &str,
    now: i64,
    timeout_secs: i64,
) {
    let mut guard = match registry.lock() {
        Ok(value) => value,
        Err(_) => return,
    };
    for session in guard.sessions.values_mut() {
        if session.root != root {
            continue;
        }
        if session.active {
            continue;
        }
        if !session.explicit_end && !session.soft_end {
            continue;
        }
        if now - session.last_activity < timeout_secs {
            continue;
        }
        session.auto_push_attempted = true;
    }
}

fn check_auto_push_on_startup() {
    if parse_bool_env("GG_AUTO_PUSH") == Some(false) {
        return;
    }

    let timeout_secs = env::var("GG_AUTO_PUSH_AFTER_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3600);

    let root = match git::repo_root() {
        Ok(value) => value,
        Err(_) => return,
    };

    let sessions = match store::list_sessions() {
        Ok(value) => value,
        Err(_) => return,
    };

    let now = OffsetDateTime::now_utc().unix_timestamp();
    let branches = branches_for_startup_autopush(&sessions, now, timeout_secs);
    for branch in branches {
        if let Err(err) = auto_push_branch(&root, &branch) {
            eprintln!("daemon: auto push error: {err}");
        }
    }
}

fn branches_for_startup_autopush(
    sessions: &[store::SessionInfo],
    now: i64,
    timeout_secs: i64,
) -> Vec<String> {
    let mut branches = Vec::new();
    for session in sessions {
        if session.end_status.is_none() {
            continue;
        }
        let last_event = match session.last_event.as_deref() {
            Some(value) => value,
            None => continue,
        };
        let ts = match parse_event_timestamp(last_event) {
            Some(value) => value,
            None => continue,
        };
        if now - ts < timeout_secs {
            continue;
        }
        branches.push(session_branch_name(&session.id));
    }
    branches.sort();
    branches.dedup();
    branches
}

fn parse_event_timestamp(value: &str) -> Option<i64> {
    if let Ok(parsed) = OffsetDateTime::parse(value, &Rfc3339) {
        return Some(parsed.unix_timestamp());
    }

    let mut trimmed = value.trim().to_string();
    if let Some(stripped) = trimmed.strip_suffix(".json") {
        trimmed = stripped.to_string();
    }

    if let Some((date, rest)) = trimmed.split_once('T') {
        let rest_value = rest.to_string();
        let mut replaced = String::new();
        let mut hyphen_count = 0usize;
        for ch in rest_value.chars() {
            if ch == '-' && hyphen_count < 2 {
                replaced.push(':');
                hyphen_count += 1;
            } else {
                replaced.push(ch);
            }
        }
        let candidate = format!("{date}T{replaced}");
        if let Ok(parsed) = OffsetDateTime::parse(&candidate, &Rfc3339) {
            return Some(parsed.unix_timestamp());
        }
    }

    None
}

fn start_stdin_thread(registry: Arc<Mutex<SessionRegistry>>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    if buf[0] == 0x03 {
                        if let Err(err) = handle_session_end(&registry) {
                            eprintln!("daemon: {err}");
                        }
                    }
                }
                Err(err) => {
                    eprintln!("daemon: stdin read error: {err}");
                    break;
                }
            }
        }
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

    let mut spinner = SpinnerGuard::new(start_spinner("checkpointing"));

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
    spinner.finish();

    if response.ok {
        print_response(&response, compact);
        Ok(())
    } else {
        Err(response.message)
    }
}

fn handle_stream(
    mut stream: UnixStream,
    registry: Arc<Mutex<SessionRegistry>>,
) -> Result<(), String> {
    let mut buffer = String::new();
    stream
        .read_to_string(&mut buffer)
        .map_err(|err| err.to_string())?;
    if buffer.trim().is_empty() {
        return Ok(());
    }
    let request: Request = serde_json::from_str(&buffer).map_err(|err| err.to_string())?;

    let kind = request.kind.clone();
    let response = match kind.as_str() {
        "event" => handle_event(&request, registry),
        "end_all" => handle_session_end(&registry).map(|_| EventResult::pong()),
        "ping" => Ok(EventResult::pong()),
        _ => Err("unsupported request".to_string()),
    };

    let response = match response {
        Ok(result) => result.to_response(true),
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
    if let Err(err) = stream.write_all(&response_json) {
        return Err(err.to_string());
    }

    Ok(())
}

struct EventResult {
    message: String,
    summary: Option<String>,
    staged_paths: Vec<String>,
    commit_hash: Option<String>,
    git_stdout: Option<String>,
    git_stderr: Option<String>,
}

impl EventResult {
    fn pong() -> Self {
        Self {
            message: "pong".to_string(),
            summary: None,
            staged_paths: Vec::new(),
            commit_hash: None,
            git_stdout: None,
            git_stderr: None,
        }
    }

    fn to_response(self, ok: bool) -> Response {
        Response {
            ok,
            message: self.message,
            summary: self.summary,
            staged_paths: self.staged_paths,
            commit_hash: self.commit_hash,
            git_stdout: self.git_stdout,
            git_stderr: self.git_stderr,
        }
    }
}

fn handle_event(
    request: &Request,
    registry: Arc<Mutex<SessionRegistry>>,
) -> Result<EventResult, String> {
    let session_id = request.session_id.as_ref().ok_or("missing session_id")?;
    let cwd = match &request.cwd {
        Some(value) => value.clone(),
        None => env::current_dir()
            .map_err(|err| err.to_string())?
            .to_string_lossy()
            .to_string(),
    };
    let root = git::repo_root_from(&cwd).or_else(|_| git::repo_root())?;
    ensure_session_branch(&root, session_id)?;
    let commit_message = build_commit_message(request.summary.as_deref(), request.meta.as_ref());
    let tool_tokens = request.tool_tokens.clone().unwrap_or_default();
    let git_stdout = request.git_stdout.unwrap_or(false);
    let tool = request
        .meta
        .as_ref()
        .and_then(|meta| meta.get("source"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let (explicit_end, soft_end) = extract_end_flags(request.meta.as_ref());
    update_registry(&registry, &tool, session_id, &root, explicit_end, soft_end)?;

    let requested_paths = request.paths.clone().unwrap_or_default();
    let mut stage_paths = if requested_paths.is_empty() {
        let mut paths = git::list_changed_paths_in_root(&root)?;
        paths = git::filter_paths_to_cwd(&root, Path::new(&cwd), &paths)?;
        paths
    } else {
        requested_paths
    };

    stage_paths = git::filter_ignored_paths_in_root(&root, &stage_paths)?;

    let mut stage_output = GitOutput::default();
    if !stage_paths.is_empty() {
        stage_output = git::stage_paths_in_root(&root, &stage_paths)?;
    }

    let mut trailers: Vec<(String, String)> = Vec::new();
    if let Some(coauthor) = coauthor_trailer() {
        trailers.push(("Co-authored-by".to_string(), coauthor));
    }

    let (commit_hash, commit_output) = git::commit_in_root_with_footer(
        &root,
        &commit_message.subject,
        commit_message.footer.as_deref(),
        &trailers,
    )?;

    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|err| err.to_string())?;

    let payload = SessionEventPayload {
        schema: 1,
        session_id: session_id.to_string(),
        summary: commit_message.subject.clone(),
        commit: commit_hash.clone(),
        paths: stage_paths.clone(),
        timestamp,
        tokens: request.tokens.clone(),
        tool_tokens,
        meta: request.meta.clone(),
    };

    let payload_json = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    let _ = store::write_event(&root, session_id, &payload_json)?;

    let mut git_stdout_buf = String::new();
    let mut git_stderr_buf = String::new();
    if !stage_output.stdout.trim().is_empty() {
        git_stdout_buf.push_str(&stage_output.stdout);
    }
    if !commit_output.stdout.trim().is_empty() {
        if !git_stdout_buf.is_empty() {
            git_stdout_buf.push('\n');
        }
        git_stdout_buf.push_str(&commit_output.stdout);
    }
    if !stage_output.stderr.trim().is_empty() {
        git_stderr_buf.push_str(&stage_output.stderr);
    }
    if !commit_output.stderr.trim().is_empty() {
        if !git_stderr_buf.is_empty() {
            git_stderr_buf.push('\n');
        }
        git_stderr_buf.push_str(&commit_output.stderr);
    }

    Ok(EventResult {
        message: "event stored".to_string(),
        summary: Some(commit_message.subject),
        staged_paths: stage_paths,
        commit_hash,
        git_stdout: if git_stdout {
            Some(git_stdout_buf)
        } else {
            None
        },
        git_stderr: if git_stdout {
            Some(git_stderr_buf)
        } else {
            None
        },
    })
}

fn handle_session_end(registry: &Arc<Mutex<SessionRegistry>>) -> Result<(), String> {
    let mut guard = registry
        .lock()
        .map_err(|_| "session registry lock poisoned".to_string())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();

    for session in guard.sessions.values_mut() {
        if !session.active {
            continue;
        }

        let timestamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|err| err.to_string())?;

        let payload = SessionEventPayload {
            schema: 1,
            session_id: session.session_id.clone(),
            summary: "session end".to_string(),
            commit: None,
            paths: Vec::new(),
            timestamp,
            tokens: None,
            tool_tokens: Vec::new(),
            meta: Some(json!({
                "source": "stdin",
                "end": true,
                "signal": "CTRL+C",
            })),
        };

        let payload_json = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
        let _ = store::write_event(&session.root, &session.session_id, &payload_json)?;

        session.last_activity = now;
        session.explicit_end = true;
        session.soft_end = false;
        session.active = false;
    }

    Ok(())
}

pub fn ensure_daemon_running() -> Result<(), String> {
    let socket = socket_path();
    if let Ok(mut stream) = UnixStream::connect(&socket) {
        let ping = Request {
            kind: "ping".to_string(),
            session_id: None,
            summary: None,
            paths: None,
            tokens: None,
            tool_tokens: None,
            meta: None,
            cwd: None,
            git_stdout: None,
        };

        if let Ok(payload) = serde_json::to_vec(&ping) {
            if stream.write_all(&payload).is_ok()
                && stream.shutdown(std::net::Shutdown::Write).is_ok()
            {
                let mut response_buf = String::new();
                if stream.read_to_string(&mut response_buf).is_ok()
                    && serde_json::from_str::<Response>(&response_buf).is_ok()
                {
                    return Ok(());
                }
            }
        }
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

fn coauthor_trailer() -> Option<String> {
    let raw = env::var("GG_COAUTHOR").ok();
    match raw.as_deref() {
        Some("") | Some("0") | Some("false") | Some("off") | Some("no") => None,
        Some(value) => Some(value.to_string()),
        None => {
            if let Ok(Some(value)) = git::get_local_config("gg.coauthor") {
                if matches!(value.as_str(), "" | "0" | "false" | "off" | "no") {
                    None
                } else {
                    Some(value)
                }
            } else {
                Some("gg <gg@local>".to_string())
            }
        }
    }
}

fn build_commit_message(summary: Option<&str>, meta: Option<&serde_json::Value>) -> CommitMessage {
    let source = meta
        .and_then(|value| value.get("source"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let agent = meta
        .and_then(|value| value.get("agent"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let allow_meta = source != "daemon" && agent != "build";

    let prompt_text = if allow_meta {
        meta.and_then(extract_prompt_text)
    } else {
        None
    };
    let response_text = if allow_meta {
        meta.and_then(extract_response_text)
    } else {
        None
    };
    let mut base = prompt_text
        .as_deref()
        .and_then(summarize_text)
        .or_else(|| response_text.as_deref().and_then(summarize_text))
        .or_else(|| summary.and_then(summarize_text));
    if base.is_none() || !allow_meta {
        base = Some("update session changes".to_string());
    }

    let base_text = base.unwrap_or_else(|| "update session changes".to_string());
    let prefix = classify_prefix(&base_text);
    let subject = format!("{prefix}: {base_text}");
    let subject = trim_subject(&subject, 72);

    let issue = find_issue_number(
        prompt_text.as_deref().unwrap_or(""),
        git::branch_name().ok().flatten().as_deref().unwrap_or(""),
    );
    let footer = issue.map(|value| format!("Resolves #{value}"));

    CommitMessage { subject, footer }
}

struct CommitMessage {
    subject: String,
    footer: Option<String>,
}

fn update_registry(
    registry: &Arc<Mutex<SessionRegistry>>,
    tool: &str,
    session_id: &str,
    root: &str,
    explicit_end: bool,
    soft_end: bool,
) -> Result<(), String> {
    let mut guard = registry
        .lock()
        .map_err(|_| "session registry lock poisoned".to_string())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let key = format!("{tool}:{session_id}");
    let entry = guard.sessions.entry(key).or_insert(SessionState {
        tool: tool.to_string(),
        session_id: session_id.to_string(),
        root: root.to_string(),
        last_activity: now,
        explicit_end: false,
        soft_end: false,
        active: true,
        auto_push_attempted: false,
        auto_push_last_attempt: 0,
    });

    entry.root = root.to_string();
    entry.last_activity = now;
    if explicit_end {
        entry.explicit_end = true;
        entry.soft_end = false;
        entry.active = false;
        entry.auto_push_attempted = false;
        entry.auto_push_last_attempt = 0;
    } else if soft_end {
        entry.soft_end = true;
        entry.active = false;
        entry.auto_push_attempted = false;
        entry.auto_push_last_attempt = 0;
    } else {
        entry.explicit_end = false;
        entry.soft_end = false;
        entry.active = true;
        entry.auto_push_attempted = false;
        entry.auto_push_last_attempt = 0;
    }

    Ok(())
}

fn extract_end_flags(meta: Option<&serde_json::Value>) -> (bool, bool) {
    let value = match meta {
        Some(value) => value,
        None => return (false, false),
    };
    let explicit_end = value
        .get("end")
        .and_then(|val| val.as_bool())
        .unwrap_or(false);
    let soft_end = value
        .get("soft_end")
        .and_then(|val| val.as_bool())
        .unwrap_or(false);
    (explicit_end, soft_end)
}

fn ensure_session_branch(root: &str, session_id: &str) -> Result<(), String> {
    let branch = session_branch_name(session_id);
    if git::branch_exists_in_root(root, &branch)? {
        git::checkout_branch_in_root(root, &branch)?;
    } else {
        git::create_branch_in_root(root, &branch)?;
    }
    Ok(())
}

fn session_branch_name(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len() + 11);
    out.push_str("gg/session-");
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

fn extract_prompt_text(meta: &serde_json::Value) -> Option<String> {
    let prompt = meta.get("prompt")?;
    match prompt {
        serde_json::Value::String(value) => Some(value.to_string()),
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(value)) = map.get("text") {
                return Some(value.to_string());
            }
            if let Some(serde_json::Value::String(value)) = map.get("rich_text") {
                return Some(value.to_string());
            }
            None
        }
        _ => None,
    }
}

fn extract_response_text(meta: &serde_json::Value) -> Option<String> {
    if let Some(response) = meta.get("response") {
        match response {
            serde_json::Value::String(value) => return Some(value.to_string()),
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(value)) = map.get("text") {
                    return Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(serde_json::Value::String(value)) = meta.get("response_text") {
        return Some(value.to_string());
    }
    None
}

fn classify_prefix(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if lower.contains("fix") || lower.contains("bug") || lower.contains("error") {
        return "fix";
    }
    if lower.contains("add") || lower.contains("create") || lower.contains("implement") {
        return "feat";
    }
    if lower.contains("doc") || lower.contains("readme") {
        return "docs";
    }
    if lower.contains("refactor") || lower.contains("cleanup") || lower.contains("restructure") {
        return "refactor";
    }
    if lower.contains("test") {
        return "test";
    }
    "chore"
}

fn trim_subject(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }
    value.chars().take(max_len).collect::<String>()
}

fn find_issue_number(prompt: &str, branch: &str) -> Option<String> {
    extract_issue_number(prompt).or_else(|| extract_issue_number(branch))
}

fn extract_issue_number(text: &str) -> Option<String> {
    if let Some(value) = scan_issue_patterns(text) {
        return Some(value);
    }
    first_number(text)
}

fn scan_issue_patterns(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if let Some(hash) = extract_hash_number(&lower) {
        return Some(hash);
    }
    for keyword in ["issue", "ticket", "jira"] {
        if let Some(value) = extract_keyword_number(&lower, keyword) {
            return Some(value);
        }
    }
    None
}

fn extract_hash_number(text: &str) -> Option<String> {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            let mut digits = String::new();
            while let Some(next) = chars.peek() {
                if next.is_ascii_digit() {
                    digits.push(*next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !digits.is_empty() {
                return Some(digits);
            }
        }
    }
    None
}

fn extract_keyword_number(text: &str, keyword: &str) -> Option<String> {
    let mut search = text;
    while let Some(index) = search.find(keyword) {
        let after = &search[index + keyword.len()..];
        let trimmed = after.trim_start_matches([':', '-', ' ']);
        let mut digits = String::new();
        let mut chars = trimmed.chars();
        while let Some(ch) = chars.next() {
            if ch.is_ascii_digit() {
                digits.push(ch);
            } else if !digits.is_empty() {
                break;
            } else if ch == '#' {
                continue;
            } else {
                break;
            }
        }
        if !digits.is_empty() {
            return Some(digits);
        }
        search = &search[index + keyword.len()..];
    }
    None
}

fn first_number(text: &str) -> Option<String> {
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn summarize_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
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

fn start_spinner(message: &str) -> Option<ProgressBar> {
    if !env_flag("GG_SPINNER", true) {
        return None;
    }

    let spinner = ProgressBar::new_spinner();
    let style = ProgressStyle::with_template("{spinner} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    spinner.set_style(style);
    spinner.set_message(message.to_string());
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    Some(spinner)
}

struct SpinnerGuard {
    spinner: Option<ProgressBar>,
}

impl SpinnerGuard {
    fn new(spinner: Option<ProgressBar>) -> Self {
        Self { spinner }
    }

    fn finish(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.finish_and_clear();
        }
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

fn env_flag(key: &str, default: bool) -> bool {
    match env::var(key).ok().as_deref() {
        Some("1") | Some("true") | Some("yes") | Some("on") => true,
        Some("0") | Some("false") | Some("no") | Some("off") => false,
        _ => default,
    }
}

fn print_response(response: &Response, compact: bool) {
    if let Some(stdout) = &response.git_stdout {
        if !stdout.trim().is_empty() {
            print!("{stdout}");
        }
    }

    if let Some(stderr) = &response.git_stderr {
        if !stderr.trim().is_empty() {
            eprint!("{stderr}");
        }
    }

    let theme = Theme::from_env();
    let staged_count = response.staged_paths.len();
    if staged_count == 0 {
        let msg = apply_color("no changes", theme.dim).dim();
        println!("{msg}");
        return;
    }

    if compact {
        let summary = response.summary.as_deref().unwrap_or("checkpoint");
        let hash = response
            .commit_hash
            .as_ref()
            .map(|value| value.chars().take(7).collect::<String>())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{} {} {}",
            apply_color("staged", theme.staged).bold(),
            apply_color(format!("{staged_count}"), theme.count),
            apply_color(hash, theme.hash)
        );
        println!("    {}", summary);
        return;
    }

    println!(
        "{} {}",
        apply_color("staged", theme.staged).bold(),
        apply_color(format!("{staged_count} file(s)"), theme.count)
    );

    let max_list = 8;
    for path in response.staged_paths.iter().take(max_list) {
        println!("  + {path}");
    }
    if staged_count > max_list {
        println!("  … and {} more", staged_count - max_list);
    }

    if let Some(hash) = &response.commit_hash {
        let short = hash.chars().take(7).collect::<String>();
        let summary = response.summary.as_deref().unwrap_or("checkpoint");
        println!(
            "{} {} - {}",
            apply_color("committed", theme.committed).bold(),
            apply_color(short, theme.hash),
            summary
        );
    }
}

struct Theme {
    staged: Option<Color>,
    count: Option<Color>,
    committed: Option<Color>,
    hash: Option<Color>,
    dim: Option<Color>,
}

impl Theme {
    fn from_env() -> Self {
        Self {
            staged: color_from_env("GG_COLOR_STAGED", Some(Color::Green)),
            count: color_from_env("GG_COLOR_COUNT", Some(Color::Cyan)),
            committed: color_from_env("GG_COLOR_COMMITTED", Some(Color::Green)),
            hash: color_from_env("GG_COLOR_HASH", Some(Color::Yellow)),
            dim: color_from_env("GG_COLOR_DIM", None),
        }
    }
}

fn apply_color<T: std::fmt::Display>(value: T, color: Option<Color>) -> console::StyledObject<T> {
    let styled = style(value);
    match color {
        Some(color) => styled.fg(color),
        None => styled,
    }
}

fn color_from_env(key: &str, default: Option<Color>) -> Option<Color> {
    let value = match env::var(key) {
        Ok(value) => value.to_ascii_lowercase(),
        Err(_) => return default,
    };
    if value == "none" || value == "off" || value == "false" {
        return None;
    }

    match value.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "brightblack" | "gray" | "grey" => Some(Color::Color256(8)),
        "brightred" => Some(Color::Color256(9)),
        "brightgreen" => Some(Color::Color256(10)),
        "brightyellow" => Some(Color::Color256(11)),
        "brightblue" => Some(Color::Color256(12)),
        "brightmagenta" => Some(Color::Color256(13)),
        "brightcyan" => Some(Color::Color256(14)),
        "brightwhite" => Some(Color::Color256(15)),
        _ => default,
    }
}
