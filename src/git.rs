use ignore::gitignore::GitignoreBuilder;
use ignore::WalkBuilder;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Default, Clone)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct SessionCommit {
    pub commit: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub last_event: Option<String>,
    pub event_count: usize,
}

#[allow(dead_code)]
pub fn stage_paths(paths: &[String]) -> Result<GitOutput, String> {
    if paths.is_empty() {
        return Ok(GitOutput::default());
    }

    let root = repo_root()?;
    stage_paths_in_root(&root, paths)
}

pub fn stage_paths_in_root(root: &str, paths: &[String]) -> Result<GitOutput, String> {
    if paths.is_empty() {
        return Ok(GitOutput::default());
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("add").arg("--");
    for path in paths {
        cmd.arg(path);
    }

    run_status_output(cmd)
}

pub fn run_passthrough(subcommand: &str, args: &[String]) -> Result<(), String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&root).arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }
    let status = cmd.status().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {subcommand} failed"))
    }
}

#[allow(dead_code)]
pub fn list_changed_paths() -> Result<Vec<String>, String> {
    let root = repo_root()?;
    list_changed_paths_in_root(&root)
}

pub fn list_changed_paths_in_root(root: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--porcelain")
        .arg("-z")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git status failed".to_string());
    }

    let mut paths = Vec::new();
    let mut iter = output.stdout.split(|byte| *byte == 0);

    while let Some(record_bytes) = iter.next() {
        if record_bytes.is_empty() {
            continue;
        }

        let record = String::from_utf8_lossy(record_bytes);
        if record.len() < 3 {
            continue;
        }

        let status = &record[0..2];
        if status == "!!" {
            continue;
        }

        let path = record[3..].to_string();
        let status_code = status.chars().next().unwrap_or(' ');

        if status_code == 'R' || status_code == 'C' {
            if let Some(new_path_bytes) = iter.next() {
                let new_path = String::from_utf8_lossy(new_path_bytes).to_string();
                if !new_path.is_empty() {
                    paths.push(new_path);
                }
            }
        } else if !path.is_empty() {
            paths.push(path);
        }
    }

    Ok(paths)
}

#[allow(dead_code)]
pub fn filter_ignored_paths(paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let root = repo_root()?;
    filter_ignored_paths_in_root(&root, paths)
}

pub fn filter_ignored_paths_in_root(root: &str, paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = GitignoreBuilder::new(&root);
    let root_path = Path::new(&root);
    let info_exclude_path = root_path.join(".git").join("info").join("exclude");

    for ignore_path in collect_ignore_files(root_path) {
        let _ = builder.add(ignore_path);
    }
    if info_exclude_path.exists() {
        let _ = builder.add(info_exclude_path);
    }

    let gitignore = builder.build().map_err(|err| err.to_string())?;

    let mut filtered = Vec::new();
    for path in paths {
        let rel_path = Path::new(path);
        let is_dir = root_path.join(rel_path).is_dir();
        if !gitignore.matched(rel_path, is_dir).is_ignore() {
            filtered.push(path.clone());
        }
    }

    Ok(filtered)
}

fn collect_ignore_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false);
    let walker = walker.filter_entry(|entry| {
        if entry
            .file_type()
            .map(|value| value.is_dir())
            .unwrap_or(false)
        {
            return entry.file_name() != OsStr::new(".git");
        }
        true
    });

    for entry in walker.build() {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !entry
            .file_type()
            .map(|value| value.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name == ".gitignore" || name == ".ggignore" {
            files.push(entry.path().to_path_buf());
        }
    }

    files.sort_by(|left, right| {
        let left_depth = left.components().count();
        let right_depth = right.components().count();
        let left_name = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let right_name = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        left_depth
            .cmp(&right_depth)
            .then_with(|| left_name.cmp(right_name))
    });
    files
}

pub fn filter_paths_to_cwd(
    root: &str,
    cwd: &Path,
    paths: &[String],
) -> Result<Vec<String>, String> {
    let root_path = Path::new(root);
    let prefix = match cwd.strip_prefix(root_path) {
        Ok(value) => value,
        Err(_) => return Ok(paths.to_vec()),
    };

    if prefix.as_os_str().is_empty() {
        return Ok(paths.to_vec());
    }

    let mut filtered = Vec::new();
    for path in paths {
        let candidate = Path::new(path);
        if candidate.strip_prefix(prefix).is_ok() {
            filtered.push(path.clone());
        }
    }

    Ok(filtered)
}

#[allow(dead_code)]
pub fn commit(
    summary: &str,
    trailers: &[(String, String)],
) -> Result<(Option<String>, GitOutput), String> {
    let root = repo_root()?;
    commit_in_root(&root, summary, trailers)
}

