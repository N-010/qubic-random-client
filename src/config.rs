use std::fmt;
use std::io::Write as _;

use atty::Stream;
use clap::Parser;
use zeroize::Zeroize;

const DEFAULT_SENDERS: usize = 3;

const DEFAULT_CONTRACT_ID: &str = "DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG";
const DEFAULT_COMMIT_AMOUNT: u64 = 10_000;
const DEFAULT_REVEAL_DELAY_TICKS: u32 = 3;
const DEFAULT_TICK_POLL_INTERVAL_MS: u64 = 50;
const DEFAULT_COMMIT_REVEAL_SLEEP_MS: u64 = 200;
const DEFAULT_COMMIT_REVEAL_PIPELINE_COUNT: usize = 3;
const DEFAULT_RUNTIME_THREADS: usize = 0;
const DEFAULT_ENDPOINT: &str = "https://rpc.qubic.org/live/v1/";

const DEFAULT_BALANCE_INTERVAL_MS: u64 = 300;

#[derive(Debug, Parser)]
#[command(name = "random-client", version, about = "Random SC client")]
pub struct Cli {
    #[arg(long)]
    pub seed: Option<String>,

    #[arg(long, default_value_t = DEFAULT_SENDERS)]
    pub senders: usize,

    #[arg(long, default_value_t = DEFAULT_REVEAL_DELAY_TICKS)]
    pub reveal_delay_ticks: u32,

    #[arg(long, default_value_t = DEFAULT_COMMIT_AMOUNT)]
    pub commit_amount: u64,

    #[arg(long, default_value_t = DEFAULT_COMMIT_REVEAL_SLEEP_MS)]
    pub commit_reveal_sleep_ms: u64,

    #[arg(long, default_value_t = DEFAULT_COMMIT_REVEAL_PIPELINE_COUNT)]
    pub commit_reveal_pipeline_count: usize,

    #[arg(long, default_value_t = DEFAULT_RUNTIME_THREADS)]
    pub runtime_threads: usize,

    #[arg(long, default_value_t = DEFAULT_TICK_POLL_INTERVAL_MS)]
    pub tick_poll_interval_ms: u64,

    #[arg(long, default_value = DEFAULT_CONTRACT_ID)]
    pub contract_id: String,

    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,

    #[arg(long, default_value_t = DEFAULT_BALANCE_INTERVAL_MS)]
    pub balance_interval_ms: u64,
}

pub struct Seed(LockedSeed);

impl Seed {
    fn new(mut seed: String) -> Result<Self, String> {
        if let Err(err) = validate_seed(&seed) {
            seed.zeroize();
            return Err(err);
        }
        LockedSeed::new(seed).map(Self)
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(REDACTED)")
    }
}

#[derive(Debug)]
pub struct AppConfig {
    pub seed: Seed,
    pub runtime: Config,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub senders: usize,
    pub reveal_delay_ticks: u32,
    pub commit_amount: u64,
    pub commit_reveal_sleep_ms: u64,
    pub commit_reveal_pipeline_count: usize,
    pub runtime_threads: usize,
    pub tick_poll_interval_ms: u64,
    pub contract_id: String,
    pub endpoint: String,
    pub balance_interval_ms: u64,
}

impl AppConfig {
    pub fn from_cli() -> Result<Self, String> {
        let cli = Cli::parse();
        let seed_value = resolve_seed(&cli, read_seed_from_stdin)?;
        Self::from_cli_inner(cli, seed_value)
    }

    fn from_cli_inner(cli: Cli, seed_value: String) -> Result<Self, String> {
        let seed = Seed::new(seed_value)?;
        let senders = if cli.senders == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.senders
        };
        let runtime_threads = if cli.runtime_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.runtime_threads
        };

        Ok(Self {
            seed,
            runtime: Config {
                senders,
                reveal_delay_ticks: cli.reveal_delay_ticks,
                commit_amount: cli.commit_amount,
                commit_reveal_sleep_ms: cli.commit_reveal_sleep_ms,
                commit_reveal_pipeline_count: cli.commit_reveal_pipeline_count,
                runtime_threads,
                tick_poll_interval_ms: cli.tick_poll_interval_ms,
                contract_id: cli.contract_id,
                endpoint: cli.endpoint,
                balance_interval_ms: cli.balance_interval_ms,
            },
        })
    }
}

fn validate_seed(seed: &str) -> Result<(), String> {
    if seed.len() != 55 {
        return Err("seed must be 55 characters".to_string());
    }
    if !seed.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err("seed must contain only a-z characters".to_string());
    }
    Ok(())
}

fn resolve_seed<F>(cli: &Cli, read_seed: F) -> Result<String, String>
where
    F: FnOnce() -> Result<String, String>,
{
    if let Some(seed) = cli.seed.clone() {
        Ok(seed)
    } else {
        read_seed()
    }
}

fn read_seed_from_stdin() -> Result<String, String> {
    let seed = if atty::is(Stream::Stdin) {
        print!("seed: ");
        std::io::stdout()
            .flush()
            .map_err(|err| format!("failed to flush stdout: {err}"))?;
        let mut input = rpassword::read_password()
            .map_err(|err| format!("failed to read seed from tty: {err}"))?;
        let seed = input.trim().to_string();
        input.zeroize();
        seed
    } else {
        read_seed_from_reader(std::io::stdin().lock())?
    };
    if seed.is_empty() {
        return Err("seed from stdin is empty".to_string());
    }
    Ok(seed)
}

