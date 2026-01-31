use std::collections::VecDeque;
use std::io::{Write, stdout};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::style::{Color, Stylize};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use tokio::sync::mpsc;

const HEADER_LINES: u16 = 2;
const COMMAND_HINT: &str = "Commands: Ctrl+C to exit";

#[derive(Clone, Copy)]
enum LogLevel {
    Info,
    Warn,
    Error,
}

enum ConsoleEvent {
    SetBalance(String),
    SetTick(String),
    Log(LogLevel, String),
    Shutdown,
}

#[derive(Clone)]
pub struct ConsoleHandle {
    tx: mpsc::Sender<ConsoleEvent>,
}

static CONSOLE: OnceLock<ConsoleHandle> = OnceLock::new();

pub fn init() {
    if CONSOLE.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel(256);
    let handle = ConsoleHandle { tx };
    let _ = CONSOLE.set(handle);
    tokio::spawn(run_renderer(rx));
    set_balance_line("waiting for balance");
    set_tick_line("waiting for tick");
}

pub fn set_balance_line(line: impl Into<String>) {
    emit(ConsoleEvent::SetBalance(line.into()));
}

pub fn set_tick_line(line: impl Into<String>) {
    emit(ConsoleEvent::SetTick(line.into()));
}

pub fn log_info(message: impl Into<String>) {
    emit(ConsoleEvent::Log(LogLevel::Info, message.into()));
}

pub fn log_warn(message: impl Into<String>) {
    emit(ConsoleEvent::Log(LogLevel::Warn, message.into()));
}

fn emit(event: ConsoleEvent) {
    if let Some(handle) = CONSOLE.get() {
        let _ = handle.tx.try_send(event);
    }
}

struct RenderState {
    balance: String,
    tick: String,
    logs: VecDeque<LogEntry>,
    width: u16,
    height: u16,
}

impl RenderState {
    fn new() -> Self {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        Self {
            balance: String::new(),
            tick: String::new(),
            logs: VecDeque::new(),
            width,
            height,
        }
    }

    fn refresh_size(&mut self) -> bool {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        if self.width == width && self.height == height {
            return false;
        }
        self.width = width;
        self.height = height;
        true
    }

    fn log_start(&self) -> u16 {
        HEADER_LINES.min(self.height.saturating_sub(1))
    }

    fn command_row(&self) -> u16 {
        self.height.saturating_sub(1)
    }

    fn bottom_border_row(&self) -> u16 {
        self.command_row().saturating_sub(1)
    }

    fn log_bottom(&self) -> u16 {
        self.bottom_border_row().saturating_sub(1)
    }
}

async fn run_renderer(mut rx: mpsc::Receiver<ConsoleEvent>) {
    let mut out = stdout();
    let mut state = RenderState::new();
    let _ = execute!(out, Clear(ClearType::All), Hide);
    render_all(&mut out, &state);

    while let Some(event) = rx.recv().await {
        if state.refresh_size() {
            let _ = execute!(out, Clear(ClearType::All));
            render_all(&mut out, &state);
        }
        match event {
            ConsoleEvent::SetBalance(line) => {
                state.balance = line;
                render_status(&mut out, &state);
            }
            ConsoleEvent::SetTick(line) => {
                state.tick = line;
                render_status(&mut out, &state);
            }
            ConsoleEvent::Log(level, message) => {
                render_log(&mut out, &mut state, level, &message);
            }
            ConsoleEvent::Shutdown => {
                break;
            }
        }
    }

    let _ = execute!(out, Show, MoveTo(0, state.command_row()));
}

fn render_header(out: &mut impl Write, state: &RenderState) {
    render_status(out, state);
    render_top_border(out, state);
    let _ = execute!(out, MoveTo(0, state.log_start()));
}

fn render_all(out: &mut impl Write, state: &RenderState) {
    render_header(out, state);
    let start = state.log_start();
    let bottom = state.log_bottom();
    if bottom >= start {
        let capacity = bottom.saturating_sub(start).saturating_add(1) as usize;
        render_logs(out, state, start, capacity);
    }
    render_bottom_border(out, state);
    render_command_panel(out, state);
}

fn render_status(out: &mut impl Write, state: &RenderState) {
    let _ = queue!(out, MoveTo(0, 0), Clear(ClearType::CurrentLine));
    let balance_label = "Balance:";
    let tick_label = "Tick:";
    let separator = " | ";
    let full_line = format!(
        "{balance_label} {}{separator}{tick_label} {}",
        state.balance, state.tick
    );
    let line = truncate_to_width(&full_line, state.width);
    if line.len() < full_line.len() {
        let _ = write!(out, "{}", line.with(Color::Cyan));
        let _ = out.flush();
        return;
    }
    let _ = write!(
        out,
        "{}{}{}",
        format!("{balance_label} {}", state.balance).with(Color::Cyan),
        separator.with(Color::DarkGrey),
        format!("{tick_label} {}", state.tick).with(Color::Green)
    );
    let _ = out.flush();
}

