# Claude Code session schema notes (macOS)

This file documents where Claude Code stores session data on this machine
and how to extract model, token, and prompt/response history.

## Locations

- Primary session logs:
  - `~/.claude/projects/<project_dir>/<session_id>.jsonl`
- Prompt history (user-only):
  - `~/.claude/history.jsonl`
- Session metadata (title/model/created/last activity):
  - `~/Library/Application Support/Claude/claude-code-sessions/<account>/<org>/<session>.json`

## Project directory mapping

`<project_dir>` is the repo path with `/` replaced by `-`:

- Repo path: `/Users/joe/git/daemon`
- Project dir: `-Users-joe-git-daemon`

If the repo is not present, open it in Claude Code to create the entry.

## JSONL entry schema (observed)

Each line in `<session_id>.jsonl` is a JSON object.

Common fields:

- `sessionId` (string)
- `uuid` (string)
- `timestamp` (ISO string)
- `cwd` (string)
- `gitBranch` (string)
- `version` (string)
- `type` (string: `user`, `assistant`, `progress`, etc.)

Assistant message entries:

- `type: "assistant"` and `message.role: "assistant"`
- `message.id` (string)
- `message.model` (string)
- `message.content` (array)
  - objects with `{ "type": "text", "text": "..." }`
  - objects with `{ "type": "tool_use", "name": "Read", ... }`
- `message.usage` (token usage)
  - `input_tokens`, `output_tokens`

User entries:

- `type: "user"` and `message.role: "user"`
- `message.content` (string or array)

Progress entries:

- `type: "progress"` and `data.type: "hook_progress"`

## Session metadata (observed)

From `claude-code-sessions/.../local_<id>.json`:

- `sessionId`, `cliSessionId`
- `cwd`, `originCwd`, `worktreePath`
- `createdAt`, `lastActivityAt`
- `model`, `title`, `isArchived`

## Mapping sessions to repos

1) Compute `<project_dir>` from the repo root path.
2) Read `<project_dir>/<session_id>.jsonl` to get `sessionId` and `cwd`.
3) Use `cwd` + repo root to confirm the session belongs to the repo.

## Event extraction plan

- Emit one event per assistant response.
- Use `message.content` items where `type == "text"` to build the response text.
- Skip assistant entries that have no text (tool-use only).
- Token usage from `message.usage` if present.
- No inactivity end detection.
