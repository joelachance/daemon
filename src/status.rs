use crate::daemon;
use crate::git;
use crate::session_row;
use crate::store;

pub fn run_status_ui() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let root = git::repo_root()?;
    let sessions = store::list_sessions_for_repo(&root)?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(120);
    for session in sessions {
        println!(
            "{}",
            session_row::format_session_columns(&session, width, None)
        );
        let drafts = store::list_drafts(&session.id)?;
        for draft in drafts {
            println!("  - {} ({})", draft.id, draft.message);
        }
    }
    Ok(())
}
