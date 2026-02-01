use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crossterm::cursor;
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;

const LOG_CAPACITY: usize = 500;
const REFRESH_INTERVAL_MS: u64 = 250;

#[derive(Clone, Copy)]
enum LogLevel {
    Info,
    Warn,
}

struct LogEntry {
    level: LogLevel,
    message: String,
}

struct ConsoleState {
    balance: String,
    tick: String,
    logs: VecDeque<LogEntry>,
}

impl ConsoleState {
    fn new() -> Self {
        Self {
            balance: String::new(),
            tick: String::new(),
            logs: VecDeque::with_capacity(LOG_CAPACITY),
        }
    }
}

enum ConsoleEvent {
    SetBalance(String),
    SetTick(String),
    Log(LogLevel, String),
    Shutdown,
}

struct ConsoleRuntime {
    tx: mpsc::Sender<ConsoleEvent>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

static CONSOLE: OnceLock<ConsoleRuntime> = OnceLock::new();

pub fn init() {
    if CONSOLE.get().is_some() {
        return;
    }

    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || run_console(rx));
    let _ = CONSOLE.set(ConsoleRuntime {
        tx: tx.clone(),
        join: Mutex::new(Some(join)),
    });

    log_info("console initialized");
}

pub fn set_balance_line(line: impl Into<String>) {
    let _ = send_event(ConsoleEvent::SetBalance(line.into()));
}

pub fn set_tick_line(line: impl Into<String>) {
    let _ = send_event(ConsoleEvent::SetTick(line.into()));
}

pub fn log_info(message: impl Into<String>) {
    let message = message.into();
    if send_event(ConsoleEvent::Log(LogLevel::Info, message.clone())).is_err() {
        println!("[INFO] {}", message);
    }
}

pub fn log_warn(message: impl Into<String>) {
    let message = message.into();
    if send_event(ConsoleEvent::Log(LogLevel::Warn, message.clone())).is_err() {
        println!("[WARN] {}", message);
    }
}

pub async fn shutdown() {
    if let Some(runtime) = CONSOLE.get() {
        let _ = runtime.tx.send(ConsoleEvent::Shutdown);
        if let Ok(mut join) = runtime.join.lock() {
            if let Some(handle) = join.take() {
                let _ = handle.join();
            }
        }
    }
}

fn send_event(event: ConsoleEvent) -> Result<(), ()> {
    if let Some(runtime) = CONSOLE.get() {
        runtime.tx.send(event).map_err(|_| ())
    } else {
        Err(())
    }
}

fn run_console(rx: mpsc::Receiver<ConsoleEvent>) {
    let mut state = ConsoleState::new();
    let mut terminal = match setup_terminal() {
        Ok(terminal) => terminal,
        Err(_) => return,
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(REFRESH_INTERVAL_MS)) {
            Ok(event) => {
                if handle_event(event, &mut state) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if terminal.draw(|frame| draw_ui(frame, &state)).is_err() {
            break;
        }
    }

    restore_terminal(&mut terminal);
}

fn handle_event(event: ConsoleEvent, state: &mut ConsoleState) -> bool {
    match event {
        ConsoleEvent::SetBalance(line) => {
            state.balance = line;
        }
        ConsoleEvent::SetTick(line) => {
            state.tick = line;
        }
        ConsoleEvent::Log(level, message) => push_log(state, level, message),
        ConsoleEvent::Shutdown => return true,
    }

    false
}

fn push_log(state: &mut ConsoleState, level: LogLevel, message: String) {
    if state.logs.len() >= LOG_CAPACITY {
        state.logs.pop_front();
    }
    state.logs.push_back(LogEntry { level, message });
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, state: &ConsoleState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(frame.area());

    let header = Paragraph::new(header_line(state));
    frame.render_widget(header, layout[0]);

    let log_lines = log_lines(state, layout[1].height.saturating_sub(2) as usize);
    let log = Paragraph::new(Text::from(log_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Log")
                .title_alignment(ratatui::layout::Alignment::Left),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(log, layout[1]);
}

fn header_line(state: &ConsoleState) -> Line<'static> {
    let label_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    Line::from(vec![
        Span::styled("Tick: ", label_style),
        Span::raw(state.tick.clone()),
        Span::raw(" | "),
        Span::styled("Balance: ", label_style),
        Span::raw(state.balance.clone()),
    ])
}

fn log_lines(state: &ConsoleState, max_lines: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if max_lines == 0 {
        return lines;
    }

    let start = state.logs.len().saturating_sub(max_lines);
    for entry in state.logs.iter().skip(start) {
        let (label, style) = match entry.level {
            LogLevel::Info => ("[INFO] ", Style::default()),
            LogLevel::Warn => ("[WARN] ", Style::default().fg(Color::Yellow)),
        };

        lines.push(Line::from(vec![
            Span::styled(label, style),
            Span::styled(entry.message.clone(), style),
        ]));
    }

    lines
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let _ = terminal.show_cursor();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, cursor::Show);
}
