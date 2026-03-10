# Vibe Commit Daemon

`gg` automates git for vibe coding sessions. It  drafts and only creates branches & writes commits after approval.

## Usage
```
gg
gg start
gg stop
gg status
gg ticket <session-id> <ticket>
gg install-model
```

## Runtime Behavior
- `gg` or `gg start` ensures the daemon is running and opens the dashboard.
- Session pollers read Cursor/Claude/OpenCode activity across discovered repos and record turns.
- Change capture uses `git diff -U0 HEAD` snapshots and stores per-change records.
- Active sessions require recent activity and are filtered to exclude ended/completed rows.
- No `git add` or `git commit` happens during capture or proposal editing.
- Dashboard proposes a branch + draft commits; user can edit branch and commit messages before apply.
- Apply replays selected draft patches into real commits on the chosen session branch.

## Storage
- Primary store: `~/.vibe-commits/db.sqlite` (or `VIBE_DB_PATH`).
- Session metadata ref is written on approval at:
  - `refs/vibe/sessions/<session-id>`

## Local API
- HTTP server runs on `127.0.0.1:7340`.
- Implemented endpoints include:
  - `GET /sessions`
  - `GET /sessions/:id`
  - `GET /sessions/:id/drafts`
  - `GET /sessions/:id/changes/unassigned`
  - `PATCH /sessions/:id/branch`
  - `PATCH /drafts/:id/message`
  - `POST /sessions/:id/drafts/approve`
  - `GET /config/llm_provider` – returns `{ "provider": "ollama" | "openai" | "anthropic" | "llama" }`
  - `PATCH /config/llm_provider` – body `{ "provider": "ollama" | "openai" | "anthropic" | "llama" }`

## Environment
- `GG_SOCKET` override daemon socket path
- `VIBE_DB_PATH` override SQLite path
- `GG_CURSOR_DB` override Cursor DB path
- `GG_CLAUDE_DIR` override Claude directory
- `GG_OPENCODE_DB` override OpenCode DB path
- `GG_DAEMON_LOG=0` hide the daemon log panel at the bottom of the dashboard
- **Commit messages** use an LLM (required). Provider priority: OpenAI (if `OPENAI_API_KEY`) > Anthropic (if `ANTHROPIC_API_KEY`) > Llama embedded (default) > Ollama.
- **Embedded (Llama)**: Default when no API keys. Run `gg install-model` to download SmolLM2-360M-Instruct-Q4_K_M.gguf from Hugging Face. Override with `GG_LLAMA_MODEL` (path to GGUF file). For better quality, try 1.7B: set `GG_LLAMA_MODEL` to a SmolLM2-1.7B-Instruct GGUF path.
- **Ollama**: Install from [ollama.com](https://ollama.com), run `ollama pull llama3.2` (or preferred model), ensure `ollama serve` is running. Uses `http://localhost:11434` by default. Override with `GG_OLLAMA_BASE_URL`, `GG_OLLAMA_MODEL`.
- **Model selection**: Press `/` in the dashboard for the slash menu → Models → choose OpenAI, Anthropic, Llama (embedded), or Ollama. For Ollama, select a model from your installed list.
- Override with `GG_OPENAI_MODEL`, `GG_ANTHROPIC_MODEL`.
