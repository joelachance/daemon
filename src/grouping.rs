pub fn build_full_message(subject: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        subject.to_string()
    } else {
        format!("{}\n\n{}", subject, body)
    }
}

pub fn subject_line(full_message: &str) -> &str {
    full_message.lines().next().unwrap_or("")
}

/// Returns the subject line truncated to max_chars for display.
pub fn subject_line_truncated(full_message: &str, max_chars: usize) -> String {
    let line = subject_line(full_message);
    if line.chars().count() <= max_chars {
        line.to_string()
    } else if max_chars <= 3 {
        line.chars().take(max_chars).collect()
    } else {
        let mut s: String = line.chars().take(max_chars - 3).collect();
        s.push_str("...");
        s
    }
}

fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars <= 3 {
        s.chars().take(max_chars).collect()
    } else {
        let mut out: String = s.chars().take(max_chars - 3).collect();
        out.push_str("...");
        out
    }
}

/// Format subject + body for list display. Subject on first line, body truncated on second.
pub fn format_message_for_list(full_message: &str, max_subject: usize, max_body: usize) -> Vec<String> {
    let subject = subject_line_truncated(full_message, max_subject);
    let body = full_message
        .lines()
        .skip(1)
        .skip_while(|l| l.trim().is_empty())
        .next()
        .map(|b| truncate_with_ellipsis(b.trim(), max_body))
        .unwrap_or_default();
    if body.is_empty() {
        vec![subject]
    } else {
        vec![subject, format!("    {}", body)]
    }
}

const CONVERSATION_PHRASES: &[&str] = &[
    "ok, here's",
    "here's some",
    "we need to follow",
    "rules we need",
    "let me ",
    "i'll ",
    "i will ",
];

/// Returns false if the subject looks like a conversation quote rather than a code-change description.
pub fn is_valid_commit_subject(subject: &str) -> bool {
    if subject.contains('\n') {
        return false;
    }
    let lower = subject.to_lowercase();
    for phrase in CONVERSATION_PHRASES {
        if lower.contains(phrase) {
            return false;
        }
    }
    let description = subject
        .splitn(2, ':')
        .nth(1)
        .map(|s| s.trim())
        .unwrap_or(subject);
    if description.chars().count() < 10 {
        return false;
    }
    if description.ends_with(':') {
        return false;
    }
    true
}

