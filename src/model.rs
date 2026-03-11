//! Resolve Llama model path: GG_LLAMA_MODEL env or default download from Hugging Face.
//! Model registry maps IDs (qwen2.5-coder-3b, smollm2-1.7b) to Hugging Face repo/file.

use crate::daemon_log;
use crate::store;
use std::env;
use std::path::PathBuf;
use std::sync::Mutex;

/// Model registry: (id, display_name, Hugging Face repo, filename)
pub const MODEL_REGISTRY: &[(&str, &str, &str, &str)] = &[
    ("qwen2.5-coder-3b", "Qwen2.5-Coder-3B", "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF", "qwen2.5-coder-3b-instruct-q4_k_m.gguf"),
    ("smollm2-1.7b", "SmolLM2-1.7B", "bartowski/SmolLM2-1.7B-Instruct-GGUF", "SmolLM2-1.7B-Instruct-Q4_K_M.gguf"),
];

pub const DEFAULT_EMBEDDED_MODEL: &str = "qwen2.5-coder-3b";

static CACHED_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Clear cached model path so next call resolves fresh (e.g. after switching embedded model).
pub fn clear_model_cache() {
    if let Ok(mut g) = CACHED_PATH.lock() {
        *g = None;
    }
}

/// Return the model path to use: env override or config (download if needed).
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

    let model_id = store::get_embedded_model()
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_EMBEDDED_MODEL.to_string());
    daemon_log::log(&format!("daemon: resolving model path (id={})...", model_id));
    let path = model_path_for(&model_id)?;
    if let Ok(mut g) = CACHED_PATH.lock() {
        *g = Some(path.clone());
    }
    Ok(path)
}

/// Resolve path for a model ID from registry. Downloads if not cached.
#[cfg(feature = "llama-embedded")]
pub fn model_path_for(model_id: &str) -> Result<PathBuf, String> {
    let (repo, file) = MODEL_REGISTRY
        .iter()
        .find(|(id, _, _, _)| *id == model_id)
        .map(|(_, _, r, f)| (*r, *f))
        .ok_or_else(|| format!("unknown model id: {}", model_id))?;
    ensure_model_cached(repo, file)
}

/// List embedded model options for dashboard: (id, display_name)
#[cfg(feature = "llama-embedded")]
pub fn list_embedded_models() -> Vec<(String, String)> {
    MODEL_REGISTRY
        .iter()
        .map(|(id, display, _, _)| (id.to_string(), display.to_string()))
        .collect()
}

/// Download model to HF cache if not present. Returns cache path.
#[cfg(feature = "llama-embedded")]
fn ensure_model_cached(repo: &str, file: &str) -> Result<PathBuf, String> {
    if let Some(p) = find_cached_model(repo, file) {
        daemon_log::log(&format!("daemon: model found in cache at {}", p.display()));
        return Ok(p);
    }

    use hf_hub::api::sync::ApiBuilder;

    daemon_log::log(&format!("daemon: downloading model {}...", file));
    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .map_err(|e| format!("hf-hub init failed: {}", e))?;
    let repo_api = api.model(repo.to_string());
    let path = repo_api
        .get(file)
        .map_err(|e| format!("failed to download model: {}", e))?;
    daemon_log::log(&format!("daemon: model ready at {}", path.display()));
    Ok(path)
}

/// Check HF cache directly without hf-hub (avoids potential network/lock slowness).
#[cfg(feature = "llama-embedded")]
fn find_cached_model(repo: &str, file: &str) -> Option<PathBuf> {
    let hub = env::var("HF_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::var("HOME").ok().map(|h| PathBuf::from(h).join(".cache").join("huggingface")))?;
    let dir_name = repo.replace('/', "--");
    let refs_path = hub.join("hub").join(format!("models--{}", dir_name)).join("refs").join("main");
    let commit = std::fs::read_to_string(&refs_path).ok()?;
    let commit = commit.trim();
    let path = hub
        .join("hub")
        .join(format!("models--{}", dir_name))
        .join("snapshots")
        .join(commit)
        .join(file);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Download the default model. Used by install-model CLI.
pub fn ensure_default_model() -> Result<PathBuf, String> {
    #[cfg(feature = "llama-embedded")]
    {
        let model_id = store::get_embedded_model()
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_EMBEDDED_MODEL.to_string());
        model_path_for(&model_id)
    }

    #[cfg(not(feature = "llama-embedded"))]
    {
        let _ = ();
        Err("llama-embedded feature not enabled".to_string())
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
