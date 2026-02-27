use crate::claude;
use crate::cursor;
use crate::git::{self, GitOutput};
use crate::opencode;
use crate::session::{TokenUsage, ToolTokenUsage};
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
const NOTES_REF: &str = "refs/notes/gg";

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
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionRegistry {
    sessions: HashMap<String, SessionState>,
}

pub fn run_daemon() -> Result<(), String> {
    let socket = socket_path();
    if Path::new(&socket).exists() {
        fs::remove_file(&socket).map_err(|err| err.to_string())?;
    }

    let listener = UnixListener::bind(&socket).map_err(|err| err.to_string())?;
    eprintln!("daemon: listening on {socket}");

    let registry = Arc::new(Mutex::new(load_registry()));
    start_stdin_thread(registry.clone());
    start_cursor_poll_thread();
    start_claude_poll_thread();
    start_opencode_poll_thread();

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
    let summary = derive_summary(request.summary.as_deref(), request.meta.as_ref());
    let tool_tokens = request.tool_tokens.clone().unwrap_or_default();
    let git_stdout = request.git_stdout.unwrap_or(false);

    let cwd = match &request.cwd {
        Some(value) => value.clone(),
        None => env::current_dir()
            .map_err(|err| err.to_string())?
            .to_string_lossy()
            .to_string(),
    };
    let root = git::repo_root_from(&cwd).or_else(|_| git::repo_root())?;
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

    let (commit_hash, commit_output) = git::commit_in_root(&root, &summary, &trailers)?;

    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|err| err.to_string())?;

    let payload = SessionEventPayload {
        schema: 1,
        session_id: session_id.to_string(),
        summary: summary.clone(),
        commit: commit_hash.clone(),
        paths: stage_paths.clone(),
        timestamp,
        tokens: request.tokens.clone(),
        tool_tokens,
        meta: request.meta.clone(),
    };

    let payload_json = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    let _ = git::append_session_event_in_root(&root, session_id, &payload_json)?;

    if let Some(ref hash) = commit_hash {
        let _ = git::write_notes_in_root(&root, NOTES_REF, hash, &payload_json)?;
    }

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
        summary: Some(summary),
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
        let _ =
            git::append_session_event_in_root(&session.root, &session.session_id, &payload_json)?;

        session.last_activity = now;
        session.explicit_end = true;
        session.soft_end = false;
        session.active = false;
    }

    save_registry(&guard)?;
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
    Command::new(exe).spawn().map_err(|err| err.to_string())?;

    for _ in 0..20 {
        if UnixStream::connect(&socket).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    Err("daemon failed to start".to_string())
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

fn derive_summary(summary: Option<&str>, meta: Option<&serde_json::Value>) -> String {
    let prompt_text = meta.and_then(extract_prompt_text);
    let response_text = meta.and_then(extract_response_text);

    if let Some(text) = prompt_text.as_deref().and_then(summarize_text) {
        return text;
    }
    if let Some(text) = response_text.as_deref().and_then(summarize_text) {
        return text;
    }
    if let Some(value) = summary {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "assistant response".to_string()
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
    });

    entry.root = root.to_string();
    entry.last_activity = now;
    if explicit_end {
        entry.explicit_end = true;
        entry.soft_end = false;
        entry.active = false;
    } else if soft_end {
        entry.soft_end = true;
        entry.active = false;
    } else {
        entry.explicit_end = false;
        entry.soft_end = false;
        entry.active = true;
    }

    save_registry(&guard)
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

fn registry_path() -> Option<std::path::PathBuf> {
    let home = env::var("HOME").ok()?;
    let dir = Path::new(&home).join(".gg");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("daemon.json"))
}

fn load_registry() -> SessionRegistry {
    let path = match registry_path() {
        Some(path) => path,
        None => return SessionRegistry::default(),
    };
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return SessionRegistry::default(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_registry(registry: &SessionRegistry) -> Result<(), String> {
    let path = match registry_path() {
        Some(path) => path,
        None => return Ok(()),
    };
    let data = serde_json::to_string_pretty(registry).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
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