pub fn commit_in_root(
    root: &str,
    summary: &str,
    trailers: &[(String, String)],
) -> Result<(Option<String>, GitOutput), String> {
    if staged_is_clean(root)? {
        return Ok((None, GitOutput::default()));
    }

    let trailer_block = trailers
        .iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<String>>()
        .join("\n");

    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("commit").arg("-m").arg(summary);

    if !trailer_block.is_empty() {
        cmd.arg("-m").arg(trailer_block);
    }

    let output = run_status_output(cmd)?;
    let mut rev_cmd = Command::new("git");
    rev_cmd.arg("-C").arg(root).arg("rev-parse").arg("HEAD");
    let sha = run_stdout(rev_cmd)?;
    Ok((Some(sha.trim().to_string()), output))
}

pub fn commit_subject(commit: &str) -> Result<String, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("log")
        .arg("-1")
        .arg("--pretty=%s")
        .arg(commit)
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git log failed".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn commit_parent(commit: &str) -> Result<Option<String>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("rev-parse")
        .arg(format!("{commit}^"))
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

#[derive(Debug, Deserialize)]
struct SessionEventRecord {
    commit: Option<String>,
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionEventMeta {
    end: Option<bool>,
    soft_end: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SessionEventPayload {
    meta: Option<SessionEventMeta>,
}

pub fn list_session_commits(session_id: &str) -> Result<Vec<SessionCommit>, String> {
    let root = repo_root()?;
    let ref_name = format!("refs/gg/sessions/{session_id}");
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("rev-list")
        .arg("--reverse")
        .arg(&ref_name)
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err(format!("session not found: {session_id}"));
    }

    let mut seen = HashSet::new();
    let mut commits = Vec::new();
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let event_commit = line.trim();
        if event_commit.is_empty() {
            continue;
        }

        let payload = read_commit_file(&root, event_commit, "event.json")?;
        let record: SessionEventRecord =
            serde_json::from_str(&payload).map_err(|err| err.to_string())?;

        let commit_hash = match record.commit {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => continue,
        };

        if !seen.insert(commit_hash.clone()) {
            continue;
        }

        let summary = match commit_subject(&commit_hash) {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => record
                .summary
                .unwrap_or_else(|| "commit".to_string())
                .trim()
                .to_string(),
        };

        commits.push(SessionCommit {
            commit: commit_hash,
            summary,
        });
    }

    Ok(commits)
}

pub fn session_ended(session_id: &str) -> Result<bool, String> {
    let root = repo_root()?;
    let ref_name = format!("refs/gg/sessions/{session_id}");
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("rev-list")
        .arg("-1")
        .arg(&ref_name)
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err(format!("session not found: {session_id}"));
    }

    let event_commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if event_commit.is_empty() {
        return Ok(false);
    }

    let payload = read_commit_file(&root, &event_commit, "event.json")?;
    let record: SessionEventPayload =
        serde_json::from_str(&payload).map_err(|err| err.to_string())?;
    Ok(record
        .meta
        .map(|meta| meta.end.unwrap_or(false) || meta.soft_end.unwrap_or(false))
        .unwrap_or(false))
}

pub fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("for-each-ref")
        .arg("--sort=-committerdate")
        .arg("--format=%(refname:strip=3)\t%(committerdate:iso8601-strict)")
        .arg("refs/gg/sessions")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git for-each-ref failed".to_string());
    }

    let mut sessions = Vec::new();
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, '\t');
        let id = match parts.next() {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => continue,
        };
        let last_event = parts.next().map(|value| value.trim().to_string());

        let mut count_cmd = Command::new("git");
        count_cmd
            .arg("-C")
            .arg(&root)
            .arg("rev-list")
            .arg("--count")
            .arg(format!("refs/gg/sessions/{id}"));
        let count_output = run_stdout(count_cmd)?;
        let event_count = count_output.trim().parse::<usize>().unwrap_or(0);

        sessions.push(SessionInfo {
            id,
            last_event,
            event_count,
        });
    }

    Ok(sessions)
}

pub fn push() -> Result<(), String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&root).arg("push");
    run_status(cmd)
}

pub fn head_commit() -> Result<String, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git rev-parse HEAD failed".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn reset_soft_to(commit: &str) -> Result<(), String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("reset")
        .arg("--soft")
        .arg(commit);
    run_status(cmd)
}

pub fn commit_with_message(summary: &str, amend: bool) -> Result<GitOutput, String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("commit")
        .arg("-m")
        .arg(summary);
    if amend {
        cmd.arg("--amend");
    }
    run_status_output(cmd)
}

fn read_commit_file(root: &str, commit: &str, path: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("show")
        .arg(format!("{commit}:{path}"));
    run_stdout(cmd)
}

#[allow(dead_code)]
pub fn append_session_event(session_id: &str, payload: &str) -> Result<String, String> {
    let root = repo_root()?;
    append_session_event_in_root(&root, session_id, payload)
}

