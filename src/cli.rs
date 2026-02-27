use crate::bedrock::{BedrockClient, BedrockPlan};
use crate::daemon;
use crate::git;
use crate::status;
use std::io::{self, Write};

const COMMANDS: &[&str] = &["status"];

pub fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

pub fn is_command(arg: &str) -> bool {
    COMMANDS.iter().any(|cmd| cmd.eq_ignore_ascii_case(arg))
}

pub fn run_command(command: &str, _args: &[String]) -> Result<(), String> {
    match command {
        "status" => run_status_command(),
        _ => Err(format!("unsupported command: {command}")),
    }
}

pub fn run_prompt(prompt: &str) -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    println!("gg prompt: {prompt}");

    let repo_root = git::repo_root().ok();
    let plan = fetch_plan(prompt, repo_root.as_deref())?;
    print_plan(&plan);

    if plan.commands.is_empty() {
        println!("gg prompt: no commands generated");
        return Ok(());
    }

    if !confirm_plan()? {
        println!("gg prompt: cancelled");
        return Ok(());
    }

    for command in plan.commands {
        execute_git_command(&command)?;
    }
    Ok(())
}

pub fn print_banner() {
    println!(
        "{art}\nSatori Computer Co",
        art = r"██████╗ ██╗████████╗██████╗ ██████╗  ██████╗
██╔════╝ ██║╚══██╔══╝██╔══██╗██╔══██╗██╔═══██╗
██║  ███╗██║   ██║   ██████╔╝██████╔╝██║   ██║
██║   ██║██║   ██║   ██╔═══╝ ██╔══██╗██║   ██║
╚██████╔╝██║   ██║   ██║     ██║  ██║╚██████╔╝
 ╚═════╝ ╚═╝   ╚═╝   ╚═╝     ╚═╝  ╚═╝ ╚═════╝"
    );
    let _ = io::stdout().flush();
}

pub fn print_help() {
    println!(
        "gg - AI-native Git/JJ porcelain\n\
\n\
Usage:\n\
  gg\n\
  gg status\n\
  gg \"<prompt>\"\n\
  gg -h | --help\n\
\n\
Behavior:\n\
  - gg starts the daemon and listens for sessions\n\
  - Ctrl+C ends all active sessions\n\
  - gg status opens the review UI\n\
  - any other args are treated as a prompt\n\
\n\
Examples:\n\
  gg\n\
  gg status\n\
  gg \"summarize recent changes\"\n\
"
    );
}

fn run_status_command() -> Result<(), String> {
    status::run_status_ui()
}

fn fetch_plan(prompt: &str, repo_root: Option<&str>) -> Result<BedrockPlan, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;

    runtime.block_on(async {
        let client = BedrockClient::new().await?;
        client.plan_git_commands(prompt, repo_root).await
    })
}

fn print_plan(plan: &BedrockPlan) {
    if !plan.summary.trim().is_empty() {
        println!("plan summary: {}", plan.summary.trim());
    }

    if !plan.assumptions.is_empty() {
        println!("assumptions:");
        for item in &plan.assumptions {
            if !item.trim().is_empty() {
                println!("- {}", item.trim());
            }
        }
    }

    if !plan.risks.is_empty() {
        println!("risks:");
        for item in &plan.risks {
            if !item.trim().is_empty() {
                println!("- {}", item.trim());
            }
        }
    }

    if !plan.commands.is_empty() {
        println!("commands:");
        for (index, command) in plan.commands.iter().enumerate() {
            println!("{}. {}", index + 1, command.trim());
        }
    }
}

fn confirm_plan() -> Result<bool, String> {
    print!("Proceed with these git commands? [y/N] ");
    io::stdout().flush().map_err(|err| err.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| err.to_string())?;
    let answer = input.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

fn execute_git_command(command: &str) -> Result<(), String> {
    let tokens = tokenize_command(command)?;
    if tokens.first().map(String::as_str) != Some("git") {
        return Err(format!("unsupported command: {command}"));
    }
    let subcommand = tokens
        .get(1)
        .ok_or_else(|| format!("invalid git command: {command}"))?;
    let args = tokens
        .iter()
        .skip(2)
        .cloned()
        .collect::<Vec<String>>();
    git::run_passthrough(subcommand, &args)
}

fn tokenize_command(command: &str) -> Result<Vec<String>, String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err("empty command".to_string());
    }

    let forbidden = ['|', '&', ';', '>', '<', '`', '$', '\n', '\r'];
    if trimmed.contains('\'') || trimmed.contains('"') || trimmed.chars().any(|c| forbidden.contains(&c)) {
        return Err(format!("unsupported characters in command: {command}"));
    }

    let tokens = trimmed
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<String>>();
    if tokens.len() < 2 {
        return Err(format!("invalid git command: {command}"));
    }
    Ok(tokens)
}
