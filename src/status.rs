use crate::daemon;
use crate::git;
use crate::store;
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal;
use std::io::{self, Write};

pub fn run_status_ui() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let root = git::repo_root()?;
    let sessions = store::list_sessions_for_repo(&root)?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        println!("{}", session.id);
        let drafts = store::list_drafts(&session.id)?;
        for draft in drafts {
            println!("  - {} ({})", draft.id, draft.message);
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn run_review_for_session(session_id: &str) -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let drafts = store::list_drafts(session_id)?;
    if drafts.is_empty() {
        println!("no drafts");
        return Ok(());
    }
    let mut stdout = io::stdout();
    let _guard = RawModeGuard::new()?;
    let mut cursor_idx = 0usize;
    let mut selected = Vec::<String>::new();
    loop {
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::All)
        )
        .map_err(|err| err.to_string())?;
        writeln!(stdout, "review session {session_id}").map_err(|err| err.to_string())?;
        writeln!(stdout, "up/down move  space select  a approve  q quit")
            .map_err(|err| err.to_string())?;
        writeln!(stdout).map_err(|err| err.to_string())?;
        for (idx, draft) in drafts.iter().enumerate() {
            let c = if idx == cursor_idx { ">" } else { " " };
            let mark = if selected.contains(&draft.id) { "[x]" } else { "[ ]" };
            writeln!(stdout, "{} {} {}", c, mark, draft.message).map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        let event = event::read().map_err(|err| err.to_string())?;
        if let Event::Key(key) = event {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Up => cursor_idx = cursor_idx.saturating_sub(1),
                KeyCode::Down => {
                    if cursor_idx + 1 < drafts.len() {
                        cursor_idx += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(draft) = drafts.get(cursor_idx) {
                        if selected.contains(&draft.id) {
                            selected.retain(|value| value != &draft.id);
                        } else {
                            selected.push(draft.id.clone());
                        }
                    }
                }
                KeyCode::Char('a') => {
                    let ids = if selected.is_empty() {
                        None
                    } else {
                        Some(selected.clone())
                    };
                    let result = daemon::approve_drafts(session_id, ids, None)?;
                    drop(_guard);
                    println!("approved {} commit(s)", result.len());
                    return Ok(());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
struct RawModeGuard;
impl RawModeGuard {
    #[allow(dead_code)]
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
