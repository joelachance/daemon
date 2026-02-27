use std::env;
use std::process;

mod claude;
mod bedrock;
mod cli;
mod cursor;
mod daemon;
mod git;
mod opencode;
mod session;
mod status;

fn main() {
    let mut args = env::args().skip(1);
    let first = match args.next() {
        Some(value) => value,
        None => {
            cli::print_banner();
            if let Err(err) = daemon::run_daemon() {
                eprintln!("gg daemon: {err}");
                process::exit(1);
            }
            return;
        }
    };

    if cli::is_help(&first) {
        cli::print_help();
        return;
    }

    if cli::is_command(&first) {
        let rest: Vec<String> = args.collect();
        if let Err(err) = cli::run_command(&first, &rest) {
            eprintln!("error: {err}");
            process::exit(1);
        }
        return;
    }

    let mut prompt_parts = Vec::new();
    prompt_parts.push(first);
    prompt_parts.extend(args);
    let prompt = prompt_parts.join(" ");
    if prompt.trim().is_empty() {
        cli::print_help();
        return;
    }
    if let Err(err) = cli::run_prompt(&prompt) {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