fn read_seed_from_reader<R: std::io::BufRead>(mut reader: R) -> Result<String, String> {
    let mut input = String::new();
    let bytes = reader
        .read_line(&mut input)
        .map_err(|err| format!("failed to read seed from stdin: {err}"))?;
    if bytes == 0 {
        return Err("seed from stdin is empty".to_string());
    }
    let seed = input.trim().to_string();
    input.zeroize();
    if seed.is_empty() {
        return Err("seed from stdin is empty".to_string());
    }
    Ok(seed)
}

struct LockedSeed {
    bytes: Vec<u8>,
}

impl LockedSeed {
    fn new(seed: String) -> Result<Self, String> {
        let bytes = seed.into_bytes();
        if let Err(err) = lock_bytes(&bytes) {
            let mut bytes = bytes;
            bytes.zeroize();
            return Err(err);
        }
        Ok(Self { bytes })
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("seed must be validated as lowercase ascii")
    }
}

impl Drop for LockedSeed {
    fn drop(&mut self) {
        self.bytes.zeroize();
        unlock_bytes(&self.bytes);
    }
}

#[cfg(unix)]
fn lock_bytes(bytes: &[u8]) -> Result<(), String> {
    let result = unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!("mlock failed: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(unix)]
fn unlock_bytes(bytes: &[u8]) {
    let _ = unsafe { libc::munlock(bytes.as_ptr().cast(), bytes.len()) };
}

#[cfg(windows)]
fn lock_bytes(bytes: &[u8]) -> Result<(), String> {
    let result = unsafe {
        windows_sys::Win32::System::Memory::VirtualLock(bytes.as_ptr().cast(), bytes.len())
    };
    if result != 0 {
        Ok(())
    } else {
        Err(format!(
            "VirtualLock failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(windows)]
fn unlock_bytes(bytes: &[u8]) {
    let _ = unsafe {
        windows_sys::Win32::System::Memory::VirtualUnlock(bytes.as_ptr().cast(), bytes.len())
    };
}

#[cfg(not(any(unix, windows)))]
fn lock_bytes(_bytes: &[u8]) -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn unlock_bytes(_bytes: &[u8]) {}

#[cfg(test)]
mod tests {
    use super::{AppConfig, Cli, Seed, read_seed_from_reader, resolve_seed, validate_seed};
    use std::io::Cursor;

    #[test]
    fn validate_seed_accepts_55_lowercase() {
        // 55 lowercase letters is the only valid seed shape.
        let seed = "a".repeat(55);
        assert!(validate_seed(&seed).is_ok());
    }

    #[test]
    fn validate_seed_rejects_wrong_length() {
        // 54 chars should fail length validation.
        let seed = "a".repeat(54);
        assert!(validate_seed(&seed).is_err());
    }

    #[test]
    fn validate_seed_rejects_non_lowercase() {
        // Uppercase is not allowed even if length is correct.
        let seed = "a".repeat(54) + "Z";
        assert!(validate_seed(&seed).is_err());
    }

    #[test]
    fn resolve_seed_prefers_cli_value() {
        // When --seed is set, stdin must not be read.
        let cli = Cli {
            seed: Some("a".repeat(55)),
            senders: 1,
            reveal_delay_ticks: 3,
            commit_amount: 10,
            commit_reveal_sleep_ms: 10,
            commit_reveal_pipeline_count: 1,
            runtime_threads: 1,
            tick_poll_interval_ms: 10,
            contract_id: "id".to_string(),
            endpoint: "endpoint".to_string(),
            balance_interval_ms: 10,
        };
        let result = resolve_seed(&cli, || Err("should not read".to_string()));
        assert_eq!(result.expect("seed"), "a".repeat(55));
    }

    #[test]
    fn resolve_seed_uses_reader_error() {
        // If --seed is missing, the reader error must be propagated.
        let cli = Cli {
            seed: None,
            senders: 1,
            reveal_delay_ticks: 3,
            commit_amount: 10,
            commit_reveal_sleep_ms: 10,
            commit_reveal_pipeline_count: 1,
            runtime_threads: 1,
            tick_poll_interval_ms: 10,
            contract_id: "id".to_string(),
            endpoint: "endpoint".to_string(),
            balance_interval_ms: 10,
        };
        let err = resolve_seed(&cli, || Err("no seed".to_string())).expect_err("expected err");
        assert_eq!(err, "no seed");
    }

    #[test]
    fn read_seed_from_reader_rejects_empty() {
        // Empty stdin is an error.
        let cursor = Cursor::new("");
        let err = read_seed_from_reader(cursor).expect_err("expected error");
        assert_eq!(err, "seed from stdin is empty");
    }

    #[test]
    fn read_seed_from_reader_trims_input() {
        // Trailing newline is trimmed.
        let cursor = Cursor::new("abc\n");
        let seed = read_seed_from_reader(cursor).expect("seed");
        assert_eq!(seed, "abc");
    }

    #[test]
    fn from_cli_inner_auto_threads_and_senders() {
        // Zero values are replaced by available_parallelism.
        let cli = Cli {
            seed: None,
            senders: 0,
            reveal_delay_ticks: 3,
            commit_amount: 10,
            commit_reveal_sleep_ms: 10,
            commit_reveal_pipeline_count: 1,
            runtime_threads: 0,
            tick_poll_interval_ms: 10,
            contract_id: "id".to_string(),
            endpoint: "endpoint".to_string(),
            balance_interval_ms: 10,
        };
        let config = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect("config");
        assert!(config.runtime.senders > 0);
        assert!(config.runtime.runtime_threads > 0);
    }

    #[test]
    fn seed_expose_returns_original() {
        // Seed::expose returns the exact original string.
        let seed_value = "a".repeat(55);
        let seed = Seed::new(seed_value.clone()).expect("seed");
        assert_eq!(seed.expose(), seed_value);
    }
}
