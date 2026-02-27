# OpenCode session schema (macOS)

This file documents where OpenCode stores session data and how to extract
assistant responses for harvesting.

## Locations

- Default DB (SQLite):
  - /Users/joe/.local/share/opencode/opencode.db
  - WAL/SHM: /Users/joe/.local/share/opencode/opencode.db-wal / .db-shm
- Overrides:
  - GG_OPENCODE_DB (preferred for gg)
  - OPENCODE_DB (fallback)

## Tables

- project
  - id (text, primary key)
  - worktree (repo root path)
  - vcs, name, time_created, time_updated, sandboxes, commands
- session
  - id (text, primary key)
  - project_id (FK -> project.id)
  - directory (repo root path)
  - title, version, summary_* fields
  - time_created, time_updated, time_archived
- message
  - id, session_id, time_created, time_updated
  - data (JSON string)
- part
  - id, message_id, session_id, time_created, time_updated
  - data (JSON string)

## Message data (observed)

`message.data` JSON (assistant messages) includes:
- role ("assistant" or "user")
- modelID, providerID
- mode, agent
- path (cwd/root)
- tokens: { input, output, total, cache }
- time { created, completed }
- finish (e.g., "tool-calls")

## Part data (observed)

`part.data` JSON includes:
- type: text | reasoning | tool | step-start | step-finish | ...
- text (for type = "text")
- tool details for tool parts

Assistant response text is stored in `part.data` where:
- message.role == "assistant"
- part.type == "text"

## Session -> repo mapping

Use either:
- session.directory == repo root path
- project.worktree == repo root path

## Event extraction (gg)

For a given repo root:
1) Join session/project/message/part where session.directory or project.worktree
   matches root.
2) Filter message.role == "assistant" and part.type == "text".
3) Emit a gg session event per assistant response:
   - session_id: session.id
   - summary: first non-empty line of part.text (truncate if long)
   - tokens: message.tokens (if present)
   - meta: ids, model/provider, timestamps, path, title, directory
4) Persist last seen part time per session in:
   - .git/gg/opencode.json

## Session end detection

Explicit end:
- If the user prompt contains `/exit`, emit an end event (`meta.end = true`).

Soft end (timeout):
- If no assistant response arrives for 600s (default), emit a soft end event
  (`meta.soft_end = true`).
- Override via `GG_OPENCODE_TIMEOUT_SECS`.
