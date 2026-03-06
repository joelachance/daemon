use crate::daemon;
use crate::git;
use crate::store;

pub fn run_status_ui() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let root = git::repo_root()?;
    let sessions = store::list_sessions_for_repo(&root)?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        println!("{}", session.id);
        let drafts = store::list_drafts(&session.id)?;
        for draft in drafts {
            println!("  - {} ({})", draft.id, draft.message);
        }
    }
    Ok(())
}
