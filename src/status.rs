use crate::daemon;
use crate::git;
use crate::store::{self, EndStatus, SessionCommit, SessionInfo};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Squash,
    Amend,
    Undo,
}

enum View {
    Sessions {
        sessions: Vec<SessionInfo>,
        cursor: usize,
        status: Option<String>,
    },
    Active {
        session: SessionInfo,
        status: Option<String>,
    },
    Commits {
        session: SessionInfo,
        commits: Vec<SessionCommit>,
        selected: Vec<bool>,
        cursor: usize,
        pending_action: Option<Action>,
        status: Option<String>,
    },
}

pub fn run_status_ui() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let sessions = store::list_sessions()?;

    if sessions.is_empty() {
        println!("gg status: no sessions found");
        return Ok(());
    }

    let mut stdout = io::stdout();
    let _guard = RawModeGuard::new()?;
    let mut view = View::Sessions {
        sessions,
        cursor: 0,
        status: None,
    };

    let mut exit_message: Option<String> = None;

    loop {
        render_view(&mut stdout, &view)?;
        let event = event::read().map_err(|err| err.to_string())?;
        if let Event::Key(key) = event {
            match view {
                View::Sessions {
                    ref sessions,
                    ref mut cursor,
                    ref mut status,
                } => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *cursor > 0 {
                            *cursor -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *cursor + 1 < sessions.len() {
                            *cursor += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(session) = sessions.get(*cursor).cloned() {
                            match store::session_ended(&session.id) {
                                Ok(true) => match store::list_session_commits(&session.id) {
                                    Ok(commits) => {
                                        if commits.is_empty() {
                                            *status = Some("no commits for session".to_string());
                                        } else {
                                            view = View::Commits {
                                                session,
                                                selected: vec![false; commits.len()],
                                                commits,
                                                cursor: 0,
                                                pending_action: None,
                                                status: None,
                                            };
                                        }
                                    }
                                    Err(err) => *status = Some(err),
                                },
                                Ok(false) => {
                                    view = View::Active {
                                        session,
                                        status: None,
                                    };
                                }
                                Err(err) => *status = Some(err),
                            }
                        }
                    }
                    _ => {}
                },
                View::Active {
                    session: _,
                    ref mut status,
                } => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ => {
                        let sessions = store::list_sessions()?;
                        view = View::Sessions {
                            sessions,
                            cursor: 0,
                            status: status.take(),
                        };
                    }
                },
                View::Commits {
                    ref session,
                    ref mut commits,
                    ref mut selected,
                    ref mut cursor,
                    ref mut pending_action,
                    ref mut status,
                } => {
                    let commit_len = commits.len();
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Up | KeyCode::Char('k') => {
                            if *cursor > 0 {
                                *cursor -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if *cursor + 1 < commit_len {
                                *cursor += 1;
                            }
                        }
                        KeyCode::Char(' ') => {
                            if let Some(slot) = selected.get_mut(*cursor) {
                                *slot = !*slot;
                                normalize_selection(selected);
                            }
                        }
                        KeyCode::Char('s') => {
                            *pending_action = Some(Action::Squash);
                        }
                        KeyCode::Char('a') => {
                            *pending_action = Some(Action::Amend);
                        }
                        KeyCode::Char('u') => {
                            *pending_action = Some(Action::Undo);
                        }
                        KeyCode::Enter => {
                            if pending_action.is_none() {
                                let mut messages = vec!["accepted".to_string()];
                                match maybe_push() {
                                    Ok(push_message) => messages.push(push_message),
                                    Err(err) => messages.push(err),
                                }
                                exit_message = Some(messages.join("\n"));
                                break;
                            }

                            if !selected.iter().any(|value| *value) {
                                if let Some(slot) = selected.get_mut(*cursor) {
                                    *slot = true;
                                }
                            }
                            normalize_selection(selected);
                            let action = pending_action.unwrap_or(Action::Squash);
                            match apply_action(action, session, commits, selected) {
                                Ok(message) => {
                                    let mut messages = vec![message];
                                    match maybe_push() {
                                        Ok(push_message) => messages.push(push_message),
                                        Err(err) => messages.push(err),
                                    }
                                    exit_message = Some(messages.join("\n"));
                                    break;
                                }
                                Err(err) => {
                                    *status = Some(err);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    drop(_guard);
    if let Some(message) = exit_message {
        println!("{message}");
    }
    Ok(())
}

pub fn run_review_for_session(session_id: &str) -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let ended = store::session_end_status(session_id)?;
    if ended.is_none() {
        return Err("session still active".to_string());
    }
    let sessions = store::list_sessions()?;
    let session = sessions
        .into_iter()
        .find(|item| item.id == session_id)
        .ok_or_else(|| "session not found".to_string())?;
    let commits = store::list_session_commits(&session.id)?;
    if commits.is_empty() {
        return Err("no commits for session".to_string());
    }

    let mut stdout = io::stdout();
    let _guard = RawModeGuard::new()?;
    let mut view = View::Commits {
        session,
        selected: vec![false; commits.len()],
        commits,
        cursor: 0,
        pending_action: None,
        status: None,
    };
    let mut exit_message: Option<String> = None;

    loop {
        render_view(&mut stdout, &view)?;
        let event = event::read().map_err(|err| err.to_string())?;
        if let Event::Key(key) = event {
            if let View::Commits {
                ref session,
                ref mut commits,
                ref mut selected,
                ref mut cursor,
                ref mut pending_action,
                ref mut status,
            } = view
            {
                let commit_len = commits.len();
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *cursor > 0 {
                            *cursor -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *cursor + 1 < commit_len {
                            *cursor += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(slot) = selected.get_mut(*cursor) {
                            *slot = !*slot;
                            normalize_selection(selected);
                        }
                    }
                    KeyCode::Char('s') => {
                        *pending_action = Some(Action::Squash);
                    }
                    KeyCode::Char('a') => {
                        *pending_action = Some(Action::Amend);
                    }
                    KeyCode::Char('u') => {
                        *pending_action = Some(Action::Undo);
                    }
                    KeyCode::Enter => {
                        if pending_action.is_none() {
                            exit_message = Some("accepted".to_string());
                            break;
                        }

                        if !selected.iter().any(|value| *value) {
                            if let Some(slot) = selected.get_mut(*cursor) {
                                *slot = true;
                            }
                        }
                        normalize_selection(selected);
                        let action = pending_action.unwrap_or(Action::Squash);
                        match apply_action(action, session, commits, selected) {
                            Ok(message) => {
                                exit_message = Some(message);
                                break;
                            }
                            Err(err) => {
                                *status = Some(err);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    drop(_guard);
    if let Some(message) = exit_message {
        println!("{message}");
    }
    Ok(())
}

fn normalize_selection(selected: &mut [bool]) {
    let mut first = None;
    let mut last = None;
    for (idx, value) in selected.iter().enumerate() {
        if *value {
            if first.is_none() {
                first = Some(idx);
            }
            last = Some(idx);
        }
    }

    let (first, last) = match (first, last) {
        (Some(first), Some(last)) => (first, last),
        _ => return,
    };

    for (idx, slot) in selected.iter_mut().enumerate() {
        *slot = idx >= first && idx <= last;
    }
}

fn apply_action(
    action: Action,
    session: &SessionInfo,
    commits: &[SessionCommit],
    selected: &[bool],
) -> Result<String, String> {
    if commits.is_empty() {
        return Err("no commits to review".to_string());
    }

    match action {
        Action::Undo => undo_all(commits),
        Action::Squash => squash_selected(session, commits, selected),
        Action::Amend => amend_selected(commits, selected),
    }
}

fn undo_all(commits: &[SessionCommit]) -> Result<String, String> {
    let first = commits
        .first()
        .ok_or_else(|| "no commits to undo".to_string())?;
    let base =
        git::commit_parent(&first.commit)?.ok_or_else(|| "cannot undo root commit".to_string())?;
    git::reset_soft_to(&base)?;
    Ok("reset to session base; all changes staged".to_string())
}

fn squash_selected(
    _session: &SessionInfo,
    commits: &[SessionCommit],
    selected: &[bool],
) -> Result<String, String> {
    let (start, end) = selection_range(selected)?;
    if start == end {
        return Err("select at least two commits to squash".to_string());
    }
    ensure_selection_includes_head(commits, end)?;
    let base = git::commit_parent(&commits[start].commit)?
        .ok_or_else(|| "cannot squash root commit".to_string())?;
    git::reset_soft_to(&base)?;

    let message = commits[start].summary.clone();
    git::commit_with_message(&message, false)?;
    Ok("squash complete".to_string())
}

fn amend_selected(commits: &[SessionCommit], selected: &[bool]) -> Result<String, String> {
    let (start, _) = selection_range(selected)?;
    if start + 1 != commits.len() {
        return Err("amend requires selecting the latest commit".to_string());
    }
    let head = git::head_commit()?;
    if commits[start].commit != head {
        return Err("amend requires selecting the latest commit".to_string());
    }
    let message = git::commit_subject(&commits[start].commit)?;
    git::commit_with_message(message.trim(), true)?;
    Ok("amend complete".to_string())
}

fn selection_range(selected: &[bool]) -> Result<(usize, usize), String> {
    let mut start = None;
    let mut end = None;
    for (idx, value) in selected.iter().enumerate() {
        if *value {
            if start.is_none() {
                start = Some(idx);
            }
            end = Some(idx);
        }
    }
    match (start, end) {
        (Some(start), Some(end)) => Ok((start, end)),
        _ => Err("no commits selected".to_string()),
    }
}

fn maybe_push() -> Result<String, String> {
    if !git::has_remote()? {
        let url = match prompt_remote_url()? {
            Some(value) => value,
            None => return Ok("no remote found; skipped push".to_string()),
        };
        git::add_remote("origin", &url)?;
    }
    if !git::has_remote()? {
        return Ok("no remote found; skipped push".to_string());
    }
    if !git::working_tree_clean()? {
        return Ok("working tree not clean; skipped push".to_string());
    }
    git::push()?;
    Ok("push complete".to_string())
}

fn ensure_selection_includes_head(commits: &[SessionCommit], end: usize) -> Result<(), String> {
    let head = git::head_commit()?;
    let selected = commits
        .get(end)
        .ok_or_else(|| "invalid selection".to_string())?;
    if selected.commit != head {
        return Err("selection must include the latest commit".to_string());
    }
    Ok(())
}

fn prompt_remote_url() -> Result<Option<String>, String> {
    let mut stdout = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = execute!(stdout, cursor::Show);
    write!(stdout, "Remote URL for origin: ").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| err.to_string())?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn render_view(stdout: &mut io::Stdout, view: &View) -> Result<(), String> {
    execute!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )
    .map_err(|err| err.to_string())?;

    match view {
        View::Sessions {
            sessions,
            cursor,
            status,
        } => render_sessions(stdout, sessions, *cursor, status)?,
        View::Active { session, status } => render_active(stdout, session, status)?,
        View::Commits {
            session,
            commits,
            selected,
            cursor,
            pending_action,
            status,
        } => render_commits(
            stdout,
            session,
            commits,
            selected,
            *cursor,
            *pending_action,
            status,
        )?,
    }

    stdout.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn render_sessions(
    stdout: &mut io::Stdout,
    sessions: &[SessionInfo],
    cursor: usize,
    status: &Option<String>,
) -> Result<(), String> {
    writeln!(stdout, "gg status - sessions").map_err(|err| err.to_string())?;
    writeln!(stdout, "Enter: open   q: quit").map_err(|err| err.to_string())?;
    writeln!(stdout).map_err(|err| err.to_string())?;

    for (idx, session) in sessions.iter().enumerate() {
        let cursor_mark = if idx == cursor { ">" } else { " " };
        let last_event = session
            .last_event
            .as_ref()
            .map(String::as_str)
            .unwrap_or("unknown");
        let end_label = match session.end_status {
            Some(EndStatus::Explicit) => " (ended)",
            Some(EndStatus::Soft) => " (soft end)",
            None => "",
        };
        writeln!(
            stdout,
            "{} {}  {} events  last {}",
            cursor_mark,
            session.id,
            session.event_count,
            format!("{last_event}{end_label}")
        )
        .map_err(|err| err.to_string())?;
    }

    if let Some(message) = status {
        writeln!(stdout).map_err(|err| err.to_string())?;
        writeln!(stdout, "{message}").map_err(|err| err.to_string())?;
    }

    Ok(())
}

fn render_active(
    stdout: &mut io::Stdout,
    session: &SessionInfo,
    status: &Option<String>,
) -> Result<(), String> {
    writeln!(stdout, "gg status - active session").map_err(|err| err.to_string())?;
    writeln!(stdout, "{}", session.id).map_err(|err| err.to_string())?;
    writeln!(
        stdout,
        "events: {}  last: {}",
        session.event_count,
        session
            .last_event
            .as_ref()
            .map(String::as_str)
            .unwrap_or("unknown")
    )
    .map_err(|err| err.to_string())?;
    writeln!(stdout, "session is still active").map_err(|err| err.to_string())?;
    writeln!(stdout, "any key: back   q: quit").map_err(|err| err.to_string())?;

    if let Some(message) = status {
        writeln!(stdout).map_err(|err| err.to_string())?;
        writeln!(stdout, "{message}").map_err(|err| err.to_string())?;
    }

    Ok(())
}

fn render_commits(
    stdout: &mut io::Stdout,
    session: &SessionInfo,
    commits: &[SessionCommit],
    selected: &[bool],
    cursor: usize,
    pending_action: Option<Action>,
    status: &Option<String>,
) -> Result<(), String> {
    writeln!(stdout, "gg status - review").map_err(|err| err.to_string())?;
    writeln!(stdout, "session: {}", session.id).map_err(|err| err.to_string())?;
    if let Ok(Some(status)) = store::session_end_status(&session.id) {
        let label = match status {
            EndStatus::Explicit => "ended",
            EndStatus::Soft => "soft end",
        };
        writeln!(stdout, "status: {label}").map_err(|err| err.to_string())?;
    }
    writeln!(
        stdout,
        "arrows/kj move  space select  s squash  a amend  u undo  enter accept as-is  q quit"
    )
    .map_err(|err| err.to_string())?;
    writeln!(stdout).map_err(|err| err.to_string())?;

    for (idx, commit) in commits.iter().enumerate() {
        let cursor_mark = if idx == cursor { ">" } else { " " };
        let selected_mark = if selected.get(idx).copied().unwrap_or(false) {
            "[x]"
        } else {
            "[ ]"
        };
        let short = commit.commit.chars().take(7).collect::<String>();
        writeln!(
            stdout,
            "{} {} {} {}",
            cursor_mark, selected_mark, short, commit.summary
        )
        .map_err(|err| err.to_string())?;
    }

    if let Some(action) = pending_action {
        let label = match action {
            Action::Squash => "pending: squash",
            Action::Amend => "pending: amend",
            Action::Undo => "pending: undo",
        };
        writeln!(stdout).map_err(|err| err.to_string())?;
        writeln!(stdout, "{label}").map_err(|err| err.to_string())?;
    }

    if let Some(message) = status {
        writeln!(stdout).map_err(|err| err.to_string())?;
        writeln!(stdout, "{message}").map_err(|err| err.to_string())?;
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
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show);
        let _ = stdout.flush();
    }
}
