use crate::daemon;
use crate::session_row;
use crate::store;
use std::io::{self, Write};
use time::OffsetDateTime;

pub fn run_dashboard() -> Result<(), String> {
    daemon::ensure_daemon_running()?;

    loop {
        print!("\x1B[2J\x1B[H");
        println!("vibe dashboard");
        println!("commands: r=refresh  a <index>=approve all  q=quit");
        println!();
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(120);

        let now = OffsetDateTime::now_utc().unix_timestamp();
        let window_secs = active_window_secs();
        let sessions = store::list_active_cursor_sessions(now, window_secs)?;
        if sessions.is_empty() {
            println!("(no sessions)");
        } else {
            for (idx, session) in sessions.iter().enumerate() {
                let state = if session.ended_at.is_some() {
                    "ended"
                } else {
                    "active"
                };
                let row = session_row::format_session_columns(session, width, Some(idx));
                println!("{row}  {state}");
                let drafts = store::list_drafts(&session.id)?;
                if drafts.is_empty() {
                    println!("    - (expected commits pending)");
                } else {
                    for draft in drafts {
                        println!("    - {}", draft.message);
                    }
                }
                println!();
            }
        }

        print!("> ");
        io::stdout().flush().map_err(|err| err.to_string())?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|err| err.to_string())?;
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("q") {
            break;
        }
        if trimmed.eq_ignore_ascii_case("r") || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("a ") {
            let idx = rest
                .trim()
                .parse::<usize>()
                .map_err(|_| "invalid index".to_string())?;
            let session = sessions.get(idx).ok_or("session index out of range")?;
            match daemon::approve_drafts(&session.id, None, None) {
                Ok(commits) => {
                    println!("approved {} commit(s)", commits.len());
                }
                Err(err) => {
                    println!("approve failed: {err}");
                }
            }
            println!("press enter to continue...");
            let mut pause = String::new();
            let _ = io::stdin().read_line(&mut pause);
            continue;
        }
    }

    Ok(())
}

fn active_window_secs() -> i64 {
    std::env::var("GG_ACTIVE_WINDOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(900)
}
