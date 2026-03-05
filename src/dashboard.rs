use crate::daemon;
use crate::git;
use crate::store;
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::style::{Color, Stylize};
use crossterm::terminal;
use std::io::{self, Write};

pub fn run_dashboard() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let root = git::repo_root()?;
    let mut stdout = io::stdout();
    let _guard = RawModeGuard::new()?;
    let mut cursor_idx = 0usize;
    let mut status = String::new();

    loop {
        let sessions = store::list_sessions_for_repo(&root)?;
        if cursor_idx >= sessions.len() && !sessions.is_empty() {
            cursor_idx = sessions.len() - 1;
        }
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::All)
        )
        .map_err(|err| err.to_string())?;
        writeln!(stdout, "vibe dashboard").map_err(|err| err.to_string())?;
        writeln!(stdout, "up/down move  enter approve-all  q quit")
            .map_err(|err| err.to_string())?;
        writeln!(stdout).map_err(|err| err.to_string())?;
        for (idx, session) in sessions.iter().enumerate() {
            let mark = if idx == cursor_idx { ">" } else { " " };
            let branch = session
                .confirmed_branch
                .as_deref()
                .unwrap_or(&session.suggested_branch);
            let ended = if session.ended_at.is_some() {
                "ended".with(Color::Red)
            } else {
                "active".with(Color::Green)
            };
            writeln!(
                stdout,
                "{} {} [{}] {}",
                mark,
                session.id,
                branch,
                ended
            )
            .map_err(|err| err.to_string())?;
            let drafts = store::list_drafts(&session.id)?;
            for draft in drafts {
                writeln!(stdout, "    - {}", draft.message).map_err(|err| err.to_string())?;
            }
        }
        if !status.is_empty() {
            writeln!(stdout).map_err(|err| err.to_string())?;
            writeln!(stdout, "{}", status.clone().with(Color::Yellow))
                .map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;

        let evt = event::read().map_err(|err| err.to_string())?;
        if let Event::Key(key) = evt {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Up => {
                    cursor_idx = cursor_idx.saturating_sub(1);
                }
                KeyCode::Down => {
                    if cursor_idx + 1 < sessions.len() {
                        cursor_idx += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(session) = sessions.get(cursor_idx) {
                        match daemon::approve_drafts(&session.id, None, None) {
                            Ok(commits) => {
                                status = format!("approved {} commit(s)", commits.len());
                            }
                            Err(err) => status = err,
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
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
    }
}
