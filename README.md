# gg (daemon prototype)

Minimal Rust skeleton for an AI-native Git/JJ porcelain wrapper.

## Behavior
- `gg <tool>` ensures the daemon is running and attempts to spawn the tool.
- `gg session event ...` sends an event to the daemon, which stages changes,
  commits with trailers, appends a session ref, and writes git notes.
- Staging respects `.gitignore` plus a root `.ggignore` file (if present).
- AI metadata is stored in git notes and session refs, not commit messages.

## Init
Run `gg init` once per repo to configure git notes/refs and co-author identity.
It will prompt for a co-author name unless you pass `--coauthor` or
`--no-coauthor`.

## Output customization
Colors and spinner behavior can be configured via environment variables:
- `GG_SPINNER=off` disables the spinner (default: on)
- `GG_COLOR_STAGED` (default: green)
- `GG_COLOR_COUNT` (default: cyan)
- `GG_COLOR_COMMITTED` (default: green)
- `GG_COLOR_HASH` (default: yellow)
- `GG_COLOR_DIM` (default: dim/no color)
- Use `--compact` for a one-line summary (Pretty Mode is default).

Supported color names: black, red, green, yellow, blue, magenta, cyan, white,
brightblack/gray, brightred, brightgreen, brightyellow, brightblue,
brightmagenta, brightcyan, brightwhite. Use `none` to disable a color.

## Example
```
gg init
gg session event --session ses_123 --summary "Add tests" --path src/lib.rs
gg session event --session ses_123 --summary "Fix bug" --tokens-in 1200 --tokens-out 250 --tool-token bash:30:10:system --git-stdout --compact
```

## Joke
Why do Rust developers never get lost? Because they always know their lifetimes.
