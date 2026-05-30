//! Sleep-window helpers: parse `HH:MM-HH:MM` strings and check whether the
//! current local time falls inside the window (with midnight wrap-around).

use chrono::NaiveTime;

/// Parse a sleep window. Accepts:
///   - `HH:MM-HH:MM` (e.g. `23:00-09:00`)
///   - `H-H` or `HH-HH` (hour-only, e.g. `23-9`, padded to `:00`)
///   - tolerates `HH::MM` typo
pub fn parse(s: &str) -> Result<(NaiveTime, NaiveTime), String> {
    let cleaned = s.trim().replace("::", ":");
    let mut parts = cleaned.splitn(2, '-');
    let a = parts.next().unwrap_or("").trim();
    let b = parts
        .next()
        .ok_or_else(|| "expected HH:MM-HH:MM or H-H".to_string())?
        .trim();

    let start = parse_time_part(a).map_err(|e| format!("bad start time '{a}': {e}"))?;
    let end = parse_time_part(b).map_err(|e| format!("bad end time '{b}': {e}"))?;
    Ok((start, end))
}

fn parse_time_part(s: &str) -> Result<NaiveTime, String> {
    // Full HH:MM.
    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        return Ok(t);
    }
    // Hour only — pad to "<h>:00".
    if s.chars().all(|c| c.is_ascii_digit()) && (s.len() == 1 || s.len() == 2) {
        let padded = format!("{s}:00");
        if let Ok(t) = NaiveTime::parse_from_str(&padded, "%H:%M") {
            return Ok(t);
        }
    }
    Err(format!("not a valid time"))
}

/// Returns `Some("HH:MM-HH:MM")` if the input parses, normalising to two-digit
/// hours/minutes.
pub fn canonical(s: &str) -> Result<String, String> {
    let (start, end) = parse(s)?;
    Ok(format!(
        "{}-{}",
        start.format("%H:%M"),
        end.format("%H:%M")
    ))
}

pub fn is_in_window(start: NaiveTime, end: NaiveTime, now: NaiveTime) -> bool {
    if start <= end {
        now >= start && now < end
    } else {
        // Wraps midnight (e.g. 23:00–09:00).
        now >= start || now < end
    }
}

/// Convenience: check whether the chat's configured sleep window (if any)
/// covers the current local time.
pub fn is_sleeping(sleep: Option<&str>) -> bool {
    let Some(s) = sleep else { return false };
    let Ok((start, end)) = parse(s) else { return false };
    let now = chrono::Local::now().time();
    is_in_window(start, end, now)
}
