//! Local Llama inference for commit message generation.
//! Requires `llama-embedded` feature and GG_LLAMA_MODEL env var or model_path.

use crate::daemon_log;
use std::env;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_PROMPT_CHARS: usize = 4096;
const PROMPT_SUFFIX_CHARS: usize = 350; // preserve "Output ONLY a JSON object..." at end

/// Run prompt completion. Returns generated text.
/// Config from env: GG_LLAMA_MODEL (when model_path is None), GG_LLAMA_MAX_TOKENS (default 500), GG_LLAMA_TIMEOUT_MS (default 30000).
pub fn run_completion(
    prompt: &str,
    max_tokens: usize,
    timeout_ms: u64,
    model_path: Option<&Path>,
) -> Result<String, String> {
    #[cfg(feature = "llama-embedded")]
    {
        run_completion_impl(prompt, max_tokens, timeout_ms, model_path)
    }

    #[cfg(not(feature = "llama-embedded"))]
    {
        let _ = (prompt, max_tokens, timeout_ms, model_path);
        Err("llama-embedded feature not enabled".to_string())
    }
}

#[cfg(feature = "llama-embedded")]
fn get_cached_model(
    backend: &llama_cpp_2::llama_backend::LlamaBackend,
    model_path: &str,
) -> Result<std::sync::MutexGuard<'static, Option<llama_cpp_2::model::LlamaModel>>, String> {
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::LlamaModel;
    use std::time::Instant;

    static CACHED_MODEL: Mutex<Option<LlamaModel>> = Mutex::new(None);

    let mut guard = CACHED_MODEL.lock().unwrap();
    if guard.is_none() {
        daemon_log::log(&format!("daemon: llama loading model from disk: {}", model_path));
        let load_start = Instant::now();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut s = 0u64;
            while !stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(1));
                s += 1;
                    daemon_log::log(&format!("daemon: llama still loading model... {}s", s));
            }
        });
        let model = LlamaModel::load_from_file(backend, model_path, &LlamaModelParams::default())
            .map_err(|e| format!("failed to load llama model {}: {}", model_path, e))?;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = thread.join();
        daemon_log::log(&format!(
            "daemon: llama model loaded in {:.1}s",
            load_start.elapsed().as_secs_f64()
        ));
        *guard = Some(model);
    } else {
        daemon_log::log(&format!("daemon: llama using cached model"));
    }
    Ok(guard)
}