fn render_top_border(out: &mut impl Write, state: &RenderState) {
    let row = state.log_start().saturating_sub(1);
    let _ = queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine));
    let width = state.width.max(1) as usize;
    if width < 2 {
        let _ = out.flush();
        return;
    }
    let inner = width - 2;
    let label = " LOGS ";
    let label_start = if inner > label.len() {
        (inner - label.len()) / 2
    } else {
        0
    };
    let label_end = (label_start + label.len()).min(inner);

    let _ = write!(out, "+");
    if label_start > 0 {
        let _ = write!(out, "{}", "-".repeat(label_start).with(Color::DarkGrey));
    }
    if label_end > label_start {
        let label_slice = &label[..label_end - label_start];
        let _ = write!(out, "{}", label_slice.with(Color::Cyan));
    }
    if inner > label_end {
        let _ = write!(
            out,
            "{}",
            "-".repeat(inner - label_end).with(Color::DarkGrey)
        );
    }
    let _ = write!(out, "+");
    let _ = out.flush();
}

fn render_log(out: &mut impl Write, state: &mut RenderState, level: LogLevel, message: &str) {
    let start = state.log_start();
    let bottom = state.log_bottom();
    let capacity = bottom.saturating_sub(start).saturating_add(1) as usize;
    if capacity == 0 {
        return;
    }

    state
        .logs
        .push_back(LogEntry::new(level, message.to_string()));
    while state.logs.len() > capacity {
        state.logs.pop_front();
    }
    render_logs(out, state, start, capacity);
}

struct LogEntry {
    level: LogLevel,
    message: String,
    timestamp: SystemTime,
}

impl LogEntry {
    fn new(level: LogLevel, message: String) -> Self {
        Self {
            level,
            message,
            timestamp: SystemTime::now(),
        }
    }
}

fn render_logs(out: &mut impl Write, state: &RenderState, start: u16, capacity: usize) {
    for index in 0..capacity {
        let row = start.saturating_add(index as u16);
        let _ = queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine));
        render_log_row(out, state, index);
    }
    let _ = out.flush();
}

fn render_log_row(out: &mut impl Write, state: &RenderState, index: usize) {
    let width = state.width as usize;
    if width < 2 {
        return;
    }
    let inner = width - 2;
    let _ = write!(out, "|");
    if let Some(entry) = state.logs.get(index) {
        let (tag, color) = match entry.level {
            LogLevel::Info => ("INFO", Color::White),
            LogLevel::Warn => ("WARN", Color::Yellow),
            LogLevel::Error => ("ERROR", Color::Red),
        };
        let timestamp = format_timestamp(entry.timestamp);
        let prefix = format!("[{}] [{}] ", timestamp, tag);
        let available = inner.saturating_sub(prefix.len());
        let message = truncate_message(&entry.message, available);
        let padding = inner.saturating_sub(prefix.len() + message.len());
        let message_color = log_message_color(&entry.message);
        let _ = write!(
            out,
            "[{}] [{}] ",
            timestamp.with(Color::DarkGrey),
            tag.with(color),
        );
        if let Some(color) = message_color {
            let _ = write!(out, "{}", message.with(color));
        } else {
            let _ = write!(out, "{}", message);
        }
        if padding > 0 {
            let _ = write!(out, "{}", " ".repeat(padding));
        }
    } else {
        let _ = write!(out, "{}", " ".repeat(inner));
    }
    let _ = write!(out, "|");
}

fn render_bottom_border(out: &mut impl Write, state: &RenderState) {
    let row = state.bottom_border_row();
    let _ = queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine));
    let width = state.width.max(1) as usize;
    if width < 2 {
        let _ = out.flush();
        return;
    }
    let inner = width - 2;
    let _ = write!(out, "+{}+", "-".repeat(inner).with(Color::DarkGrey));
    let _ = out.flush();
}

fn render_command_panel(out: &mut impl Write, state: &RenderState) {
    let row = state.command_row();
    let _ = queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine));
    let line = truncate_to_width(COMMAND_HINT, state.width);
    let _ = write!(out, "{}", line.with(Color::DarkGrey));
    let _ = out.flush();
}

fn truncate_to_width(value: &str, width: u16) -> String {
    let max = width as usize;
    if max == 0 || value.len() <= max {
        return value.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out = String::with_capacity(max);
    out.push_str(&value.chars().take(keep).collect::<String>());
    out.push_str("...");
    out
}

fn truncate_message(value: &str, max: usize) -> String {
    if max == 0 || value.len() <= max {
        return value.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out = String::with_capacity(max);
    out.push_str(&value.chars().take(keep).collect::<String>());
    out.push_str("...");
    out
}

fn log_message_color(message: &str) -> Option<Color> {
    if message.starts_with("pipeline") {
        return Some(Color::Cyan);
    }
    if message.starts_with("scapi") {
        return Some(Color::Magenta);
    }
    None
}

fn format_timestamp(time: SystemTime) -> String {
    let dur = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = dur.as_secs();
    let secs = (total_secs % 60) as u32;
    let mins = ((total_secs / 60) % 60) as u32;
    let hours = ((total_secs / 3600) % 24) as u32;
    let days = total_secs / 86_400;

    let (year, month, day) = date_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, mins, secs
    )
}

fn date_from_days(mut days: u64) -> (u32, u32, u32) {
    let mut year: u32 = 1970;
    loop {
        let leap = is_leap_year(year);
        let year_days = if leap { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }

    let month_days = if is_leap_year(year) {
        [31u32, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u32, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month: u32 = 1;
    let mut remaining = days as u32;
    for md in month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        month += 1;
    }

    let day = remaining + 1;
    (year, month, day)
}

fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
