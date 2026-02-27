use crate::daemon;
use crate::status;
use crate::store::{self, EndStatus, SessionCommit, SessionInfo};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal;
use std::io::{self, Write};
use std::time::Duration;

pub fn run_dashboard() -> Result<(), String> {
    let mut stdout = io::stdout();
    let mut guard = RawModeGuard::new()?;
    let mut cursor = 0usize;
    let mut status_msg: Option<String> = None;

    loop {
        let sessions = store::list_sessions()?;
        if cursor >= sessions.len() && !sessions.is_empty() {
            cursor = sessions.len() - 1;
        }
        render(&mut stdout, &sessions, cursor, status_msg.as_deref())?;

        if event::poll(Duration::from_millis(600)).map_err(|err| err.to_string())? {
            if let Event::Key(key) = event::read().map_err(|err| err.to_string())? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => {
                        if cursor > 0 {
                            cursor -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if cursor + 1 < sessions.len() {
                            cursor += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(session) = sessions.get(cursor) {
                            match store::session_end_status(&session.id)? {
                                Some(_) => {
                                    drop(guard);
                                    let _ = status::run_review_for_session(&session.id);
                                    guard = RawModeGuard::new()?;
                                    status_msg = Some("returned from review".to_string());
                                }
                                None => {
                                    status_msg =
                                        Some("session still active (read-only)".to_string());
                                }
                            }
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        daemon::end_all_sessions()?;
                        status_msg = Some("sent end signal".to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn render(
    stdout: &mut io::Stdout,
    sessions: &[SessionInfo],
    cursor_pos: usize,
    status_msg: Option<&str>,
) -> Result<(), String> {
    execute!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )
    .map_err(|err| err.to_string())?;

    writeln!(stdout, "gg live").map_err(|err| err.to_string())?;
    writeln!(
        stdout,
        "arrows/kj move  enter review (ended only)  ctrl+c end all  q quit"
    )
    .map_err(|err| err.to_string())?;
    writeln!(stdout).map_err(|err| err.to_string())?;

    if sessions.is_empty() {
        writeln!(stdout, "no sessions found").map_err(|err| err.to_string())?;
    }

    for (idx, session) in sessions.iter().enumerate() {
        let cursor_mark = if idx == cursor_pos { ">" } else { " " };
        let end_label = match session.end_status {
            Some(EndStatus::Explicit) => "ended",
            Some(EndStatus::Soft) => "soft end",
            None => "active",
        };
        let last_event = session
            .last_event
            .as_ref()
            .map(String::as_str)
            .unwrap_or("unknown");
        writeln!(
            stdout,
            "{} {}  {} events  last {}  {}",
            cursor_mark, session.id, session.event_count, last_event, end_label
        )
        .map_err(|err| err.to_string())?;

        let commits = load_recent_commits(&session.id, 6)?;
        for commit in commits {
            writeln!(
                stdout,
                "    {}  {}",
                short_hash(&commit.commit),
                commit.summary
            )
            .map_err(|err| err.to_string())?;
        }
    }

    if let Some(message) = status_msg {
        writeln!(stdout).map_err(|err| err.to_string())?;
        writeln!(stdout, "{message}").map_err(|err| err.to_string())?;
    }

    stdout.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn load_recent_commits(session_id: &str, limit: usize) -> Result<Vec<SessionCommit>, String> {
    let commits = store::list_session_commits(session_id)?;
    if commits.len() <= limit {
        return Ok(commits);
    }
    Ok(commits[commits.len() - limit..].to_vec())
}

fn short_hash(value: &str) -> String {
    value.chars().take(7).collect::<String>()
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show);
        let _ = stdout.flush();
    }
}
