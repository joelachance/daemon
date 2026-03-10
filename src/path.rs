/// Normalizes a repo path for consistent storage and comparison.
/// Callers should canonicalize filesystem paths first, then pass through this.
pub fn normalize_repo_path(path: &str) -> String {
    let mut text = path.trim().to_string();
    if text.is_empty() {
        return String::new();
    }
    if let Some(stripped) = text.strip_prefix("file://") {
        text = stripped.to_string();
    }
    text = text.replace('\\', "/");
    while text.ends_with('/') && text.len() > 1 {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_trailing_slash() {
        assert_eq!(normalize_repo_path("/foo/bar/"), "/foo/bar");
    }

    #[test]
    fn normalizes_backslashes() {
        assert_eq!(normalize_repo_path("C:\\foo\\bar"), "C:/foo/bar");
    }

    #[test]
    fn strips_file_prefix() {
        assert_eq!(normalize_repo_path("file:///foo/bar"), "/foo/bar");
    }

    #[test]
    fn preserves_root() {
        assert_eq!(normalize_repo_path("/"), "/");
    }
}
