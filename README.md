# Vibe Commit Daemon

`gg` now runs a draft-first daemon flow. It captures coding-session changes into local drafts and only writes git commits when a draft is explicitly approved.

## Usage
```
gg
gg start
gg status
gg ticket <session-id> <ticket>
```

## Runtime Behavior
- `gg` or `gg start` ensures the daemon is running and opens the dashboard.
- Session pollers read Claude/Cursor/OpenCode activity and record turns.
- Change capture uses `git diff -U0 HEAD` snapshots and stores per-change records.
- No `git add` or `git commit` happens during capture.
- Draft approval replays selected draft patches into real commits on a session branch.

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
  - `POST /sessions/:id/drafts/approve`

## Environment
- `GG_SOCKET` override daemon socket path
- `VIBE_DB_PATH` override SQLite path
- `GG_CURSOR_DB` override Cursor DB path
- `GG_CLAUDE_DIR` override Claude directory
- `GG_OPENCODE_DB` override OpenCode DB path
