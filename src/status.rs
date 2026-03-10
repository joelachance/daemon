use crate::daemon;
use crate::session_row;
use crate::store;
use time::OffsetDateTime;

pub fn run_status_ui() -> Result<(), String> {
    daemon::ensure_daemon_running()?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let window_secs = active_window_secs();
    let sessions = store::list_active_sessions(now, window_secs)?;
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
            println!("  - {}", draft.message);
        }
    }
    Ok(())
}

fn active_window_secs() -> i64 {
    std::env::var("GG_ACTIVE_WINDOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(900)
}
