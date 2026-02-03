use std::sync::{Mutex, OnceLock};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
struct Status {
    balance: String,
    tick: String,
    reveal_ratio: String,
    tick_value: Option<u32>,
    reveal_success: u64,
    reveal_failed: u64,
    reveal_empty: u64,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            balance: String::new(),
            tick: String::new(),
            reveal_ratio: "n/a".to_string(),
            tick_value: None,
            reveal_success: 0,
            reveal_failed: 0,
            reveal_empty: 0,
        }
    }
}

static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();

#[cfg(test)]
static TEST_REVEAL_SUCCESS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static TEST_REVEAL_FAILED: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static TEST_REVEAL_EMPTY: AtomicU64 = AtomicU64::new(0);

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

pub fn set_tick_value(tick: u32) {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.tick = tick.to_string();
        status.tick_value = Some(tick);
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
    if value.len() <= 6 {
        return value.to_string();
    }

    let head = &value[..6];
    head.to_string()
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
    let level = colorize_level(level);
    format!(
        "[{level}] tick={} balance={} reveal={} | {message}",
        status.tick, status.balance, status.reveal_ratio
    )
}

pub fn record_reveal_result(success: bool) {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        if success {
            status.reveal_success = status.reveal_success.saturating_add(1);
        } else {
            status.reveal_failed = status.reveal_failed.saturating_add(1);
        }
        status.reveal_ratio = format_reveal_ratio(
            status.reveal_success,
            status.reveal_failed,
            status.reveal_empty,
        );
    }

    #[cfg(test)]
    {
        if success {
            TEST_REVEAL_SUCCESS.fetch_add(1, Ordering::Relaxed);
        } else {
            TEST_REVEAL_FAILED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn record_reveal_empty() {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.reveal_empty = status.reveal_empty.saturating_add(1);
        if status.reveal_success > 0 {
            status.reveal_success = status.reveal_success.saturating_sub(1);
        }
        status.reveal_ratio = format_reveal_ratio(
            status.reveal_success,
            status.reveal_failed,
            status.reveal_empty,
        );
    }

    #[cfg(test)]
    {
        TEST_REVEAL_EMPTY.fetch_add(1, Ordering::Relaxed);
        let _ = TEST_REVEAL_SUCCESS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_sub(1))
        });
    }
}

fn format_reveal_ratio(success: u64, failed: u64, empty: u64) -> String {
    let total = success.saturating_add(failed).saturating_add(empty);
    if total == 0 {
        return "n/a".to_string();
    }

    let percent = (success as f64) * 100.0 / (total as f64);
    format!("{percent:.1}% (ok={success} fail={failed} empty={empty})")
}

fn colorize_level(level: &str) -> String {
    const COLOR_GREEN: &str = "\x1b[32m";
    const COLOR_YELLOW: &str = "\x1b[33m";
    const COLOR_RED: &str = "\x1b[31m";
    const COLOR_RESET: &str = "\x1b[0m";

    if level.eq_ignore_ascii_case("INFO") {
        return format!("{COLOR_GREEN}{level}{COLOR_RESET}");
    }

    if level.eq_ignore_ascii_case("WARN") || level.eq_ignore_ascii_case("WARNING") {
        return format!("{COLOR_YELLOW}{level}{COLOR_RESET}");
    }

    if level.eq_ignore_ascii_case("ERROR") {
        return format!("{COLOR_RED}{level}{COLOR_RESET}");
    }

    level.to_string()
}

#[cfg(test)]
pub fn reveal_counts() -> (u64, u64, u64) {
    (
        TEST_REVEAL_SUCCESS.load(Ordering::Relaxed),
        TEST_REVEAL_FAILED.load(Ordering::Relaxed),
        TEST_REVEAL_EMPTY.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
pub fn reset_reveal_stats() {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.reveal_success = 0;
        status.reveal_failed = 0;
        status.reveal_empty = 0;
        status.reveal_ratio = "n/a".to_string();
    }

    TEST_REVEAL_SUCCESS.store(0, Ordering::Relaxed);
    TEST_REVEAL_FAILED.store(0, Ordering::Relaxed);
    TEST_REVEAL_EMPTY.store(0, Ordering::Relaxed);
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
        assert_eq!(shorten_id("abcdefghijklmnopqrstuvwxyz"), "abcdef");
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
            reveal_ratio: "r".to_string(),
            tick_value: None,
            reveal_success: 0,
            reveal_failed: 0,
            reveal_empty: 0,
        };
        let line = format_log_line("INFO", &status, "hello");
        assert_eq!(
            line,
            "[\u{1b}[32mINFO\u{1b}[0m] tick=t balance=b reveal=r | hello"
        );
    }

    #[test]
    fn set_lines_update_status_snapshot() {
        // set_tick_value / set_balance_line update shared status.
        super::init();
        super::set_tick_value(123);
        super::set_balance_line("456");
        let status = STATUS
            .get()
            .and_then(|status| status.lock().ok())
            .map(|status| status.clone())
            .unwrap_or_default();
        assert_eq!(status.tick, "123");
        assert_eq!(status.balance, "456");
    }

    #[test]
    fn reveal_ratio_formats_counts() {
        super::init();
        super::reset_reveal_stats();
        super::record_reveal_result(true);
        super::record_reveal_result(true);
        super::record_reveal_result(false);
        super::record_reveal_empty();

        let status = STATUS
            .get()
            .and_then(|status| status.lock().ok())
            .map(|status| status.clone())
            .unwrap_or_default();
        assert_eq!(status.reveal_ratio, "33.3% (ok=1 fail=1 empty=1)");
    }

    #[test]
    fn reveal_empty_decrements_success_once() {
        super::init();
        super::reset_reveal_stats();
        super::record_reveal_result(true);
        super::record_reveal_empty();
        let (ok, fail, empty) = super::reveal_counts();
        assert_eq!((ok, fail, empty), (0, 0, 1));
    }
}
