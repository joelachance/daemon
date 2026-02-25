use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use ignore::gitignore::GitignoreBuilder;

#[derive(Debug, Default, Clone)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

pub fn stage_paths(paths: &[String]) -> Result<GitOutput, String> {
    if paths.is_empty() {
        return Ok(GitOutput::default());
    }

    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&root).arg("add").arg("--");
    for path in paths {
        cmd.arg(path);
    }

    run_status_output(cmd)
}


pub fn list_changed_paths() -> Result<Vec<String>, String> {
    let root = repo_root()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
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

pub fn filter_ignored_paths(paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let root = repo_root()?;
    let mut builder = GitignoreBuilder::new(&root);
    let root_path = Path::new(&root);
    let gitignore_path = root_path.join(".gitignore");
    let ggignore_path = root_path.join(".ggignore");
    let info_exclude_path = root_path.join(".git").join("info").join("exclude");

    if gitignore_path.exists() {
        let _ = builder.add(gitignore_path);
    }
    if ggignore_path.exists() {
        let _ = builder.add(ggignore_path);
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

pub fn commit(
    summary: &str,
    trailers: &[(String, String)],
) -> Result<(Option<String>, GitOutput), String> {
    let root = repo_root()?;
    if staged_is_clean(&root)? {
        return Ok((None, GitOutput::default()));
    }

    let trailer_block = trailers
        .iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<String>>()
        .join("\n");

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("commit")
        .arg("-m")
        .arg(summary);

    if !trailer_block.is_empty() {
        cmd.arg("-m").arg(trailer_block);
    }

    let output = run_status_output(cmd)?;
    let mut rev_cmd = Command::new("git");
    rev_cmd.arg("-C").arg(&root).arg("rev-parse").arg("HEAD");
    let sha = run_stdout(rev_cmd)?;
    Ok((Some(sha.trim().to_string()), output))
}

pub fn append_session_event(session_id: &str, payload: &str) -> Result<String, String> {
    let root = repo_root()?;
    let ref_name = format!("refs/gg/sessions/{session_id}");

    let mut blob_cmd = Command::new("git");
    blob_cmd
        .arg("-C")
        .arg(&root)
        .arg("hash-object")
        .arg("-w")
        .arg("--stdin");
    let blob_hash = run_stdout_with_input(blob_cmd, payload)?;

    let tree_input = format!("100644 blob {}\tevent.json\n", blob_hash.trim());
    let mut tree_cmd = Command::new("git");
    tree_cmd.arg("-C").arg(&root).arg("mktree");
    let tree_hash = run_stdout_with_input(tree_cmd, &tree_input)?;

    let mut parent_cmd = Command::new("git");
    parent_cmd
        .arg("-C")
        .arg(&root)
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
        .arg(&root)
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
        .arg(&root)
        .arg("update-ref")
        .arg(&ref_name)
        .arg(&commit_hash);
    run_status(update_cmd)?;

    Ok(commit_hash)
}

pub fn write_notes(ref_name: &str, commit_hash: &str, payload: &str) -> Result<(), String> {
    let root = repo_root()?;
    let temp_path = temp_note_path("gg-note.json");
    fs::write(&temp_path, payload).map_err(|err| err.to_string())?;

    let status = Command::new("git")
        .arg("-C")
        .arg(&root)
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
        stdin.write_all(input.as_bytes()).map_err(|err| err.to_string())?;
    }

    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!("command failed: {:?}", cmd));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn set_local_config(key: &str, value: &str) -> Result<bool, String> {
    let current = get_local_config(key)?;
    if current.as_deref() == Some(value) {
        return Ok(false);
    }

    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("config")
        .arg("--local")
        .arg(key)
        .arg(value);
    run_status(cmd)?;
    Ok(true)
}

pub fn add_local_config_if_missing(key: &str, value: &str) -> Result<bool, String> {
    let values = get_local_config_values(key)?;
    if values.iter().any(|item| item == value) {
        return Ok(false);
    }

    let root = repo_root()?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&root)
        .arg("config")
        .arg("--local")
        .arg("--add")
        .arg(key)
        .arg(value);
    run_status(cmd)?;
    Ok(true)
}

pub fn get_local_config(key: &str) -> Result<Option<String>, String> {
    let values = get_local_config_values(key)?;
    Ok(values.into_iter().next())
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
    path.push(format!(
        "gg-{}-{}",
        std::process::id(),
        filename
    ));
    path
}
