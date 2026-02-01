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
    if let Some(status) = STATUS.get() {
        if let Ok(mut status) = status.lock() {
            status.balance = line;
        }
    }
}

pub fn set_tick_line(line: impl Into<String>) {
    let line = line.into();
    if let Some(status) = STATUS.get() {
        if let Ok(mut status) = status.lock() {
            status.tick = line;
        }
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

    println!(
        "[{level}] tick={} balance={} | {message}",
        status.tick, status.balance
    );
}
