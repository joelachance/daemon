use std::process::Command;

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
        stdin
            .write_all(json.as_bytes())
            .map_err(|err| err.to_string())?;
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

fn run_status(mut cmd: Command) -> Result<(), String> {
    let status = cmd.status().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {:?}", cmd))
    }
}
