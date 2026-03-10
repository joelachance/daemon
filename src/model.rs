//! Resolve Llama model path: GG_LLAMA_MODEL env or default download from Hugging Face.

use crate::daemon_log;
use std::env;
use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT_REPO: &str = "bartowski/SmolLM2-360M-Instruct-GGUF";
pub const DEFAULT_FILE: &str = "SmolLM2-360M-Instruct-Q4_K_M.gguf";

static CACHED_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Return the model path to use: env override or default (download if needed).
pub fn default_model_path() -> Result<PathBuf, String> {
    #[cfg(feature = "llama-embedded")]
    {
        default_model_path_impl()
    }

    #[cfg(not(feature = "llama-embedded"))]
    {
        let _ = ();
        Err("llama-embedded feature not enabled".to_string())
    }
}

#[cfg(feature = "llama-embedded")]
fn default_model_path_impl() -> Result<PathBuf, String> {
    if let Ok(guard) = CACHED_PATH.lock() {
        if let Some(ref p) = *guard {
            daemon_log::log(&format!("daemon: using cached model path: {}", p.display()));
            return Ok(p.clone());
        }
    }

    daemon_log::log(&format!("daemon: resolving llama model path (default: {})...", DEFAULT_FILE));
    if let Ok(path) = env::var("GG_LLAMA_MODEL") {
        let path = path.trim();
        if !path.is_empty() {
            let expanded = expand_tilde(path);
            if expanded.exists() {
                daemon_log::log(&format!("daemon: using GG_LLAMA_MODEL at {}", expanded.display()));
                if let Ok(mut g) = CACHED_PATH.lock() {
                    *g = Some(expanded.clone());
                }
                return Ok(expanded);
            }
            return Err(format!("GG_LLAMA_MODEL file not found: {}", path));
        }
    }
    daemon_log::log(&format!("daemon: GG_LLAMA_MODEL not set, using default (may download)..."));
    let path = ensure_default_model()?;
    if let Ok(mut g) = CACHED_PATH.lock() {
        *g = Some(path.clone());
    }
    Ok(path)
}

/// Download the default model to HF cache if not present. Returns cache path.
pub fn ensure_default_model() -> Result<PathBuf, String> {
    #[cfg(feature = "llama-embedded")]
    {
        ensure_default_model_impl()
    }

    #[cfg(not(feature = "llama-embedded"))]
    {
        Err("llama-embedded feature not enabled".to_string())
    }
}

#[cfg(feature = "llama-embedded")]
fn ensure_default_model_impl() -> Result<PathBuf, String> {
    if let Some(p) = find_cached_default_model() {
        daemon_log::log(&format!("daemon: default model found in cache at {}", p.display()));
        return Ok(p);
    }

    use hf_hub::api::sync::Api;

    daemon_log::log(&format!("daemon: ensure_default_model: init hf-hub api..."));
    let api = Api::new().map_err(|e| format!("hf-hub init failed: {}", e))?;
    daemon_log::log(&format!("daemon: ensure_default_model: api ready, calling repo.get()..."));
    let repo = api.model(DEFAULT_REPO.to_string());
    let path = repo
        .get(DEFAULT_FILE)
        .map_err(|e| format!("failed to download model: {}", e))?;
    daemon_log::log(&format!("daemon: default model ready at {}", path.display()));
    Ok(path)
}

/// Check HF cache directly without hf-hub (avoids potential network/lock slowness).
#[cfg(feature = "llama-embedded")]
fn find_cached_default_model() -> Option<PathBuf> {
    let hub = env::var("HF_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("HOME").ok().map(|h| PathBuf::from(h).join(".cache").join("huggingface")))?;
    let refs_path = hub.join("hub").join("models--bartowski--SmolLM2-360M-Instruct-GGUF").join("refs").join("main");
    let commit = std::fs::read_to_string(&refs_path).ok()?;
    let commit = commit.trim();
    let path = hub
        .join("hub")
        .join("models--bartowski--SmolLM2-360M-Instruct-GGUF")
        .join("snapshots")
        .join(commit)
        .join(DEFAULT_FILE);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(path.trim_start_matches("~/"));
        }
    }
    if path == "~" {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}
