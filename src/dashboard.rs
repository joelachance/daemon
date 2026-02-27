use crate::daemon;
use crate::status;
use crate::store::{self, EndStatus, SessionCommit, SessionInfo};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::style::{Color, Stylize};
use crossterm::terminal;
use std::fmt::Display;
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
        let width = terminal::size().map(|(w, _)| w as usize).unwrap_or(100);
        let frame = build_frame_key(&sessions, cursor, status_msg.as_deref(), width)?;
        if frame != last_frame {
            render(&mut stdout, &sessions, cursor, status_msg.as_deref(), width)?;
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

fn render(
    stdout: &mut io::Stdout,
    sessions: &[SessionInfo],
    cursor_pos: usize,
    status_msg: Option<&str>,
    width: usize,
) -> Result<(), String> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All)
    )
    .map_err(|err| err.to_string())?;

    write_line(stdout, "gg live".with(Color::Cyan).bold())?;
    write_line(
        stdout,
        "arrows/kj move  enter review (ended only)  ctrl+c end all  q quit".with(Color::DarkGrey),
    )?;
    write_blank_line(stdout)?;

    if sessions.is_empty() {
        write_line(stdout, "no sessions found".with(Color::Yellow))?;
    }

    for (idx, session) in sessions.iter().enumerate() {
        let cursor_char = if idx == cursor_pos { ">" } else { " " };
        let cursor_mark = if idx == cursor_pos {
            cursor_char.with(Color::Green).bold()
        } else {
            cursor_char.with(Color::DarkGrey)
        };
        let end_label = match session.end_status {
            Some(EndStatus::Explicit) | Some(EndStatus::Soft) => "ended".with(Color::Red).bold(),
            None => "active".with(Color::Green),
        };
        let last_event = session
            .last_event
            .as_ref()
            .map(String::as_str)
            .unwrap_or("unknown");
        let header_line = session_header_line(session, width.saturating_sub(2));
        write_line(stdout, format!("{} {}", cursor_mark, header_line))?;
        let status_line = session_status_line(last_event, width);
        write_line(stdout, status_line.with(Color::DarkGrey))?;
        write_line(stdout, end_label)?;

        let commits = load_recent_commits(&session.id, 6)?;
        for commit in commits {
            let commit_line = commit_line(&commit, width);
            write_line(stdout, commit_line)?;
        }
    }

    if let Some(message) = status_msg {
        write_blank_line(stdout)?;
        write_line(stdout, message.with(Color::Yellow))?;
    }

    stdout.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn write_line(stdout: &mut io::Stdout, content: impl Display) -> Result<(), String> {
    write!(stdout, "\r{}\r\n", content).map_err(|err| err.to_string())
}

fn write_blank_line(stdout: &mut io::Stdout) -> Result<(), String> {
    write!(stdout, "\r\n").map_err(|err| err.to_string())
}

fn build_frame_key(
    sessions: &[SessionInfo],
    cursor_pos: usize,
    status_msg: Option<&str>,
    width: usize,
) -> Result<String, String> {
    let mut output = String::new();
    output.push_str("gg live\n");
    output.push_str("arrows/kj move  enter review (ended only)  ctrl+c end all  q quit\n\n");

    if sessions.is_empty() {
        output.push_str("no sessions found\n");
    }

    for (idx, session) in sessions.iter().enumerate() {
        let cursor_mark = if idx == cursor_pos { ">" } else { " " };
        let header_line = session_header_line_plain(session, width.saturating_sub(2));
        output.push_str(&format!("{} {}", cursor_mark, header_line));
        output.push('\n');
        output.push_str(&session_status_line(
            session
                .last_event
                .as_ref()
                .map(String::as_str)
                .unwrap_or("unknown"),
            width,
        ));
        output.push('\n');
        let end_label = match session.end_status {
            Some(EndStatus::Explicit) | Some(EndStatus::Soft) => "ended",
            None => "active",
        };
        output.push_str(end_label);
        output.push('\n');

        let commits = load_recent_commits(&session.id, 6)?;
        for commit in commits {
            output.push_str(&commit_line_plain(&commit, width));
            output.push('\n');
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

fn session_header_line(session: &SessionInfo, width: usize) -> String {
    let name = session
        .display_name
        .as_deref()
        .unwrap_or("untitled session");
    let source = source_label(session.source.as_deref());
    let prefix = format!("[{source}] ");
    let available = width.saturating_sub(prefix.chars().count());
    let title = truncate_to_width(name, available);
    format!("{}{}", prefix.with(Color::Cyan), title)
}

fn session_header_line_plain(session: &SessionInfo, width: usize) -> String {
    let name = session
        .display_name
        .as_deref()
        .unwrap_or("untitled session");
    let source = source_label(session.source.as_deref());
    let prefix = format!("[{source}] ");
    let available = width.saturating_sub(prefix.chars().count());
    let title = truncate_to_width(name, available);
    format!("{prefix}{title}")
}

fn source_label(source: Option<&str>) -> &'static str {
    match source.unwrap_or("") {
        "cursor" => "Cursor",
        "opencode" => "OpenCode",
        "claude" => "Claude",
        "stdin" => "CLI",
        "daemon" => "Daemon",
        "" => "Unknown",
        _ => "Unknown",
    }
}

fn session_status_line(last_event: &str, width: usize) -> String {
    let line = format!("    last {}", last_event);
    truncate_to_width(&line, width)
}

fn commit_line(commit: &SessionCommit, width: usize) -> String {
    let prefix = format!("    {} ", short_hash(&commit.commit));
    let available = width.saturating_sub(prefix.chars().count());
    let summary = truncate_to_width(&commit.summary, available);
    format!(
        "{}{}",
        prefix.with(Color::DarkGrey),
        summary.with(Color::White)
    )
}

fn commit_line_plain(commit: &SessionCommit, width: usize) -> String {
    let prefix = format!("    {} ", short_hash(&commit.commit));
    let available = width.saturating_sub(prefix.chars().count());
    let summary = truncate_to_width(&commit.summary, available);
    format!("{}{}", prefix, summary)
}

fn truncate_to_width(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }
    if max_len <= 3 {
        return value.chars().take(max_len).collect::<String>();
    }
    let mut out = value
        .chars()
        .take(max_len.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|err| err.to_string())?;
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Hide, terminal::EnterAlternateScreen);
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = stdout.flush();
    }
}
