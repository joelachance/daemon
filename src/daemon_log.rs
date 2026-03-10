//! Daemon log file for dashboard display. When GG_DAEMON=1, logs go to ~/.vibe-commits/daemon.log.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn log_path() -> PathBuf {
    if let Ok(p) = env::var("VIBE_DB_PATH") {
        if !p.trim().is_empty() {
            if let Some(parent) = PathBuf::from(&p).parent() {
                return parent.join("daemon.log");
            }
        }
    }
    let home = env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".vibe-commits").join("daemon.log")
}

pub fn init() {
    if env::var("GG_DAEMON").ok().as_deref() != Some("1") {
        return;
    }
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let mut guard = LOG_FILE.lock().unwrap();
        *guard = Some(file);
    }
}

pub fn log(msg: &str) {
    println!("{}", msg);
    let mut guard = match LOG_FILE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(ref mut f) = *guard {
        let _ = writeln!(f, "{}", msg);
        let _ = f.flush();
    }
}

pub fn log_path_for_reader() -> PathBuf {
    log_path()
}

#[macro_export]
macro_rules! daemon_log {
    ($($arg:tt)*) => {
        $crate::daemon_log::log(&format!($($arg)*))
    };
}
