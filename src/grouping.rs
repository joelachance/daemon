use crate::session::Change;
use std::collections::HashSet;
use std::path::Path;

pub fn infer_message(prompt: &str, changes: &[Change]) -> String {
    let ty = infer_type(changes);
    let scope = infer_scope(changes);
    let subject = infer_subject(prompt);
    match scope {
        Some(scope) => format!("{ty}({scope}): {subject}"),
        None => format!("{ty}: {subject}"),
    }
}

fn infer_type(changes: &[Change]) -> &'static str {
    if changes.is_empty() {
        return "fix";
    }
    if changes.iter().all(|change| change.file_path.ends_with(".md") || change.file_path.starts_with("docs/")) {
        return "docs";
    }
    if changes
        .iter()
        .any(|change| change.file_path.contains("__tests__/") || change.file_path.contains(".test."))
    {
        return "test";
    }
    if changes.iter().all(|change| {
        let path = change.file_path.as_str();
        path.ends_with(".toml")
            || path.ends_with(".yaml")
            || path.ends_with(".yml")
            || path.ends_with(".json")
            || path.ends_with(".lock")
    }) {
        return "chore";
    }
    let mut deleted = 0i64;
    let mut added = 0i64;
    for change in changes {
        deleted += change.line_range.old_count;
        added += change.line_range.new_count;
    }
    if deleted > added {
        return "refactor";
    }
    if changes.iter().all(|change| change.line_range.old_count == 0) {
        return "feat";
    }
    "fix"
}

fn infer_scope(changes: &[Change]) -> Option<String> {
    let mut dirs = HashSet::new();
    for change in changes {
        let parent = Path::new(&change.file_path).parent()?;
        let name = parent.file_name()?.to_string_lossy().to_string();
        if !name.is_empty() {
            dirs.insert(name);
        }
    }
    if dirs.len() == 1 {
        dirs.into_iter().next()
    } else {
        None
    }
}

fn infer_subject(prompt: &str) -> String {
    let mut subject = prompt
        .lines()
        .next()
        .unwrap_or("update session changes")
        .trim()
        .trim_end_matches('?')
        .to_string();
    if subject.is_empty() {
        subject = "update session changes".to_string();
    }
    if subject.chars().count() > 72 {
        subject = subject.chars().take(72).collect::<String>();
    }
    subject
}