pub fn append_session_event_in_root(
    root: &str,
    session_id: &str,
    payload: &str,
) -> Result<String, String> {
    let ref_name = format!("refs/gg/sessions/{session_id}");

    let mut blob_cmd = Command::new("git");
    blob_cmd
        .arg("-C")
        .arg(root)
        .arg("hash-object")
        .arg("-w")
        .arg("--stdin");
    let blob_hash = run_stdout_with_input(blob_cmd, payload)?;

    let tree_input = format!("100644 blob {}\tevent.json\n", blob_hash.trim());
    let mut tree_cmd = Command::new("git");
    tree_cmd.arg("-C").arg(root).arg("mktree");
    let tree_hash = run_stdout_with_input(tree_cmd, &tree_input)?;

    let mut parent_cmd = Command::new("git");
    parent_cmd
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("-q")
        .arg("--verify")
        .arg(&ref_name);
    let parent = run_stdout(parent_cmd)
        .ok()
        .map(|value| value.trim().to_string());

    let mut commit_cmd = Command::new("git");
    commit_cmd
        .arg("-C")
        .arg(root)
        .arg("commit-tree")
        .arg(tree_hash.trim())
        .arg("-m")
        .arg(format!("gg session {session_id} event"));

    if let Some(parent_hash) = parent {
        commit_cmd.arg("-p").arg(parent_hash);
    }

    let commit_hash = run_stdout(commit_cmd)?;
    let commit_hash = commit_hash.trim().to_string();

    let mut update_cmd = Command::new("git");
    update_cmd
        .arg("-C")
        .arg(root)
        .arg("update-ref")
        .arg(&ref_name)
        .arg(&commit_hash);
    run_status(update_cmd)?;

    Ok(commit_hash)
}

#[allow(dead_code)]
pub fn write_notes(ref_name: &str, commit_hash: &str, payload: &str) -> Result<(), String> {
    let root = repo_root()?;
    write_notes_in_root(&root, ref_name, commit_hash, payload)
}

pub fn write_notes_in_root(
    root: &str,
    ref_name: &str,
    commit_hash: &str,
    payload: &str,
) -> Result<(), String> {
    let temp_path = temp_note_path("gg-note.json");
    fs::write(&temp_path, payload).map_err(|err| err.to_string())?;

    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("notes")
        .arg("--ref")
        .arg(ref_name)
        .arg("add")
        .arg("-f")
        .arg("--file")
        .arg(&temp_path)
        .arg(commit_hash)
        .status()
        .map_err(|err| err.to_string())?;

    let _ = fs::remove_file(&temp_path);

    if status.success() {
        Ok(())
    } else {
        Err("git notes add failed".to_string())
    }
}

pub fn repo_root() -> Result<String, String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("not inside a git repository".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn repo_root_from(path: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("not inside a git repository".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn staged_is_clean(root: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .arg("--cached")
        .arg("--quiet")
        .status()
        .map_err(|err| err.to_string())?;

    Ok(status.success())
}

fn run_stdout(mut cmd: Command) -> Result<String, String> {
    let output = cmd.output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!("command failed: {:?}", cmd));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_stdout_with_input(mut cmd: Command, input: &str) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|err| err.to_string())?;
    }

    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!("command failed: {:?}", cmd));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn get_local_config(key: &str) -> Result<Option<String>, String> {
    let values = get_local_config_values(key)?;
    Ok(values.into_iter().next())
}

pub fn get_local_config_in_root(root: &str, key: &str) -> Result<Option<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("config")
        .arg("--local")
        .arg("--get")
        .arg(key)
        .output()
        .map_err(|err| err.to_string())?;

    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
        Some(1) => Ok(None),
        _ => Err("git config failed".to_string()),
    }
}

pub fn list_remotes() -> Result<Vec<String>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("remote")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git remote failed".to_string());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

pub fn has_remote() -> Result<bool, String> {
    Ok(!list_remotes()?.is_empty())
}

pub fn add_remote(name: &str, url: &str) -> Result<(), String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("remote")
        .arg("add")
        .arg(name)
        .arg(url);
    run_status(cmd)
}

fn get_local_config_values(key: &str) -> Result<Vec<String>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("config")
        .arg("--local")
        .arg("--get-all")
        .arg(key)
        .output()
        .map_err(|err| err.to_string())?;

    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(text
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect())
        }
        Some(1) => Ok(Vec::new()),
        _ => Err("git config failed".to_string()),
    }
}

fn run_status(mut cmd: Command) -> Result<(), String> {
    let status = cmd.status().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {:?}", cmd))
    }
}

pub fn working_tree_clean() -> Result<bool, String> {
    let root = repo_root()?;
    let paths = list_changed_paths_in_root(&root)?;
    Ok(paths.is_empty())
}

fn run_status_output(mut cmd: Command) -> Result<GitOutput, String> {
    let output = cmd.output().map_err(|err| err.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(GitOutput { stdout, stderr })
    } else {
        let mut message = format!("command failed: {:?}", cmd);
        if !stderr.trim().is_empty() {
            message.push_str(&format!("\n{stderr}"));
        }
        Err(message)
    }
}

fn temp_note_path(filename: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!("gg-{}-{}", std::process::id(), filename));
    path
}
