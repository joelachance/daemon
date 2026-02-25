use crate::git;
use crate::session::{TokenUsage, ToolTokenUsage};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
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
    cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    ok: bool,
    message: String,
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
}

pub fn run_daemon() -> Result<(), String> {
    let socket = socket_path();
    if Path::new(&socket).exists() {
        fs::remove_file(&socket).map_err(|err| err.to_string())?;
    }

    let listener = UnixListener::bind(&socket).map_err(|err| err.to_string())?;
    eprintln!("gg daemon: listening on {socket}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_stream(stream) {
                    eprintln!("gg daemon: {err}");
                }
            }
            Err(err) => {
                eprintln!("gg daemon: {err}");
            }
        }
    }

    Ok(())
}

pub fn run_tool(tool: &str, args: &[String]) -> Result<(), String> {
    ensure_daemon_running()?;
    eprintln!("gg: launching tool: {tool}");

    let mut cmd = Command::new(tool);
    cmd.args(args);

    match cmd.status() {
        Ok(status) => {
            if !status.success() {
                return Err(format!("{tool} exited with status {status}"));
            }
            Ok(())
        }
        Err(_) => {
            eprintln!("gg: tool '{tool}' not found on PATH (stub)\n");
            Ok(())
        }
    }
}

pub fn send_event(
    session_id: &str,
    summary: &str,
    paths: &[String],
    tokens: Option<TokenUsage>,
    tool_tokens: Vec<ToolTokenUsage>,
) -> Result<(), String> {
    ensure_daemon_running()?;

    let cwd = env::current_dir()
        .map_err(|err| err.to_string())?
        .to_string_lossy()
        .to_string();

    let request = Request {
        kind: "event".to_string(),
        session_id: Some(session_id.to_string()),
        summary: Some(summary.to_string()),
        paths: Some(paths.to_vec()),
        tokens,
        tool_tokens: Some(tool_tokens),
        cwd: Some(cwd),
    };

    let socket = socket_path();
    let mut stream =
        UnixStream::connect(&socket).map_err(|err| format!("connect: {err}"))?;

    let payload = serde_json::to_vec(&request).map_err(|err| err.to_string())?;
    stream.write_all(&payload).map_err(|err| err.to_string())?;
    stream.shutdown(std::net::Shutdown::Write).map_err(|err| err.to_string())?;

    let mut response_buf = String::new();
    stream.read_to_string(&mut response_buf).map_err(|err| err.to_string())?;
    let response: Response =
        serde_json::from_str(&response_buf).map_err(|err| err.to_string())?;

    if response.ok {
        Ok(())
    } else {
        Err(response.message)
    }
}

fn handle_stream(mut stream: UnixStream) -> Result<(), String> {
    let mut buffer = String::new();
    stream.read_to_string(&mut buffer).map_err(|err| err.to_string())?;
    if buffer.trim().is_empty() {
        return Ok(());
    }
    let request: Request = serde_json::from_str(&buffer).map_err(|err| err.to_string())?;

    let response = match request.kind.as_str() {
        "event" => handle_event(&request),
        "ping" => Ok("pong".to_string()),
        _ => Err("unsupported request".to_string()),
    };

    let response = match response {
        Ok(message) => Response { ok: true, message },
        Err(message) => Response { ok: false, message },
    };

    let response_json = serde_json::to_vec(&response).map_err(|err| err.to_string())?;
    stream.write_all(&response_json).map_err(|err| err.to_string())?;
    Ok(())
}

fn handle_event(request: &Request) -> Result<String, String> {
    let session_id = request
        .session_id
        .as_ref()
        .ok_or("missing session_id")?;
    let summary = request.summary.as_ref().ok_or("missing summary")?;
    let tool_tokens = request.tool_tokens.clone().unwrap_or_default();

    let cwd = match &request.cwd {
        Some(value) => value.clone(),
        None => env::current_dir()
            .map_err(|err| err.to_string())?
            .to_string_lossy()
            .to_string(),
    };

    let requested_paths = request.paths.clone().unwrap_or_default();
    let mut stage_paths = if requested_paths.is_empty() {
        let root = git::repo_root()?;
        let mut paths = git::list_changed_paths()?;
        paths = git::filter_paths_to_cwd(&root, Path::new(&cwd), &paths)?;
        paths
    } else {
        requested_paths
    };

    stage_paths = git::filter_ignored_paths(&stage_paths)?;

    if stage_paths.is_empty() {
        eprintln!("gg: no changes; auto-checkpoint skipped");
    } else {
        eprintln!("gg: auto-checkpoint ({} files)", stage_paths.len());
        git::stage_paths(&stage_paths)?;
    }

    let trailers = [
        ("AI-Session", session_id.as_str()),
        ("AI-Tool", "gg"),
        ("AI-Schema", "1"),
    ];

    let commit_hash = git::commit(summary, &trailers)?;
    if commit_hash.is_none() {
        eprintln!("gg: no staged changes; commit skipped");
    }

    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|err| err.to_string())?;

    let payload = SessionEventPayload {
        schema: 1,
        session_id: session_id.to_string(),
        summary: summary.to_string(),
        commit: commit_hash.clone(),
        paths: stage_paths.clone(),
        timestamp,
        tokens: request.tokens.clone(),
        tool_tokens,
    };

    let payload_json = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    let _ = git::append_session_event(session_id, &payload_json)?;

    if let Some(hash) = commit_hash {
        let _ = git::write_notes(NOTES_REF, &hash, &payload_json)?;
    }

    Ok("event stored".to_string())
}

fn ensure_daemon_running() -> Result<(), String> {
    let socket = socket_path();
    if let Ok(mut stream) = UnixStream::connect(&socket) {
        let ping = Request {
            kind: "ping".to_string(),
            session_id: None,
            summary: None,
            paths: None,
            tokens: None,
            tool_tokens: None,
            cwd: None,
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
        .arg("--daemon")
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

fn socket_path() -> String {
    env::var("GG_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string())
}
