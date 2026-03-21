//! Debug instrumentation: append NDJSON lines to session log for hypothesis testing.
//! Log path: .cursor/debug-f03357.log (session f03357)

const LOG_PATH: &str = "/Users/joe/git/daemon/.cursor/debug-f03357.log";

pub fn log(location: &str, message: &str, data: &str, hypothesis_id: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!(
        r#"{{"sessionId":"f03357","location":"{}","message":"{}","data":{},"timestamp":{},"hypothesisId":"{}"}}"#,
        location, message, data, ts, hypothesis_id
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(LOG_PATH) {
        use std::io::Write;
        let _ = writeln!(f, "{}", line);
        let _ = f.flush();
    }
}
