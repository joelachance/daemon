#![allow(dead_code)]

use ignore::gitignore::GitignoreBuilder;
use ignore::WalkBuilder;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default, Clone)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
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

pub fn commit_in_root_with_footer(
    root: &str,
    subject: &str,
    footer: Option<&str>,
    trailers: &[(String, String)],
) -> Result<(Option<String>, GitOutput), String> {
    if staged_is_clean(root)? {
        return Ok((None, GitOutput::default()));
    }

    let mut extra_lines = Vec::new();
    if let Some(value) = footer {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            extra_lines.push(trimmed.to_string());
        }
    }
    for (key, value) in trailers {
        extra_lines.push(format!("{key}: {value}"));
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("commit").arg("-m").arg(subject);

    if !extra_lines.is_empty() {
        cmd.arg("-m").arg(extra_lines.join("\n"));
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

pub fn branch_name() -> Result<Option<String>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err("git rev-parse --abbrev-ref failed".to_string());
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(name))
    }
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

pub fn push() -> Result<(), String> {
    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&root).arg("push");
    run_status(cmd)
}

pub fn push_branch_in_root(root: &str, branch: &str) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("push")
        .arg("origin")
        .arg(branch);
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

pub fn working_tree_clean_in_root(root: &str) -> Result<bool, String> {
    let paths = list_changed_paths_in_root(root)?;
    Ok(paths.is_empty())
}

pub fn head_commit_in_root(root: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err("git rev-parse HEAD failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn diff_u0_in_root(root: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .arg("-U0")
        .arg("HEAD")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err("git diff -U0 failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn diff_u3_in_root(root: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .arg("-U3")
        .arg("HEAD")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err("git diff -U3 failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn checkout_new_branch_from(root: &str, branch: &str, base: &str) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("checkout")
        .arg("-B")
        .arg(branch)
        .arg(base);
    run_status(cmd)
}

pub fn apply_patch_in_root(root: &str, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("apply")
        .arg("--unidiff-zero")
        .arg("--whitespace=nowarn")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    {
        let stdin = child.stdin.as_mut().ok_or("missing git apply stdin")?;
        use std::io::Write;
        stdin
            .write_all(patch.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn add_files_in_root(root: &str, files: &[String]) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("add").arg("--");
    for file in files {
        cmd.arg(file);
    }
    run_status(cmd)
}

pub fn commit_message_in_root(root: &str, message: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("commit")
        .arg("-m")
        .arg(message);
    run_status(cmd)?;
    head_commit_in_root(root)
}

pub fn write_ref_blob_in_root(root: &str, ref_name: &str, json: &str) -> Result<(), String> {
    let mut hash = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("hash-object")
        .arg("-w")
        .arg("--stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    {
        let stdin = hash.stdin.as_mut().ok_or("missing hash-object stdin")?;
        use std::io::Write;
        stdin.write_all(json.as_bytes()).map_err(|err| err.to_string())?;
    }
    let hash_output = hash.wait_with_output().map_err(|err| err.to_string())?;
    if !hash_output.status.success() {
        return Err("git hash-object failed".to_string());
    }
    let blob_sha = String::from_utf8_lossy(&hash_output.stdout).trim().to_string();
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("update-ref")
        .arg(ref_name)
        .arg(blob_sha);
    run_status(cmd)
}

pub fn list_remotes_in_root(root: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
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

pub fn has_remote_in_root(root: &str) -> Result<bool, String> {
    Ok(!list_remotes_in_root(root)?.is_empty())
}

pub fn branch_exists_in_root(root: &str, branch: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map_err(|err| err.to_string())?;
    Ok(output.status.success())
}

pub fn checkout_branch_in_root(root: &str, branch: &str) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("checkout").arg(branch);
    run_status(cmd)
}

pub fn create_branch_in_root(root: &str, branch: &str) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("checkout")
        .arg("-b")
        .arg(branch);
    run_status(cmd)
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
