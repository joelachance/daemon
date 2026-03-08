use crate::daemon;
use crate::grouping;
use crate::session_row;
use crate::store;
use crossterm::event::{read, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{ListItem, List, ListState, Paragraph, Wrap};
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
        scroll_offset: u16,
    },
    EditBranch { session_idx: usize, buffer: String },
    EditCommit { session_idx: usize, draft_idx: usize, buffer: String },
}

pub fn run_dashboard() -> Result<(), String> {
    daemon::ensure_daemon_running()?;

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut ratatui::DefaultTerminal) -> Result<(), String> {
    let mut view = View::Sessions { selected: 0 };
    let mut accept_pending = false;
    let mut status: Option<(bool, String)> = None;

    loop {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let window_secs = active_window_secs();
        let sessions = store::list_active_sessions(now, window_secs)?;

        terminal
            .draw(|frame| render(frame, &view, &sessions, &accept_pending, &status))
            .map_err(|e| e.to_string())?;

        let event = read().map_err(|e| e.to_string())?;
        let key = match event {
            Event::Key(ke) if ke.kind == KeyEventKind::Press => ke.code,
            _ => continue,
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

        let (down, up, back, enter, quit) = match key {
            KeyCode::Char('j') | KeyCode::Char('n') | KeyCode::Down => {
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
            _ => continue,
        };

        if quit {
            break;
        }

        if let View::Diff {
            session_idx,
            draft_idx,
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
            if down || up {
                let total_lines = if let Some(session) = sessions.get(*session_idx) {
                    if let Ok(drafts) = store::list_drafts(&session.id) {
                        if let Some(draft) = drafts.get(*draft_idx) {
                            store::draft_change_ids(&draft.id)
                                .unwrap_or_default()
                                .iter()
                                .filter_map(|id| store::get_change(id).ok().flatten())
                                .map(|c| c.diff.lines().count())
                                .sum::<usize>()
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
                if down {
                    *scroll_offset = (*scroll_offset + 1).min(max_scroll);
                } else {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                }
                continue;
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
    } else {
        vec![
            Line::from(Span::styled("vibe dashboard", theme::header_title())),
            Line::from(Span::styled(
                "j/k/n=move  l/enter=select  h/esc=back  a+Enter=accept  q=quit",
                theme::header_hint(),
            )),
        ]
    };

    let chunks = Layout::vertical([
        Constraint::Length(header_lines.len() as u16),
        Constraint::Min(0),
    ])
    .split(frame.area());

    let header = Paragraph::new(Text::from(header_lines));
    frame.render_widget(header, chunks[0]);

    match view {
        View::Diff {
            session_idx,
            draft_idx,
            scroll_offset,
        } => {
            let diff_chunks = Layout::vertical([
                Constraint::Min(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(chunks[1]);
            let viewport_height = diff_chunks[1].height as usize;
            if let Some(session) = sessions.get(*session_idx) {
                let drafts = store::list_drafts(&session.id).unwrap_or_default();
                if let Some(draft) = drafts.get(*draft_idx) {
                    let msg_para =
                        Paragraph::new(draft.message.as_str()).wrap(Wrap { trim: false });
                    frame.render_widget(msg_para, diff_chunks[0]);
                    let change_ids = store::draft_change_ids(&draft.id).unwrap_or_default();
                    let mut lines: Vec<Line> = Vec::new();
                    for change_id in change_ids {
                        if let Ok(Some(change)) = store::get_change(&change_id) {
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
                    frame.render_widget(diff, diff_chunks[1]);
                }
            }
            let hint = Paragraph::new(Span::styled(
                "j/k=scroll  h/esc=back",
                theme::header_hint(),
            ));
            frame.render_widget(hint, diff_chunks[2]);
        }
        View::Sessions { selected } => {
            if sessions.is_empty() {
                let empty = Paragraph::new(Span::styled("(no sessions)", theme::header_hint()));
                frame.render_widget(empty, chunks[1]);
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
                        for (draft_idx, draft) in drafts.iter().enumerate() {
                            lines.push(Line::from(Span::styled(
                                format!(
                                    "    - [{}] {}",
                                    draft_idx + 1,
                                    grouping::subject_line(&draft.message)
                                ),
                                theme::commit_msg(false),
                            )));
                        }
                        ListItem::new(Text::from(lines))
                    })
                    .collect();
                let list = List::new(items);
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, chunks[1], &mut state);
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
            frame.render_widget(para, chunks[1]);
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
            frame.render_widget(para, chunks[1]);
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
                .split(chunks[1]);
                frame.render_widget(header, header_area[0]);

                let drafts = store::list_drafts(&session.id).unwrap_or_default();
                if drafts.is_empty() {
                    let empty = Paragraph::new(Span::styled(
                        "  (no commits)",
                        theme::header_hint(),
                    ));
                    frame.render_widget(empty, header_area[1]);
                } else {
                    let items: Vec<ListItem> = drafts
                        .iter()
                        .enumerate()
                        .map(|(draft_idx, draft)| {
                            let marker = if draft_idx == *selected { "> " } else { "  " };
                            let msg_style = theme::commit_msg(draft_idx == *selected);
                            ListItem::new(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(marker, msg_style),
                                Span::styled(
                                    format!(
                                        "[{}] {}",
                                        draft_idx + 1,
                                        grouping::subject_line(&draft.message)
                                    ),
                                    msg_style,
                                ),
                            ]))
                        })
                        .collect();
                    let list = List::new(items);
                    let mut state = ListState::default().with_selected(Some(*selected));
                    frame.render_stateful_widget(list, header_area[1], &mut state);
                }
            }
        }
    }
}

fn active_window_secs() -> i64 {
    std::env::var("GG_ACTIVE_WINDOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(900)
}
