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
    let mut last_frame = String::new();

    loop {
        let sessions = store::list_sessions()?;
        if cursor >= sessions.len() && !sessions.is_empty() {
            cursor = sessions.len() - 1;
        }
        let frame = build_frame(&sessions, cursor, status_msg.as_deref())?;
        if frame != last_frame {
            render(&mut stdout, &frame)?;
            last_frame = frame;
        }

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

fn render(stdout: &mut io::Stdout, frame: &str) -> Result<(), String> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::FromCursorDown)
    )
    .map_err(|err| err.to_string())?;
    write!(stdout, "{frame}").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn build_frame(
    sessions: &[SessionInfo],
    cursor_pos: usize,
    status_msg: Option<&str>,
) -> Result<String, String> {
    let mut output = String::new();
    output.push_str("gg live\n");
    output.push_str("arrows/kj move  enter review (ended only)  ctrl+c end all  q quit\n\n");

    if sessions.is_empty() {
        output.push_str("no sessions found\n");
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
        output.push_str(&format!(
            "{} {}  {} events  last {}  {}\n",
            cursor_mark, session.id, session.event_count, last_event, end_label
        ));

        let commits = load_recent_commits(&session.id, 6)?;
        for commit in commits {
            output.push_str(&format!(
                "    {}  {}\n",
                short_hash(&commit.commit),
                commit.summary
            ));
        }
    }

    if let Some(message) = status_msg {
        output.push('\n');
        output.push_str(message);
        output.push('\n');
    }

    Ok(output)
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
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Hide);
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
