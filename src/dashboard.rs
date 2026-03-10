use crate::daemon;
use crate::daemon_log;
use crate::git;
use crate::grouping;
use crate::llm;
use crate::path;
use crate::session_row;
use crate::store;
use crossterm::event::{poll, read, Event, KeyCode, KeyEventKind};
use std::sync::mpsc;
use std::time::Duration;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, ListItem, List, ListState, Paragraph, Wrap};
use ratatui::Frame;
use time::OffsetDateTime;

mod theme {
    use ratatui::style::{Color, Modifier, Style};

    pub fn header_title() -> Style {
        Style::new().fg(Color::Cyan)
    }
    pub fn header_hint() -> Style {
        Style::new().fg(Color::DarkGray)
    }
    pub fn selected() -> Style {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }
    pub fn ide() -> Style {
        Style::new().fg(Color::DarkGray)
    }
    pub fn repo() -> Style {
        Style::new().fg(Color::White)
    }
    pub fn branch() -> Style {
        Style::new().fg(Color::Green)
    }
    pub fn commit_msg(selected: bool) -> Style {
        if selected {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new().fg(Color::DarkGray)
        }
    }
    pub fn success() -> Style {
        Style::new().fg(Color::Green)
    }
    pub fn error() -> Style {
        Style::new().fg(Color::Red)
    }
}

enum View {
    Sessions { selected: usize },
    Commits { session_idx: usize, selected: usize },
    Diff {
        session_idx: usize,
        draft_idx: usize,
        file_idx: usize,
        scroll_offset: u16,
    },
    EditBranch { session_idx: usize, buffer: String },
    EditCommit { session_idx: usize, draft_idx: usize, buffer: String },
}

struct SlashMenuState {
    level: usize,
    items: Vec<String>,
    selected: usize,
}

