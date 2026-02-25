use std::env;
use std::process;

mod cli;
mod daemon;
mod git;
mod session;

fn main() {
    let mut args = env::args().skip(1);
    let first = match args.next() {
        Some(value) => value,
        None => {
            cli::print_help();
            return;
        }
    };

    if first == "--daemon" || env::var("GG_DAEMON").ok().as_deref() == Some("1") {
        if let Err(err) = daemon::run_daemon() {
            eprintln!("gg daemon: {err}");
            process::exit(1);
        }
        return;
    }

    if cli::is_help(&first) {
        cli::print_help();
        return;
    }

    if cli::is_tool(&first) {
        let rest: Vec<String> = args.collect();
        if let Err(err) = daemon::run_tool(&first, &rest) {
            eprintln!("gg: {err}");
            process::exit(1);
        }
        return;
    }

    if cli::is_command(&first) {
        let rest: Vec<String> = args.collect();
        if let Err(err) = cli::run_command(&first, &rest) {
            eprintln!("gg: {err}");
            process::exit(1);
        }
        return;
    }

    cli::print_help_with_error(&format!("unknown command or tool: {first}"));
}
