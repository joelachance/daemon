use crate::store::SessionInfo;
use std::path::Path;

pub fn format_session_columns(session: &SessionInfo, width: usize, index: Option<usize>) -> String {
    let ide = normalize_ide(&session.ide);
    let repo = repo_display_name(&session.repo_path);
    let mut branch = session
        .confirmed_branch
        .as_deref()
        .unwrap_or(&session.suggested_branch)
        .to_string();
    if branch.trim().is_empty() {
        branch = placeholder_branch_name(&session.id);
    }

    // Keep IDE + branch stable, shrink repo first.
    let ide_w = 9usize;
    let min_repo_w = 10usize;
    let separators = if index.is_some() { 8usize } else { 4usize };
    let reserved = ide_w + separators + branch.chars().count();
    let repo_w = if width > reserved + min_repo_w {
        width - reserved
    } else {
        min_repo_w
    };

    let ide_col = pad_right(&truncate(&ide, ide_w), ide_w);
    let repo_col = pad_right(&truncate(&repo, repo_w), repo_w);

    match index {
        Some(idx) => format!("[{idx:<2}] {ide_col}  {repo_col}  {branch}"),
        None => format!("{ide_col}  {repo_col}  {branch}"),
    }
}

fn placeholder_branch_name(session_id: &str) -> String {
    let short = session_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    if short.is_empty() {
        "feature/session".to_string()
    } else {
        format!("feature/session-{short}")
    }
}

fn normalize_ide(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "unknown" => "unknown".to_string(),
        other => other.to_string(),
    }
}

fn repo_display_name(repo_path: &str) -> String {
    let trimmed = repo_path.trim();
    if trimmed.is_empty() {
        return "-".to_string();
    }
    match Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => trimmed.to_string(),
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    if max <= 3 {
        return value.chars().take(max).collect::<String>();
    }
    let mut out = value.chars().take(max - 3).collect::<String>();
    out.push_str("...");
    out
}

fn pad_right(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        return value.to_string();
    }
    let mut out = String::with_capacity(width);
    out.push_str(value);
    out.push_str(&" ".repeat(width - len));
    out
}
