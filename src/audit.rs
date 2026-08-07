//! Append-only audit log for forensics.
//!
//! Unlike the normal `log` output (which goes to stdout and is lost when the
//! process ends), this writes durable, timestamped, one-line-per-event records
//! to `events.log` next to `state.json`. It exists to answer, after the fact,
//! questions like *"why did the bot re-send 19 listings at 18:00 with no command
//! from the user?"* — by recording every command received, every listing ID
//! sent (with the source watch), and every mutation of the seen-history set.
//!
//! Format (tab-separated, easy to grep):
//!   <rfc3339-utc>\t<KIND>\t<message>

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static AUDIT: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// Open (create/append) the audit log. Safe to call once at startup; if it
/// fails, events are silently mirrored only to the normal log.
pub fn init(path: PathBuf) {
    let file = OpenOptions::new().create(true).append(true).open(&path);
    match file {
        Ok(f) => {
            let _ = AUDIT.set(Mutex::new(Some(f)));
            log::info!("audit log: {}", path.display());
        }
        Err(e) => {
            log::warn!("could not open audit log {}: {e}", path.display());
            let _ = AUDIT.set(Mutex::new(None));
        }
    }
}

/// Record one event. Mirrored to the normal log and appended (flushed) to the
/// audit file. Never panics; I/O errors are dropped.
pub fn event(kind: &str, msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    log::info!("[audit] {kind} {msg}");
    let line = format!("{}\t{}\t{}\n", chrono::Utc::now().to_rfc3339(), kind, msg);
    if let Some(lock) = AUDIT.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }
}
