use std::env;
use std::process;

mod api;
mod claude;
mod cli;
mod cursor;
mod daemon;
mod dashboard;
mod git;
mod grouping;
mod opencode;
mod session;
mod session_row;
mod status;
mod store;

fn main() {
    let mut args = env::args().skip(1);
    let first = match args.next() {
        Some(value) => value,
        None => {
            if env::var("GG_DAEMON").ok().as_deref() == Some("1") {
                let _ = std::thread::spawn(|| {
                    let _ = api::run_api_server();
                });
                if let Err(err) = daemon::run_daemon(false) {
                    eprintln!("gg daemon: {err}");
                    process::exit(1);
                }
                return;
            }

            cli::print_banner();

            if let Err(err) = daemon::ensure_daemon_running() {
                eprintln!("gg daemon: {err}");
                process::exit(1);
            }

            if let Err(err) = dashboard::run_dashboard() {
                eprintln!("gg dashboard: {err}");
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

    cli::print_help();
}
