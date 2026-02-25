use crate::daemon;
use crate::session::{TokenUsage, ToolTokenUsage};
use std::env;

const TOOLS: &[&str] = &["claude", "cursor", "opencode"];
const COMMANDS: &[&str] = &[
    "log",
    "status",
    "commit",
    "show",
    "blame",
    "open",
    "session",
    "help",
];

pub fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help" | "help")
}

pub fn is_tool(arg: &str) -> bool {
    TOOLS.iter().any(|tool| tool.eq_ignore_ascii_case(arg))
}

pub fn is_command(arg: &str) -> bool {
    COMMANDS.iter().any(|cmd| cmd.eq_ignore_ascii_case(arg))
}

pub fn run_command(command: &str, args: &[String]) -> Result<(), String> {
    match command {
        "log" => {
            println!("gg log: not implemented");
        }
        "status" => {
            println!("gg status: not implemented");
        }
        "commit" => {
            println!("gg commit: not implemented");
        }
        "show" => {
            println!("gg show: not implemented");
        }
        "blame" => {
            println!("gg blame: not implemented");
        }
        "open" => {
            println!("gg open: not implemented");
        }
        "session" => {
            return run_session_command(args);
        }
        "help" => {
            print_help();
        }
        _ => {
            return Err(format!("unsupported command: {command}"));
        }
    }

    Ok(())
}

fn run_session_command(args: &[String]) -> Result<(), String> {
    let subcommand = args.get(0).map(String::as_str).unwrap_or("");
    if subcommand != "event" {
        return Err(
            "usage: gg session event --session <id> --summary <text> [--path <file>]... [--tokens-in <n> --tokens-out <n> --tokens-total <n>] [--tool-token <tool>:<input>:<output>[:<type>]]... [--git-stdout]".to_string(),
        );
    }

    let mut session_id: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut tokens_in: Option<u64> = None;
    let mut tokens_out: Option<u64> = None;
    let mut tokens_total: Option<u64> = None;
    let mut tool_tokens: Vec<ToolTokenUsage> = Vec::new();
    let mut git_stdout = env_flag("GG_GIT_STDOUT");

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                i += 1;
                session_id = args.get(i).cloned();
            }
            "--summary" => {
                i += 1;
                summary = args.get(i).cloned();
            }
            "--path" => {
                i += 1;
                if let Some(value) = args.get(i) {
                    paths.push(value.clone());
                }
            }
            "--tokens-in" => {
                i += 1;
                tokens_in = parse_u64(args.get(i))?;
            }
            "--tokens-out" => {
                i += 1;
                tokens_out = parse_u64(args.get(i))?;
            }
            "--tokens-total" => {
                i += 1;
                tokens_total = parse_u64(args.get(i))?;
            }
            "--tool-token" => {
                i += 1;
                if let Some(value) = args.get(i) {
                    tool_tokens.push(parse_tool_token(value)?);
                }
            }
            "--git-stdout" => {
                git_stdout = true;
            }
            _ => {}
        }
        i += 1;
    }

    let session_id = session_id.ok_or("missing --session <id>")?;
    let summary = summary.ok_or("missing --summary <text>")?;

    let tokens = build_tokens(tokens_in, tokens_out, tokens_total)?;

    daemon::send_event(&session_id, &summary, &paths, tokens, tool_tokens, git_stdout)
}

fn parse_u64(value: Option<&String>) -> Result<Option<u64>, String> {
    match value {
        Some(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("invalid number: {raw}")),
        None => Ok(None),
    }
}

fn build_tokens(
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    tokens_total: Option<u64>,
) -> Result<Option<TokenUsage>, String> {
    if tokens_in.is_none() && tokens_out.is_none() && tokens_total.is_none() {
        return Ok(None);
    }

    let input = tokens_in.ok_or("missing --tokens-in <n>")?;
    let output = tokens_out.ok_or("missing --tokens-out <n>")?;
    let total = tokens_total.unwrap_or(input + output);

    Ok(Some(TokenUsage { input, output, total }))
}

fn parse_tool_token(raw: &str) -> Result<ToolTokenUsage, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err("invalid --tool-token format, expected tool:input:output[:type]".to_string());
    }

    let tool = parts[0].to_string();
    let input = parts[1]
        .parse::<u64>()
        .map_err(|_| format!("invalid tool token input: {}", parts[1]))?;
    let output = parts[2]
        .parse::<u64>()
        .map_err(|_| format!("invalid tool token output: {}", parts[2]))?;
    let total = input + output;
    let tool_type = if parts.len() == 4 {
        Some(parts[3].to_string())
    } else {
        None
    };

    Ok(ToolTokenUsage {
        tool,
        tool_type,
        input,
        output,
        total,
    })
}

fn env_flag(key: &str) -> bool {
    matches!(
        env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

pub fn print_help() {
    println!(
        "gg: AI-native Git/JJ porcelain\n\
\n\
Usage:\n\
  gg <tool> [args...]\n\
  gg <command> [args...]\n\
\n\
Tools (launches daemon + tool):\n\
  claude | cursor | opencode\n\
\n\
Commands:\n\
  log | status | commit | show | blame | open | session\n\
\n\
Examples:\n\
  gg claude\n\
  gg log\n\
  gg status\n\
  gg session event --session ses_123 --summary \"Add tests\" --path src/lib.rs\n\
  gg session event --session ses_123 --summary \"Fix bug\" --tokens-in 1200 --tokens-out 250 --tool-token bash:30:10:system --git-stdout\n\
"
    );
}

pub fn print_help_with_error(message: &str) {
    eprintln!("gg: {message}");
    print_help();
}
