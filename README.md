# gg (daemon prototype)

AI-native Git wrapper that watches coding sessions and auto-commits.

## Usage
```
gg
gg status
gg "<prompt>"
```

## Behavior
- `gg` starts the daemon in the foreground and listens for sessions.
- Ctrl+C ends all active sessions and writes an end event per session.
- Assistant responses trigger auto-commit with local session metadata.
- `gg status` opens the review UI.
- Any other args are treated as a natural language prompt.

## Review UI (`gg status`)
- Arrows or j/k navigate
- Space toggles selection
- `s` squash, `a` amend, `u` undo
- Enter accepts as-is and pushes (if remote exists and tree is clean)

## Storage
- Session metadata: `.git/gg/sessions/<id>` (local only)
- Ignore rules: `.gitignore` and `.ggignore` are treated the same

## Prompt mode
- Uses Amazon Bedrock (Nova Micro) in `us-west-2`
- Always human-in-the-loop: shows git commands and asks for confirmation

## Environment
- `GG_SOCKET` override daemon socket path
- `GG_BEDROCK_MODEL` override Bedrock model id
- `GG_OPENCODE_DB` override OpenCode DB path
- `GG_OPENCODE_TIMEOUT_SECS` OpenCode inactivity soft end (default 600)
- `GG_CLAUDE_DIR` override Claude Code data directory
