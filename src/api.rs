use crate::daemon;
use crate::store;
use std::io::{Read, Write};
use std::net::TcpListener;

pub fn run_api_server() -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:7340").map_err(|err| err.to_string())?;
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(value) => value,
            Err(err) => {
                eprintln!("api: {err}");
                continue;
            }
        };
        let mut buffer = [0u8; 16384];
        let bytes = match stream.read(&mut buffer) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if bytes == 0 {
            continue;
        }
        let request = String::from_utf8_lossy(&buffer[..bytes]).to_string();
        let mut lines = request.lines();
        let first = match lines.next() {
            Some(value) => value,
            None => continue,
        };
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/");
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("").trim();
        let (status, payload) = handle(method, path, body);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(response.as_bytes());
    }
    Ok(())
}

fn handle(method: &str, path: &str, body: &str) -> (u16, String) {
    if method == "GET" && path == "/sessions" {
        match crate::git::repo_root()
            .and_then(|root| store::list_sessions_for_repo(&root))
            .and_then(|items| serde_json::to_string(&items).map_err(|err| err.to_string()))
        {
            Ok(value) => return (200, value),
            Err(err) => return error(err),
        }
    }
    if method == "GET" && path.starts_with("/sessions/") {
        let pieces: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if pieces.len() == 2 {
            match store::get_session(pieces[1])
                .and_then(|session| serde_json::to_string(&session).map_err(|err| err.to_string()))
            {
                Ok(value) => return (200, value),
                Err(err) => return error(err),
            }
        }
        if pieces.len() == 3 && pieces[2] == "drafts" {
            match store::list_drafts(pieces[1])
                .and_then(|drafts| serde_json::to_string(&drafts).map_err(|err| err.to_string()))
            {
                Ok(value) => return (200, value),
                Err(err) => return error(err),
            }
        }
        if pieces.len() == 4 && pieces[2] == "changes" && pieces[3] == "unassigned" {
            match store::list_unassigned_changes(pieces[1]).and_then(|changes| {
                serde_json::to_string(&changes).map_err(|err| err.to_string())
            }) {
                Ok(value) => return (200, value),
                Err(err) => return error(err),
            }
        }
    }
    if method == "PATCH" && path.starts_with("/sessions/") && path.ends_with("/branch") {
        let pieces: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if pieces.len() == 3 {
            let json: serde_json::Value = match serde_json::from_str(body) {
                Ok(value) => value,
                Err(err) => return error(err.to_string()),
            };
            let branch = json
                .get("branch")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let ticket = json.get("ticket").and_then(|value| value.as_str());
            let result = store::set_session_branch(pieces[1], branch)
                .and_then(|_| store::set_session_ticket(pieces[1], ticket));
            return match result {
                Ok(_) => (200, "{\"ok\":true}".to_string()),
                Err(err) => error(err),
            };
        }
    }
    if method == "POST" && path.starts_with("/sessions/") && path.ends_with("/drafts/approve") {
        let pieces: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if pieces.len() == 4 {
            let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let ids = json
                .get("draft_ids")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                });
            let branch = json
                .get("branch")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            return match daemon::approve_drafts(pieces[1], ids, branch) {
                Ok(commits) => (
                    200,
                    serde_json::to_string(&serde_json::json!({ "commits": commits }))
                        .unwrap_or_else(|_| "{\"ok\":true}".to_string()),
                ),
                Err(err) => error(err),
            };
        }
    }
    (404, "{\"error\":\"not found\"}".to_string())
}

fn error(message: String) -> (u16, String) {
    (
        500,
        serde_json::to_string(&serde_json::json!({ "error": message }))
            .unwrap_or_else(|_| "{\"error\":\"internal\"}".to_string()),
    )
}
