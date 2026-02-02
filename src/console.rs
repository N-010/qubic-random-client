use std::sync::{Mutex, OnceLock};

#[derive(Default, Clone)]
struct Status {
    balance: String,
    tick: String,
}

static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();

pub fn init() {
    let _ = STATUS.set(Mutex::new(Status::default()));
    log_info("console initialized");
}

pub fn set_balance_line(line: impl Into<String>) {
    let line = line.into();
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.balance = line;
    }
}

pub fn set_tick_line(line: impl Into<String>) {
    let line = line.into();
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.tick = line;
    }
}

pub fn log_info(message: impl Into<String>) {
    log_with_level("INFO", message.into());
}

pub fn log_warn(message: impl Into<String>) {
    log_with_level("WARN", message.into());
}

pub async fn shutdown() {}

pub fn shorten_id(value: &str) -> String {
    if value.len() <= 12 {
        return value.to_string();
    }

    let head = &value[..6];
    let tail = &value[value.len() - 6..];
    format!("{head}...{tail}")
}

pub fn format_amount(amount: u64) -> String {
    let value = amount.to_string();
    let mut out = String::with_capacity(value.len() + value.len() / 3);
    let mut count = 0;

    for ch in value.chars().rev() {
        if count == 3 {
            out.push('.');
            count = 0;
        }
        out.push(ch);
        count += 1;
    }

    out.chars().rev().collect()
}

fn log_with_level(level: &str, message: String) {
    let status = STATUS
        .get()
        .and_then(|status| status.lock().ok())
        .map(|status| status.clone())
        .unwrap_or_default();

    println!("{}", format_log_line(level, &status, &message));
}

fn format_log_line(level: &str, status: &Status, message: &str) -> String {
    format!(
        "[{level}] tick={} balance={} | {message}",
        status.tick, status.balance
    )
}

#[cfg(test)]
mod tests {
    use super::{STATUS, Status, format_amount, format_log_line, shorten_id};

    #[test]
    fn shorten_id_keeps_short_values() {
        // Short IDs are returned unchanged.
        assert_eq!(shorten_id("short"), "short");
    }

    #[test]
    fn shorten_id_truncates_long_values() {
        // Long IDs are shortened to head...tail.
        assert_eq!(shorten_id("abcdefghijklmnopqrstuvwxyz"), "abcdef...uvwxyz");
    }

    #[test]
    fn format_amount_inserts_separators() {
        // Thousands are grouped with '.' separators.
        assert_eq!(format_amount(0), "0");
        assert_eq!(format_amount(1_000), "1.000");
        assert_eq!(format_amount(12_345_678), "12.345.678");
    }

    #[test]
    fn format_log_line_includes_status() {
        // Log line format includes tick and balance status.
        let status = Status {
            balance: "b".to_string(),
            tick: "t".to_string(),
        };
        let line = format_log_line("INFO", &status, "hello");
        assert_eq!(line, "[INFO] tick=t balance=b | hello");
    }

    #[test]
    fn set_lines_update_status_snapshot() {
        // set_tick_line / set_balance_line update shared status.
        super::init();
        super::set_tick_line("123");
        super::set_balance_line("456");
        let status = STATUS
            .get()
            .and_then(|status| status.lock().ok())
            .map(|status| status.clone())
            .unwrap_or_default();
        assert_eq!(status.tick, "123");
        assert_eq!(status.balance, "456");
    }
}