pub fn run_dashboard() -> Result<(), String> {
    daemon::ensure_daemon_running()?;

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

const SPINNER: &[&str] = &["|", "/", "-", "\\"];

fn show_daemon_log() -> bool {
    match std::env::var("GG_DAEMON_LOG") {
        Ok(v) => !matches!(v.to_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

fn read_daemon_log_lines() -> Vec<String> {
    let path = daemon_log::log_path_for_reader();
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return vec!["(no log file)".to_string()],
    };
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    if lines.is_empty() {
        return vec!["(empty)".to_string()];
    }
    lines
}

fn dashboard_poll_ms() -> u64 {
    std::env::var("GG_DASHBOARD_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2000)
}

fn run_loop(terminal: &mut ratatui::DefaultTerminal) -> Result<(), String> {
    store::init()?;
    let mut view = View::Sessions { selected: 0 };
    let mut accept_pending = false;
    let mut status: Option<(bool, String)> = None;
    let mut refresh_rx: Option<mpsc::Receiver<()>> = None;
    let mut spinner_frame: usize = 0;
    let mut log_scroll: u16 = 0;
    let mut slash_menu: Option<SlashMenuState> = None;
    let mut sessions: Vec<store::SessionInfo> = Vec::new();
    let mut last_refresh_mtime: Option<std::time::SystemTime> = None;

    loop {
        let should_refetch = last_refresh_mtime.is_none()
            || match (store::refresh_signal_mtime(), last_refresh_mtime) {
                (Some(mtime), Some(last)) if mtime > last => true,
                _ => false,
            };
        if should_refetch {
            let now = OffsetDateTime::now_utc().unix_timestamp();
            let window_secs = active_window_secs();
            sessions = match git::repo_root() {
                Ok(root) => {
                    let p = std::path::Path::new(&root);
                    let canonical = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
                    let repo = path::normalize_repo_path(&canonical.to_string_lossy());
                    store::list_open_sessions_for_repo(&repo)?
                }
                Err(_) => store::list_active_sessions(now, window_secs)?,
            };
            last_refresh_mtime = store::refresh_signal_mtime();
        }

        if let Some(ref rx) = refresh_rx {
            if rx.try_recv().is_ok() {
                refresh_rx = None;
            }
        }
        let refreshing = refresh_rx.is_some();

        terminal
            .draw(|frame| {
                render(
                    frame,
                    &view,
                    &sessions,
                    &accept_pending,
                    &status,
                    refreshing,
                    spinner_frame,
                    log_scroll,
                    &slash_menu,
                )
            })
            .map_err(|e| e.to_string())?;

        let poll_duration = if refreshing {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(dashboard_poll_ms())
        };
        let event = if poll(poll_duration).map_err(|e| e.to_string())? {
            Some(read().map_err(|e| e.to_string())?)
        } else {
            if refreshing {
                spinner_frame = (spinner_frame + 1) % SPINNER.len();
            }
            None
        };

        let key = match event {
            Some(Event::Key(ke)) if ke.kind == KeyEventKind::Press => ke.code,
            Some(_) => continue,
            None => continue,
        };

        let edit_done = match &mut view {
            View::EditBranch { session_idx, buffer } => {
                let idx = *session_idx;
                match key {
                    KeyCode::Char(c) => {
                        buffer.push(c);
                        None
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        None
                    }
                    KeyCode::Enter => {
                        if let Some(session) = sessions.get(idx) {
                            let b = std::mem::take(buffer);
                            if let Err(err) = store::set_session_branch(&session.id, b.trim()) {
                                status = Some((false, err));
                            }
                        }
                        Some(View::Sessions { selected: idx })
                    }
                    KeyCode::Esc => Some(View::Sessions { selected: idx }),
                    _ => None,
                }
            }
            View::EditCommit {
                session_idx,
                draft_idx,
                buffer,
            } => {
                let sidx = *session_idx;
                let didx = *draft_idx;
                match key {
                    KeyCode::Char(c) => {
                        buffer.push(c);
                        None
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        None
                    }
                    KeyCode::Enter => {
                        if let Some(session) = sessions.get(sidx) {
                            if let Ok(drafts) = store::list_drafts(&session.id) {
                                if let Some(draft) = drafts.get(didx) {
                                    let b = std::mem::take(buffer);
                                    if !b.trim().is_empty() {
                                        let _ = store::update_draft_message(&draft.id, b.trim());
                                    }
                                }
                            }
                        }
                        Some(View::Commits {
                            session_idx: sidx,
                            selected: didx,
                        })
                    }
                    KeyCode::Esc => Some(View::Commits {
                        session_idx: sidx,
                        selected: didx,
                    }),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(new_view) = edit_done {
            view = new_view;
            continue;
        }

        if accept_pending {
            match key {
                KeyCode::Enter => {
                    let session_id = match &view {
                        View::Sessions { selected } if *selected < sessions.len() => {
                            sessions[*selected].id.clone()
                        }
                        View::Commits { session_idx, .. } if *session_idx < sessions.len() => {
                            sessions[*session_idx].id.clone()
                        }
                        _ => {
                            accept_pending = false;
                            continue;
                        }
                    };
                    accept_pending = false;
                    match daemon::approve_drafts(&session_id, None, None) {
                        Ok(commits) => {
                            status = Some((true, format!("Accepted: {} commits", commits.len())));
                        }
                        Err(err) => {
                            status = Some((false, format!("Error: {err}")));
                        }
                    }
                }
                KeyCode::Esc => {
                    accept_pending = false;
                }
                _ => {
                    accept_pending = false;
                    status = None;
                }
            }
            continue;
        }

        if status.is_some() {
            status = None;
        }

        if show_daemon_log() {
            if key == KeyCode::Char('J') {
                log_scroll = log_scroll.saturating_sub(1);
                continue;
            }
            if key == KeyCode::Char('K') {
                log_scroll = log_scroll.saturating_add(1);
                continue;
            }
        }

        if key == KeyCode::Char('/') && slash_menu.is_none() {
            slash_menu = Some(SlashMenuState {
                level: 0,
                items: vec!["Models".to_string()],
                selected: 0,
            });
            continue;
        }

        if let Some(mut menu) = slash_menu.take() {
            let mut close_with_status: Option<(bool, String)> = None;
            match key {
                KeyCode::Esc => {
                    if menu.level == 0 {
                        // close menu
                    } else if menu.level == 1 {
                        menu = SlashMenuState {
                            level: 0,
                            items: vec!["Models".to_string()],
                            selected: 0,
                        };
                        slash_menu = Some(menu);
                    } else {
                        menu = SlashMenuState {
                            level: 1,
                            items: vec![
                                "OpenAI".to_string(),
                                "Anthropic".to_string(),
                                "Ollama".to_string(),
                            ],
                            selected: 0,
                        };
                        slash_menu = Some(menu);
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1));
                    slash_menu = Some(menu);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    menu.selected = menu.selected.saturating_sub(1);
                    slash_menu = Some(menu);
                }
                KeyCode::Enter => {
                    let item = menu.items.get(menu.selected).cloned().unwrap_or_default();
                    if menu.level == 0 {
                        if item == "Models" {
                            menu.level = 1;
                            #[cfg(feature = "llama-embedded")]
                            let items = vec![
                                "OpenAI".to_string(),
                                "Anthropic".to_string(),
                                "Llama".to_string(),
                                "Ollama".to_string(),
                            ];
                            #[cfg(not(feature = "llama-embedded"))]
                            let items = vec![
                                "OpenAI".to_string(),
                                "Anthropic".to_string(),
                                "Ollama".to_string(),
                            ];
                            menu.items = items;
                            menu.selected = 0;
                            slash_menu = Some(menu);
                        } else {
                            slash_menu = Some(menu);
                        }
                    } else if menu.level == 1 {
                        match item.as_str() {
                            "OpenAI" => {
                                let _ = store::set_llm_provider("openai");
                                close_with_status = Some((true, "Switched to OpenAI".to_string()));
                            }
                            "Anthropic" => {
                                let _ = store::set_llm_provider("anthropic");
                                close_with_status = Some((true, "Switched to Anthropic".to_string()));
                            }
                            #[cfg(feature = "llama-embedded")]
                            "Llama" => {
                                let _ = store::set_llm_provider("llama");
                                close_with_status = Some((true, "Switched to Llama (embedded)".to_string()));
                            }
                            "Ollama" => {
                                match llm::list_ollama_models_blocking() {
                                    Ok(models) if !models.is_empty() => {
                                        menu.level = 2;
                                        menu.items = models;
                                        menu.selected = 0;
                                        slash_menu = Some(menu);
                                    }
                                    Ok(_) | Err(_) => {
                                        let _ = store::set_llm_provider("ollama");
                                        close_with_status = Some((
                                            true,
                                            "Switched to Ollama (default model)".to_string(),
                                        ));
                                    }
                                }
                            }
                            _ => {
                                slash_menu = Some(menu);
                            }
                        }
                    } else {
                        let _ = store::set_ollama_model(&item);
                        let _ = store::set_llm_provider("ollama");
                        close_with_status = Some((true, format!("Switched to Ollama ({})", item)));
                    }
                    if let Some(s) = close_with_status {
                        status = Some(s);
                    }
                }
                _ => {
                    slash_menu = Some(menu);
                }
            }
            continue;
        }

        let (down, up, back, enter, quit) = match key {
            KeyCode::Char('j') | KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Down => {
                (true, false, false, false, false)
            }
            KeyCode::Char('k') | KeyCode::Up => (false, true, false, false, false),
            KeyCode::Char('h') | KeyCode::Esc => (false, false, true, false, false),
            KeyCode::Char('l') | KeyCode::Enter => (false, false, false, true, false),
            KeyCode::Char('q') => (false, false, false, false, true),
            KeyCode::Char('a') => {
                let can_accept = matches!(&view, View::Sessions { .. } | View::Commits { .. })
                    && !sessions.is_empty()
                    && match &view {
                        View::Sessions { selected } => *selected < sessions.len(),
                        View::Commits { session_idx, .. } => *session_idx < sessions.len(),
                        _ => false,
                    };
                if can_accept {
                    accept_pending = true;
                }
                continue;
            }
            KeyCode::Char('p') => (false, false, false, false, false),
            KeyCode::Char('e') => {
                if let View::Sessions { selected } = &view {
                    if *selected < sessions.len() {
                        let session = &sessions[*selected];
                        let branch = session
                            .confirmed_branch
                            .as_deref()
                            .unwrap_or(&session.suggested_branch)
                            .to_string();
                        let b = if branch.trim().is_empty() {
                            daemon::placeholder_branch_name(&session.id)
                        } else {
                            branch
                        };
                        view = View::EditBranch {
                            session_idx: *selected,
                            buffer: b,
                        };
                    }
                } else if let View::Commits { session_idx, selected } = &view {
                    if *session_idx < sessions.len() {
                        let session = &sessions[*session_idx];
                        if let Ok(drafts) = store::list_drafts(&session.id) {
                            if *selected < drafts.len() {
                                let draft = &drafts[*selected];
                                view = View::EditCommit {
                                    session_idx: *session_idx,
                                    draft_idx: *selected,
                                    buffer: draft.message.clone(),
                                };
                            }
                        }
                    }
                }
                continue;
            }
            KeyCode::Char('r') => {
                // Manual refresh: clear pending refresh so we show current store state
                refresh_rx = None;
                continue;
            }
            _ => continue,
        };

        if quit {
            break;
        }

        if let View::Diff {
            session_idx,
            draft_idx,
            file_idx,
            scroll_offset,
        } = &mut view
        {
            if back || enter {
                view = View::Commits {
                    session_idx: *session_idx,
                    selected: *draft_idx,
                };
                continue;
            }
            let num_files = if let Some(session) = sessions.get(*session_idx) {
                if let Ok(drafts) = store::list_drafts(&session.id) {
                    if let Some(draft) = drafts.get(*draft_idx) {
                        store::draft_change_ids(&draft.id).unwrap_or_default().len()
                    } else {
                        0
                    }
                } else {
                    0
                }
            } else {
                0
            };
            match key {
                KeyCode::Char('j') | KeyCode::Down if num_files > 0 => {
                    *file_idx = (*file_idx + 1).min(num_files.saturating_sub(1));
                    *scroll_offset = 0;
                    continue;
                }
                KeyCode::Char('k') | KeyCode::Up if num_files > 0 => {
                    *file_idx = file_idx.saturating_sub(1);
                    *scroll_offset = 0;
                    continue;
                }
                KeyCode::Char('n') | KeyCode::Char(' ') => {
                    let total_lines = if let Some(session) = sessions.get(*session_idx) {
                        if let Ok(drafts) = store::list_drafts(&session.id) {
                            if let Some(draft) = drafts.get(*draft_idx) {
                                let change_ids =
                                    store::draft_change_ids(&draft.id).unwrap_or_default();
                                if let Some(change_id) = change_ids.get(*file_idx) {
                                    store::get_change(change_id)
                                        .ok()
                                        .flatten()
                                        .map(|c| c.diff.lines().count())
                                        .unwrap_or(0)
                                } else {
                                    0
                                }
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let header_len = if accept_pending { 3 } else { 2 };
                    let viewport_height = terminal
                        .size()
                        .map(|s| s.height.saturating_sub(header_len).saturating_sub(1))
                        .unwrap_or(20);
                    let max_scroll = total_lines
                        .saturating_sub(viewport_height as usize)
                        .min(u16::MAX as usize) as u16;
                    *scroll_offset = (*scroll_offset + 1).min(max_scroll);
                    continue;
                }
                KeyCode::Char('p') => {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                    continue;
                }
                _ => {}
            }
            continue;
        }

        match &mut view {
            View::Diff { .. } => {}
            View::Sessions { selected } => {
                let len = sessions.len();
                if down && len > 0 {
                    *selected = (*selected + 1).min(len - 1);
                } else if up && len > 0 {
                    *selected = selected.saturating_sub(1);
                } else if enter && len > 0 {
                    let session_id = sessions[*selected].id.clone();
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = daemon::send_refresh_drafts(&session_id);
                        let _ = tx.send(());
                    });
                    refresh_rx = Some(rx);
                    let drafts = store::list_drafts(&sessions[*selected].id)?;
                    let commit_selected = if drafts.is_empty() { 0 } else { 0 };
                    view = View::Commits {
                        session_idx: *selected,
                        selected: commit_selected,
                    };
                }
            }
            View::Commits { session_idx, selected } => {
                if back {
                    view = View::Sessions {
                        selected: *session_idx,
                    };
                } else {
                    let session = sessions
                        .get(*session_idx)
                        .ok_or_else(|| "session index out of range".to_string())?;
                    let drafts = store::list_drafts(&session.id)?;
                    let len = drafts.len();
                    if down && len > 0 {
                        *selected = (*selected + 1).min(len - 1);
                    } else if up && len > 0 {
                        *selected = selected.saturating_sub(1);
                    } else if enter && len > 0 {
                        view = View::Diff {
                            session_idx: *session_idx,
                            draft_idx: *selected,
                            file_idx: 0,
                            scroll_offset: 0,
                        };
                    }
                }
            }
            View::EditBranch { .. } | View::EditCommit { .. } => {}
        }
    }
    Ok(())
}

fn render(
    frame: &mut Frame,
    view: &View,
    sessions: &[store::SessionInfo],
    accept_pending: &bool,
    status: &Option<(bool, String)>,
    refreshing: bool,
    spinner_frame: usize,
    log_scroll: u16,
    slash_menu: &Option<SlashMenuState>,
) {
    let width = frame.area().width as usize;

    let header_lines = if *accept_pending {
        vec![
            Line::from(Span::styled("vibe dashboard", theme::header_title())),
            Line::from(Span::styled(
                "j/k/n=move  l/enter=select  h/esc=back  q=quit",
                theme::header_hint(),
            )),
            Line::from(Span::styled(
                "Press Enter to accept, Esc to cancel",
                theme::header_hint(),
            )),
        ]
    } else if let Some((ok, msg)) = status {
        let style = if *ok { theme::success() } else { theme::error() };
        vec![
            Line::from(Span::styled("vibe dashboard", theme::header_title())),
            Line::from(Span::styled(
                "j/k/n=move  l/enter=select  h/esc=back  q=quit",
                theme::header_hint(),
            )),
            Line::from(Span::styled(msg.clone(), style)),
        ]
    } else if refreshing {
        let spin = SPINNER[spinner_frame];
        vec![
            Line::from(Span::styled("vibe dashboard", theme::header_title())),
            Line::from(vec![
                Span::styled(
                    format!(" {} Refreshing commit messages... (r=show store)", spin),
                    theme::header_hint(),
                ),
            ]),
        ]
    } else {
        vec![
            Line::from(Span::styled("vibe dashboard", theme::header_title())),
            Line::from(Span::styled(
                "j/k/n=move  l/enter=select  h/esc=back  a+Enter=accept  r=refresh  /=menu  q=quit",
                theme::header_hint(),
            )),
        ]
    };

    let chunks = if show_daemon_log() {
        Layout::vertical([
            Constraint::Length(header_lines.len() as u16),
            Constraint::Min(0),
            Constraint::Length(9),
        ])
        .split(frame.area())
    } else {
        Layout::vertical([
            Constraint::Length(header_lines.len() as u16),
            Constraint::Min(0),
        ])
        .split(frame.area())
    };

    let header = Paragraph::new(Text::from(header_lines));
    frame.render_widget(header, chunks[0]);

    if show_daemon_log() {
        let log_chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(chunks[2]);

        let log_lines = read_daemon_log_lines();
        let total_log_lines = log_lines.len() as u16;
        let viewport = log_chunks[1].height;
        let max_scroll = total_log_lines.saturating_sub(viewport).min(u16::MAX);
        let clamped_scroll = log_scroll.min(max_scroll);
        let scroll_y = max_scroll.saturating_sub(clamped_scroll);
        let log_text: Text = log_lines
            .iter()
            .map(|s| Line::from(Span::raw(s.as_str())))
            .collect::<Vec<_>>()
            .into();
        let log_para = Paragraph::new(log_text)
            .wrap(Wrap { trim: false })
            .style(theme::header_hint())
            .scroll((scroll_y, 0));
        frame.render_widget(
            Paragraph::new(Span::styled(
                "daemon log (J/K=scroll, GG_DAEMON_LOG=0 to hide):",
                theme::header_title(),
            )),
            log_chunks[0],
        );
        frame.render_widget(log_para, log_chunks[1]);
    }

    let main_chunk = chunks[1];

    match view {
        View::Diff {
            session_idx,
            draft_idx,
            file_idx,
            scroll_offset,
        } => {
            let diff_chunks = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(main_chunk);
            let viewport_height = diff_chunks[1].height as usize;
            if let Some(session) = sessions.get(*session_idx) {
                let drafts = store::list_drafts(&session.id).unwrap_or_default();
                if let Some(draft) = drafts.get(*draft_idx) {
                    let msg_para =
                        Paragraph::new(draft.message.as_str()).wrap(Wrap { trim: false });
                    frame.render_widget(msg_para, diff_chunks[0]);
                    let change_ids = store::draft_change_ids(&draft.id).unwrap_or_default();
                    let changes: Vec<_> = change_ids
                        .iter()
                        .filter_map(|id| store::get_change(id).ok().flatten())
                        .collect();
                    let file_count = changes.len();
                    let clamped_file_idx = (*file_idx).min(file_count.saturating_sub(1));

                    let main_split = Layout::horizontal([
                        Constraint::Length(30),
                        Constraint::Min(0),
                    ])
                    .split(diff_chunks[1]);

                    let file_items: Vec<ListItem> = changes
                        .iter()
                        .enumerate()
                        .map(|(idx, change)| {
                            let marker = if idx == clamped_file_idx { "> " } else { "  " };
                            let style = if idx == clamped_file_idx {
                                theme::selected()
                            } else {
                                Style::default()
                            };
                            let path = change.file_path.as_str();
                            let display = if path.len() > 28 {
                                format!("{}..", &path[..26])
                            } else {
                                path.to_string()
                            };
                            ListItem::new(Line::from(vec![
                                Span::styled(marker, style),
                                Span::styled(display, style),
                            ]))
                        })
                        .collect();
                    let file_list = List::new(file_items);
                    let mut file_state =
                        ListState::default().with_selected(Some(clamped_file_idx));
                    frame.render_stateful_widget(file_list, main_split[0], &mut file_state);

                    let mut lines: Vec<Line> = Vec::new();
                    if let Some(change) = changes.get(clamped_file_idx) {
                        for line in change.diff.lines() {
                            let s = line.to_string();
                            let span = if s.starts_with('+') && !s.starts_with("+++") {
                                Span::styled(s, Style::new().fg(Color::Green))
                            } else if s.starts_with('-') && !s.starts_with("---") {
                                Span::styled(s, Style::new().fg(Color::Red))
                            } else {
                                Span::raw(s)
                            };
                            lines.push(Line::from(span));
                        }
                    }
                    let total_lines = lines.len();
                    let max_scroll = total_lines
                        .saturating_sub(viewport_height)
                        .min(u16::MAX as usize) as u16;
                    let clamped_scroll = (*scroll_offset).min(max_scroll);
                    let content = if lines.is_empty() {
                        Text::from(Span::styled(
                            "(no changes for this draft)",
                            theme::header_hint(),
                        ))
                    } else {
                        Text::from(lines)
                    };
                    let diff = Paragraph::new(content)
                        .wrap(Wrap { trim: false })
                        .scroll((clamped_scroll, 0));
                    frame.render_widget(diff, main_split[1]);
                }
            }
            let hint = Paragraph::new(Span::styled(
                "j/k=file  n/space/p=scroll  h/esc=back",
                theme::header_hint(),
            ));
            frame.render_widget(hint, diff_chunks[2]);
        }
        View::Sessions { selected } => {
            if sessions.is_empty() {
                let empty = Paragraph::new(Span::styled("(no sessions)", theme::header_hint()));
                frame.render_widget(empty, main_chunk);
            } else {
                let items: Vec<ListItem> = sessions
                    .iter()
                    .enumerate()
                    .map(|(idx, session)| {
                        let (ide_col, repo_col, branch, idx_prefix) =
                            session_row::format_session_parts(session, width, Some(idx));
                        let row_style = if idx == *selected {
                            theme::selected()
                        } else {
                            Style::default()
                        };
                        let marker = if idx == *selected { "> " } else { "  " };
                        let spans: Vec<Span> = vec![
                            Span::styled(marker, row_style),
                            Span::styled(idx_prefix.unwrap_or_default(), row_style),
                            Span::styled(ide_col, theme::ide()),
                            Span::raw("  "),
                            Span::styled(repo_col, theme::repo()),
                            Span::raw("  "),
                            Span::styled(branch, theme::branch()),
                            Span::styled("  active", row_style),
                        ];
                        let mut lines = vec![Line::from(spans)];
                        let drafts = store::list_drafts(&session.id).unwrap_or_default();
                        let max_subject = width.saturating_sub(12);
                        for (draft_idx, draft) in drafts.iter().enumerate() {
                            if draft.message.contains("(generating...)") {
                                let spin = SPINNER[spinner_frame];
                                lines.push(Line::from(Span::styled(
                                    format!("    - [{}] {} ...", draft_idx + 1, spin),
                                    theme::commit_msg(false),
                                )));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    format!(
                                        "    - [{}] {}",
                                        draft_idx + 1,
                                        grouping::subject_line_truncated(&draft.message, max_subject)
                                    ),
                                    theme::commit_msg(false),
                                )));
                            }
                        }
                        ListItem::new(Text::from(lines))
                    })
                    .collect();
                let list = List::new(items);
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, main_chunk, &mut state);
            }
        }
        View::EditBranch { session_idx: _, buffer } => {
            let prompt = Line::from(vec![
                Span::styled("Branch: ", theme::header_title()),
                Span::raw(buffer.as_str()),
            ]);
            let para = Paragraph::new(Text::from(vec![
                prompt,
                Line::from(""),
                Line::from(Span::styled(
                    "Enter=save  Esc=cancel",
                    theme::header_hint(),
                )),
            ]));
            frame.render_widget(para, main_chunk);
        }
        View::EditCommit {
            session_idx: _,
            draft_idx: _,
            buffer,
        } => {
            let prompt = Line::from(vec![
                Span::styled("Message: ", theme::header_title()),
                Span::raw(buffer.as_str()),
            ]);
            let para = Paragraph::new(Text::from(vec![
                prompt,
                Line::from(""),
                Line::from(Span::styled(
                    "Enter=save  Esc=cancel",
                    theme::header_hint(),
                )),
            ]));
            frame.render_widget(para, main_chunk);
        }
        View::Commits { session_idx, selected } => {
            if let Some(session) = sessions.get(*session_idx) {
                let (ide_col, repo_col, branch, idx_prefix) =
                    session_row::format_session_parts(session, width, Some(*session_idx));
                let header_spans = vec![
                    Span::raw("  "),
                    Span::styled(idx_prefix.unwrap_or_default(), theme::header_hint()),
                    Span::styled(ide_col, theme::ide()),
                    Span::raw("  "),
                    Span::styled(repo_col, theme::repo()),
                    Span::raw("  "),
                    Span::styled(branch, theme::branch()),
                    Span::styled("  active", theme::header_hint()),
                ];
                let header = Paragraph::new(Text::from(vec![
                    Line::from(header_spans),
                    Line::from(""),
                ]));
                let header_area = Layout::vertical([
                    Constraint::Length(2),
                    Constraint::Min(0),
                ])
                .split(main_chunk);
                frame.render_widget(header, header_area[0]);

                let drafts = store::list_drafts(&session.id).unwrap_or_default();
                if drafts.is_empty() {
                    let empty = Paragraph::new(Span::styled(
                        "  (no commits)",
                        theme::header_hint(),
                    ));
                    frame.render_widget(empty, header_area[1]);
                } else {
                    let spin = SPINNER[spinner_frame];
                    let max_subject = width.saturating_sub(12);
                    let items: Vec<ListItem> = drafts
                        .iter()
                        .enumerate()
                        .map(|(draft_idx, draft)| {
                            let marker = if draft_idx == *selected { "> " } else { "  " };
                            let msg_style = theme::commit_msg(draft_idx == *selected);
                            if draft.message.contains("(generating...)") {
                                ListItem::new(Line::from(vec![
                                    Span::raw("  "),
                                    Span::styled(marker, msg_style),
                                    Span::styled(format!("[{}] {} ...", draft_idx + 1, spin), msg_style),
                                ]))
                            } else {
                                ListItem::new(Line::from(vec![
                                    Span::raw("  "),
                                    Span::styled(marker, msg_style),
                                    Span::styled(
                                        format!(
                                            "[{}] {}",
                                            draft_idx + 1,
                                            grouping::subject_line_truncated(&draft.message, max_subject)
                                        ),
                                        msg_style,
                                    ),
                                ]))
                            }
                        })
                        .collect();
                    let list = List::new(items);
                    let mut state = ListState::default().with_selected(Some(*selected));
                    frame.render_stateful_widget(list, header_area[1], &mut state);
                }
            }
        }
    }

    if let Some(menu) = slash_menu {
        let area = frame.area();
        let menu_width = 40u16;
        let menu_height = (menu.items.len() + 2) as u16 + 2;
        let popup = ratatui::layout::Rect {
            x: area.x + (area.width.saturating_sub(menu_width)) / 2,
            y: area.y + (area.height.saturating_sub(menu_height)) / 2,
            width: menu_width,
            height: menu_height,
        };
        let block = Block::default()
            .title(if menu.level == 0 {
                " / "
            } else if menu.level == 1 {
                " Models "
            } else {
                " Ollama model "
            })
            .borders(Borders::ALL)
            .border_style(theme::header_title());
        let items: Vec<ListItem> = menu
            .items
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let style = if idx == menu.selected {
                    theme::selected()
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(Span::styled(
                    format!("  {}  {}", if idx == menu.selected { ">" } else { " " }, s),
                    style,
                )))
            })
            .collect();
        let list = List::new(items);
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        let mut list_state = ListState::default().with_selected(Some(menu.selected));
        frame.render_stateful_widget(list, inner, &mut list_state);
    }
}

fn active_window_secs() -> i64 {
    std::env::var("GG_ACTIVE_WINDOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(900)
}