#[cfg(feature = "llama-embedded")]
fn run_completion_impl(
    prompt: &str,
    max_tokens: usize,
    timeout_ms: u64,
    model_path_override: Option<&Path>,
) -> Result<String, String> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;
    use llama_cpp_2::sampling::LlamaSampler;

    daemon_log::log(&format!("daemon: llama run_completion start"));
    let model_path = match model_path_override {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            let path = env::var("GG_LLAMA_MODEL").map_err(|_| "GG_LLAMA_MODEL not set")?;
            let path = path.trim();
            if path.is_empty() {
                return Err("GG_LLAMA_MODEL must point to a GGUF model file".to_string());
            }
            path.to_string()
        }
    };
    daemon_log::log(&format!("daemon: llama model_path={} prompt_len={}", model_path, prompt.len()));

    let bounded_prompt = if prompt.len() > MAX_PROMPT_CHARS {
        let head_end = (MAX_PROMPT_CHARS - PROMPT_SUFFIX_CHARS - 30).min(prompt.len());
        let head_end = prompt.floor_char_boundary(head_end);
        let suffix_start = prompt.len().saturating_sub(PROMPT_SUFFIX_CHARS);
        let suffix_start = prompt.floor_char_boundary(suffix_start);
        format!(
            "{}\n... (truncated)\n\n{}",
            &prompt[..head_end],
            &prompt[suffix_start..]
        )
    } else {
        prompt.to_string()
    };

    daemon_log::log(&format!("daemon: llama getting backend..."));
    let backend = llama_backend().map_err(|e| format!("llama backend init failed: {}", e))?;
    daemon_log::log(&format!("daemon: llama getting model (load or cached)..."));
    let model_guard = get_cached_model(backend, &model_path)?;
    let model = model_guard.as_ref().unwrap();

    let max_tokens = max_tokens.max(64);
    let n_ctx = env::var("GG_LLAMA_N_CTX")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(2048)
        .clamp(512, 32768);
    let mut ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(n_ctx).ok_or("invalid n_ctx")?))
        .with_n_batch(n_ctx);
    let threads: i32 = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .try_into()
        .unwrap_or(4);
    ctx_params = ctx_params.with_n_threads(threads);
    ctx_params = ctx_params.with_n_threads_batch(threads);

    daemon_log::log(&format!("daemon: llama creating context (n_ctx={})...", n_ctx));
    let ctx_start = Instant::now();
    let ctx_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ctx_stop_t = ctx_stop.clone();
    let ctx_thread = std::thread::spawn(move || {
        let mut s = 0u64;
        while !ctx_stop_t.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            s += 1;
            daemon_log::log(&format!("daemon: llama still creating context... {}s", s));
        }
    });
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| format!("failed to initialize llama context: {}", e))?;
    ctx_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = ctx_thread.join();
    daemon_log::log(&format!(
        "daemon: llama context ready in {:.1}s",
        ctx_start.elapsed().as_secs_f64()
    ));

    daemon_log::log(&format!(
        "daemon: llama tokenizing prompt ({} chars)...",
        bounded_prompt.len()
    ));
    let tokenize_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tokenize_stop_t = tokenize_stop.clone();
    let tokenize_thread = std::thread::spawn(move || {
        let mut s = 0u64;
        while !tokenize_stop_t.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            s += 1;
            daemon_log::log(&format!("daemon: llama still tokenizing... {}s", s));
        }
    });
    let prompt_tokens = model
        .str_to_token(&bounded_prompt, AddBos::Always)
        .map_err(|e| format!("failed to tokenize prompt: {}", e))?;
    tokenize_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = tokenize_thread.join();
    daemon_log::log(&format!("daemon: llama tokenized {} tokens", prompt_tokens.len()));
    if prompt_tokens.is_empty() {
        return Err("tokenized prompt is empty".to_string());
    }

    let n_batch = ctx.n_batch() as usize;
    daemon_log::log(&format!(
        "daemon: llama decoding prompt ({} tokens, batch={})...",
        prompt_tokens.len(),
        n_batch
    ));
    let decode_start = Instant::now();
    let decode_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let decode_stop_t = decode_stop.clone();
    let decode_thread = std::thread::spawn(move || {
        let mut s = 0u64;
        while !decode_stop_t.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            s += 1;
            daemon_log::log(&format!("daemon: llama still decoding prompt... {}s", s));
        }
    });
    let mut pos = 0_i32;
    let mut last_batch_size = 0;
    for chunk in prompt_tokens.chunks(n_batch) {
        let mut batch = LlamaBatch::new(chunk.len().min(n_batch), 1);
        let last_global = (prompt_tokens.len() - 1) as i32;
        for (i, &token) in chunk.iter().enumerate() {
            let p = pos + i as i32;
            batch
                .add(token, p, &[0], p == last_global)
                .map_err(|e| format!("failed adding prompt token to batch: {}", e))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| format!("failed initial llama decode: {}", e))?;
        last_batch_size = chunk.len();
        pos += chunk.len() as i32;
    }
    decode_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = decode_thread.join();
    daemon_log::log(&format!(
        "daemon: llama prompt decoded in {:.1}s",
        decode_start.elapsed().as_secs_f64()
    ));

    let mut n_cur = prompt_tokens.len() as i32;
    let mut batch = LlamaBatch::new(512, 1);

    let mut sampler =
        LlamaSampler::chain_simple([LlamaSampler::dist(42), LlamaSampler::greedy()]);
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut out = String::new();
    let start = Instant::now();
    let mut sample_idx = last_batch_size as i32 - 1;

    daemon_log::log(&format!(
        "daemon: llama generating (max_tokens={} timeout_ms={})...",
        max_tokens, timeout_ms
    ));
    let mut last_log_secs = 0u64;
    for t in 0..max_tokens {
        if start.elapsed().as_millis() as u64 > timeout_ms {
            break;
        }

        let token = sampler.sample(&ctx, sample_idx);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }

        let piece = model
            .token_to_piece(token, &mut decoder, true, None)
            .map_err(|e| format!("failed converting token to text: {}", e))?;
        out.push_str(&piece);

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| format!("failed preparing decode batch: {}", e))?;
        ctx.decode(&mut batch)
            .map_err(|e| format!("failed llama decode loop: {}", e))?;
        n_cur += 1;
        sample_idx = 0;

        let elapsed = start.elapsed().as_secs();
        if elapsed > last_log_secs {
            last_log_secs = elapsed;
            daemon_log::log(&format!(
                "daemon: llama generating... {} tokens, {}s elapsed",
                t + 1,
                elapsed
            ));
        }
    }

    daemon_log::log(&format!(
        "daemon: llama done in {:.1}s, output len={}",
        start.elapsed().as_secs_f64(),
        out.len()
    ));
    if out.trim().is_empty() {
        return Err("llama output was empty".to_string());
    }
    Ok(out)
}

#[cfg(feature = "llama-embedded")]
fn llama_backend() -> Result<&'static llama_cpp_2::llama_backend::LlamaBackend, String> {
    use llama_cpp_2::llama_backend::LlamaBackend;

    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    Ok(BACKEND.get_or_init(|| {
        daemon_log::log(&format!("daemon: llama initializing backend (first call)..."));
        let init_start = std::time::Instant::now();
        let mut backend = LlamaBackend::init().expect("failed to initialize llama backend");
        backend.void_logs();
        daemon_log::log(&format!(
            "daemon: llama backend ready in {:.1}s",
            init_start.elapsed().as_secs_f64()
        ));
        backend
    }))
}
