# Cursor SQLite schema notes (macOS)

This file documents where Cursor stores session data and the fields we can read.
It is based on local inspection of Cursor 2.5.x on macOS.

## Locations

- Global DB:
  - /Users/joe/Library/Application Support/Cursor/User/globalStorage/state.vscdb
- Workspace DB:
  - /Users/joe/Library/Application Support/Cursor/User/workspaceStorage/<workspace_id>/state.vscdb
- Workspace mapping file:
  - /Users/joe/Library/Application Support/Cursor/User/workspaceStorage/<workspace_id>/workspace.json
  - Key: "folder": "file:///Users/joe/git/<repo>"

## Tables

Both DBs have:
- ItemTable (key/value)
- cursorDiskKV (key/value)

Key and value are stored as TEXT or BLOB (JSON encoded).

## Known keys (workspace DB)

From `ItemTable` in workspace DB:
- aiService.prompts
  - JSON array of prompt entries (partial; not full chat)
  - Fields seen: text, commandType
- aiService.generations
  - JSON array of generation metadata (partial)
  - Fields seen: textDescription, type
- composer.composerData (in some workspaces)

These are partial metadata and do not include full prompt/response history.

## Known keys (global DB)

From `cursorDiskKV` in global DB:
- composerData:<composer_id>
  - JSON blob for a conversation session.
- bubbleId:<composer_id>:<bubble_id>
  - JSON blob for a single message "bubble" (prompt or response).

### composerData:<composer_id> (observed fields)

- fullConversationHeadersOnly
  - Array of objects or IDs referencing bubble IDs.
  - Each entry maps to bubbleId:<composer_id>:<bubble_id>.
- status
  - Observed value: "completed" (likely session finished).
- lastUpdatedAt (epoch ms)
- createdAt (epoch ms)
- isArchived (bool)
- modelConfig.modelName
  - Model identifier (e.g., "gpt-5.3-codex").
- contextTokensUsed
- contextTokenLimit
- latestChatGenerationUUID
- file contexts and attachments (arrays with file paths)
- conversationState (large blob; may contain status flags)

### bubbleId:<composer_id>:<bubble_id> (observed fields)

The JSON is large and nested. Observed fields (Cursor 2.5.x):
- type
  - 1 = user prompt
  - 2 = assistant bubble
- text
  - primary prompt/response text
- richText
  - user prompt JSON payload (string)
- thinking.text
  - assistant thinking block (when text is empty)
- tokenCount.inputTokens / tokenCount.outputTokens
- createdAt (ISO timestamp)
- toolFormerData
  - tool call record: name, rawArgs/params, result, status, toolCallId
- toolResults
  - array of tool result objects (can be empty)

## How to locate the right workspace

1) Enumerate workspaceStorage directories:
   - /Users/joe/Library/Application Support/Cursor/User/workspaceStorage/*
2) Read workspace.json to map to the repo path:
   - "folder": "file:///Users/joe/git/<repo>"

If the repo is not present, open it in Cursor to generate the workspace entry.

## Minimal extraction plan (current)

For a given repo path:
1) Find matching workspace_id (workspace.json folder matches repo).
2) Use global DB to list composerData entries containing that repo path.
3) Parse composerData to get:
   - composer_id (from key)
   - modelConfig.modelName
   - contextTokensUsed / contextTokenLimit
   - fullConversationHeadersOnly -> bubble IDs
4) For each bubble_id, read bubbleId:<composer_id>:<bubble_id> JSON and extract:
   - role
   - text (prompt/response)
   - tool calls / tool results (if present)

## Example queries (sqlite3)

List composerData keys:
  select key from cursorDiskKV where key like 'composerData:%';

Find composerData containing a repo path:
  select key from cursorDiskKV
  where cast(value as text) like '%/Users/joe/git/daemon%';

Fetch a composerData JSON:
  select cast(value as text) from cursorDiskKV
  where key = 'composerData:<composer_id>';

Fetch a bubble JSON:
  select cast(value as text) from cursorDiskKV
  where key = 'bubbleId:<composer_id>:<bubble_id>';

## Live session detection (proposed)

Goal: detect when a session ends, then summarize and store metadata.

Signals to watch:
- New composerData:<id> appears (session started).
- New bubbleId:<id>:<bubble_id> rows appear (new message).
- composerData fields updated over time (token counts, last generation id).

Implementation options:
- File system watcher on state.vscdb + -wal file.
- Periodic polling of cursorDiskKV keys and updated timestamps (no native timestamp in table; must compare JSON or track bubble count).
- For concurrency, open sqlite read-only with shared cache and short timeouts.

Session end heuristic (needs validation):
- No new bubbleId entries for N seconds after last assistant message.
- composerData has a status field indicating completion (field name TBD).

## Data we want to capture

From composerData:
- modelConfig.modelName
- contextTokensUsed / contextTokenLimit
- conversation id (composer_id)
- file context references

From bubbleId:
- prompt/response text
- tool calls and tool results (if present)
- per-message token usage (if present)

From Git:
- staged/unstaged changes at end of session
- diff summary
- final commit message + metadata (notes/refs)

## Open questions / TODO

- Identify the exact bubble fields that store user/assistant text.
- Confirm if Cursor stores per-message token usage and tool token usage.
- Confirm if composerData includes a session end flag.
- Validate mapping between workspace and composerData entries.
