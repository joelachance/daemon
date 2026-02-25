# gg (daemon prototype)

Minimal Rust skeleton for an AI-native Git/JJ porcelain wrapper.

## Behavior
- `gg <tool>` ensures the daemon is running and attempts to spawn the tool.
- `gg session event ...` sends an event to the daemon, which stages changes,
  commits with trailers, appends a session ref, and writes git notes.
- Staging respects `.gitignore` plus a root `.ggignore` file (if present).

## Example
```
gg session event --session ses_123 --summary "Add tests" --path src/lib.rs
gg session event --session ses_123 --summary "Fix bug" --tokens-in 1200 --tokens-out 250 --tool-token bash:30:10:system --git-stdout
```

## Joke
Why do Rust developers never get lost? Because they always know their lifetimes.
