use crate::daemon;
use crate::status;
use crate::store;
use std::io::{self, Write};

const COMMANDS: &[&str] = &["start", "stop", "status", "ticket", "install-model"];

pub fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

pub fn is_command(arg: &str) -> bool {
    COMMANDS.iter().any(|cmd| cmd.eq_ignore_ascii_case(arg))
}

pub fn run_command(command: &str, _args: &[String]) -> Result<(), String> {
    match command {
        "start" => run_start_command(),
        "stop" => run_stop_command(),
        "status" => run_status_command(),
        "ticket" => run_ticket_command(_args),
        "install-model" => run_install_model_command(),
        _ => Err(format!("unsupported command: {command}")),
    }
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
        "vibe - Vibe Commit Daemon\n\
\n\
Usage:\n\
  gg\n\
  gg start\n\
  gg stop\n\
  gg status\n\
  gg ticket <session-id> <ticket>\n\
  gg install-model\n\
  gg -h | --help\n\
\n\
Behavior:\n\
  - start daemon + proposal dashboard\n\
  - stop stops daemon process\n\
  - status opens draft review view\n\
  - ticket updates ticket for session\n\
  - install-model downloads default GGUF model for commit inference\n\
\n\
Examples:\n\
  gg\n\
  gg start\n\
  gg stop\n\
  gg status\n\
  gg ticket ses_123 456\n\
  gg install-model\n\
"
    );
}

fn run_start_command() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    crate::dashboard::run_dashboard()
}

fn run_status_command() -> Result<(), String> {
    status::run_status_ui()
}

fn run_stop_command() -> Result<(), String> {
    daemon::stop_daemon()?;
    println!("daemon stopped");
    Ok(())
}

fn run_install_model_command() -> Result<(), String> {
    #[cfg(feature = "llama-embedded")]
    {
        let path = crate::model::ensure_default_model()?;
        println!("Default model ready at {}", path.display());
        Ok(())
    }
    #[cfg(not(feature = "llama-embedded"))]
    {
        println!("Embedded model is disabled. Use Ollama instead:");
        println!("  1. Install from https://ollama.com");
        println!("  2. Run: ollama pull llama3.2");
        Ok(())
    }
}

fn run_ticket_command(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: gg ticket <session-id> <ticket>".to_string());
    }
    let session_id = &args[0];
    let ticket = &args[1];
    store::set_session_ticket(session_id, Some(ticket))?;
    println!("ticket set for {session_id}");
    Ok(())
}
