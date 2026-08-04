use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default)]
struct Status {
    backend: String,
    epoch: Option<u32>,
    tick: Option<u32>,
    send_stats: Option<(u64, u64, u64)>,
}

static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();

pub fn init() {
    let _ = STATUS.set(Mutex::new(Status::default()));
}

pub fn set_backend(value: impl Into<String>) {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.backend = display_backend(&value.into());
    }
}

pub fn set_tick_value(epoch: u32, tick: u32) {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.epoch = Some(epoch);
        status.tick = Some(tick);
    }
}

pub fn set_send_stats(ok: u64, failed: u64, empty: u64) {
    if let Some(status) = STATUS.get()
        && let Ok(mut status) = status.lock()
    {
        status.send_stats = Some((ok, failed, empty));
    }
}

pub fn log_info(message: impl Into<String>) {
    log_with_level("INFO", message.into());
}

pub fn log_warn(message: impl Into<String>) {
    log_with_level("WARN", message.into());
}

pub fn shorten_id(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(6)
        .collect()
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
    let mut context = Vec::with_capacity(3);
    if !status.backend.is_empty() {
        context.push(status.backend.clone());
    }
    if let (Some(epoch), Some(tick)) = (status.epoch, status.tick) {
        context.push(format!("Epoch {epoch}, tick {tick}"));
    }
    if let Some((ok, failed, empty)) = status.send_stats {
        context.push(format!("Sends: {}", format_send_stats(ok, failed, empty)));
    }

    if context.is_empty() {
        format!("[{level}] {message}")
    } else {
        format!("[{level}] {message} | {}", context.join(" | "))
    }
}

fn format_send_stats(ok: u64, failed: u64, empty: u64) -> String {
    let total = ok.saturating_add(failed).saturating_add(empty);
    if total == 0 {
        return "0 ok (0.0%), 0 failed (0.0%), 0 empty (0.0%)".to_string();
    }
    let total = total as f64;
    let ok_percent = ok as f64 * 100.0 / total;
    let failed_percent = failed as f64 * 100.0 / total;
    let empty_percent = empty as f64 * 100.0 / total;
    format!(
        "{ok} ok ({ok_percent:.1}%), {failed} failed ({failed_percent:.1}%), {empty} empty ({empty_percent:.1}%)"
    )
}

fn display_backend(value: &str) -> String {
    if value.eq_ignore_ascii_case("rpc") {
        "RPC".to_string()
    } else if value.eq_ignore_ascii_case("bob") {
        "Bob".to_string()
    } else if value.eq_ignore_ascii_case("grpc") {
        "gRPC".to_string()
    } else {
        value.to_string()
    }
}

fn colorize_level(level: &str) -> String {
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RED: &str = "\x1b[31m";
    const RESET: &str = "\x1b[0m";

    if level.eq_ignore_ascii_case("INFO") {
        format!("{GREEN}{level}{RESET}")
    } else if level.eq_ignore_ascii_case("WARN") || level.eq_ignore_ascii_case("WARNING") {
        format!("{YELLOW}{level}{RESET}")
    } else if level.eq_ignore_ascii_case("ERROR") {
        format!("{RED}{level}{RESET}")
    } else {
        level.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_contains_small_runtime_context() {
        let status = Status {
            backend: "RPC".to_string(),
            epoch: Some(7),
            tick: Some(123),
            send_stats: None,
        };
        assert_eq!(
            format_log_line("INFO", &status, "hello"),
            "[\u{1b}[32mINFO\u{1b}[0m] hello | RPC | Epoch 7, tick 123"
        );
    }

    #[test]
    fn log_line_contains_send_counters_and_percentages() {
        let status = Status {
            backend: "RPC".to_string(),
            epoch: Some(7),
            tick: Some(123),
            send_stats: Some((7, 2, 1)),
        };
        assert_eq!(
            format_log_line("INFO", &status, "hello"),
            "[\u{1b}[32mINFO\u{1b}[0m] hello | RPC | Epoch 7, tick 123 | Sends: 7 ok (70.0%), 2 failed (20.0%), 1 empty (10.0%)"
        );
    }

    #[test]
    fn transaction_id_is_sanitized_and_shortened() {
        assert_eq!(shorten_id("abc\n123456"), "abc123");
    }
}
