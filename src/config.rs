use std::fmt;
use std::io::Write as _;

use atty::Stream;
use clap::Parser;
use zeroize::Zeroize;

const DEFAULT_SENDERS: usize = 3;
const DEFAULT_COMMIT_REVEAL_PIPELINE_COUNT: usize = 3;
const DEFAULT_RUNTIME_THREADS: usize = 0;
const DEFAULT_COMMIT_AMOUNT: u64 = 10_000;
const DEFAULT_REVEAL_DELAY_TICKS: u32 = 3;
const DEFAULT_REVEAL_SEND_GUARD_TICKS: u32 = 6;
const DEFAULT_TICK_POLL_INTERVAL_MS: u64 = 1000;
const DEFAULT_BALANCE_INTERVAL_MS: u64 = 600;
const DEFAULT_EMPTY_TICK_CHECK_INTERVAL_MS: u64 = 600;
const DEFAULT_TICK_DATA_MIN_DELAY_TICKS: u32 = 10;
const DEFAULT_EPOCH_STOP_LEAD_TIME_SECS: u64 = 600;
const DEFAULT_EPOCH_RESUME_DELAY_TICKS: u32 = 50;
const DEFAULT_RPC_ENDPOINT: &str = "https://rpc.qubic.org/live/v1/";
const DEFAULT_BOB_ENDPOINT: &str = scapi::bob::DEFAULT_BOB_RPC_ENDPOINT;
const DEFAULT_GRPC_ENDPOINT: &str = "http://127.0.0.1:50051";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Rpc,
    Bob,
    QlnGrpc,
}

#[derive(Debug, Parser)]
#[command(name = "random-client", version, about = "Random SC client")]
pub struct Cli {
    #[arg(long)]
    pub seed: Option<String>,

    #[arg(long = "max-inflight-sends", default_value_t = DEFAULT_SENDERS)]
    pub max_inflight_sends: usize,

    #[arg(long, default_value_t = DEFAULT_REVEAL_DELAY_TICKS)]
    pub reveal_delay_ticks: u32,

    #[arg(long = "reveal-window-ticks", default_value_t = DEFAULT_REVEAL_SEND_GUARD_TICKS)]
    pub reveal_window_ticks: u32,

    #[arg(long, default_value_t = DEFAULT_COMMIT_AMOUNT)]
    pub commit_amount: u64,

    #[arg(long = "pipeline-count", default_value_t = DEFAULT_COMMIT_REVEAL_PIPELINE_COUNT)]
    pub pipeline_count: usize,

    #[arg(long = "worker-threads", default_value_t = DEFAULT_RUNTIME_THREADS)]
    pub worker_threads: usize,

    #[arg(long = "tick-poll", default_value_t = DEFAULT_TICK_POLL_INTERVAL_MS)]
    pub tick_poll: u64,

    #[arg(long, num_args = 0..=1, default_missing_value = DEFAULT_RPC_ENDPOINT)]
    pub rpc: Option<String>,

    #[arg(long, default_value_t = DEFAULT_BALANCE_INTERVAL_MS)]
    pub balance_interval_ms: u64,

    #[arg(
        long = "empty-tick-check-interval-ms",
        alias = "reveal-checks",
        default_value_t = DEFAULT_EMPTY_TICK_CHECK_INTERVAL_MS
    )]
    pub empty_tick_check_interval_ms: u64,

    #[arg(
        long = "reveal-check-delay-ticks",
        default_value_t = DEFAULT_TICK_DATA_MIN_DELAY_TICKS
    )]
    pub reveal_check_delay_ticks: u32,

    #[arg(long, default_value_t = DEFAULT_EPOCH_STOP_LEAD_TIME_SECS)]
    pub epoch_stop_lead_time_secs: u64,

    #[arg(long, default_value_t = DEFAULT_EPOCH_RESUME_DELAY_TICKS)]
    pub epoch_resume_delay_ticks: u32,

    #[arg(long, num_args = 0..=1, default_missing_value = DEFAULT_BOB_ENDPOINT)]
    pub bob: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = DEFAULT_GRPC_ENDPOINT)]
    pub grpc: Option<String>,
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
    pub max_inflight_sends: usize,
    pub reveal_delay_ticks: u32,
    pub reveal_window_ticks: u32,
    pub commit_amount: u64,
    pub pipeline_count: usize,
    pub worker_threads: usize,
    pub tick_poll: u64,
    pub endpoint: String,
    pub backend: Backend,
    pub balance_interval_ms: u64,
    pub empty_tick_check_interval_ms: u64,
    pub reveal_check_delay_ticks: u32,
    pub epoch_stop_lead_time_secs: u64,
    pub epoch_resume_delay_ticks: u32,
    pub bob_endpoint: String,
    pub grpc_endpoint: String,
}

impl AppConfig {
    pub fn from_cli() -> Result<Self, String> {
        let mut cli = Cli::parse();
        let seed_value = resolve_seed(cli.seed.take(), read_seed_from_stdin)?;
        Self::from_cli_inner(cli, seed_value)
    }

    fn from_cli_inner(cli: Cli, seed_value: String) -> Result<Self, String> {
        let seed = Seed::new(seed_value)?;
        let max_inflight_sends = if cli.max_inflight_sends == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.max_inflight_sends
        };
        let worker_threads = if cli.worker_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.worker_threads
        };

        if cli.bob.is_some() && cli.grpc.is_some() {
            return Err("--bob and --grpc cannot be used together".to_string());
        }
        let backend = if cli.bob.is_some() {
            Backend::Bob
        } else if cli.grpc.is_some() {
            Backend::QlnGrpc
        } else {
            Backend::Rpc
        };
        let rpc_endpoint = cli.rpc.unwrap_or_else(|| DEFAULT_RPC_ENDPOINT.to_string());
        let bob_endpoint = cli.bob.unwrap_or_else(|| DEFAULT_BOB_ENDPOINT.to_string());
        let grpc_endpoint = cli
            .grpc
            .unwrap_or_else(|| DEFAULT_GRPC_ENDPOINT.to_string());

        Ok(Self {
            seed,
            runtime: Config {
                max_inflight_sends,
                reveal_delay_ticks: cli.reveal_delay_ticks,
                reveal_window_ticks: cli.reveal_window_ticks,
                commit_amount: cli.commit_amount,
                pipeline_count: cli.pipeline_count,
                worker_threads,
                tick_poll: cli.tick_poll,
                endpoint: rpc_endpoint,
                backend,
                balance_interval_ms: cli.balance_interval_ms,
                empty_tick_check_interval_ms: cli.empty_tick_check_interval_ms,
                reveal_check_delay_ticks: cli.reveal_check_delay_ticks,
                epoch_stop_lead_time_secs: cli.epoch_stop_lead_time_secs,
                epoch_resume_delay_ticks: cli.epoch_resume_delay_ticks,
                bob_endpoint,
                grpc_endpoint,
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

fn resolve_seed<F>(seed: Option<String>, read_seed: F) -> Result<String, String>
where
    F: FnOnce() -> Result<String, String>,
{
    if let Some(seed) = seed {
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
    use super::{
        AppConfig, Backend, Cli, DEFAULT_BOB_ENDPOINT, DEFAULT_RPC_ENDPOINT, Seed,
        read_seed_from_reader, resolve_seed, validate_seed,
    };
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
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: None,
            grpc: None,
        };
        let result = resolve_seed(cli.seed, || Err("should not read".to_string()));
        assert_eq!(result.expect("seed"), "a".repeat(55));
    }

    #[test]
    fn resolve_seed_uses_reader_error() {
        // If --seed is missing, the reader error must be propagated.
        let cli = Cli {
            seed: None,
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: None,
            grpc: None,
        };
        let err = resolve_seed(cli.seed, || Err("no seed".to_string())).expect_err("expected err");
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
    fn from_cli_inner_auto_threads_and_max_inflight_sends() {
        // Zero values are replaced by available_parallelism.
        let cli = Cli {
            seed: None,
            max_inflight_sends: 0,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 0,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: None,
            grpc: None,
        };
        let config = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect("config");
        assert!(config.runtime.max_inflight_sends > 0);
        assert!(config.runtime.worker_threads > 0);
    }

    #[test]
    fn from_cli_inner_uses_bob_default_endpoint() {
        let cli = Cli {
            seed: None,
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: Some(DEFAULT_BOB_ENDPOINT.to_string()),
            grpc: None,
        };
        let config = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect("config");
        assert_eq!(config.runtime.backend, Backend::Bob);
        assert_eq!(config.runtime.bob_endpoint, DEFAULT_BOB_ENDPOINT);
    }

    #[test]
    fn from_cli_inner_uses_rpc_default_endpoint() {
        let cli = Cli {
            seed: None,
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: None,
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: None,
            grpc: None,
        };
        let config = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect("config");
        assert_eq!(config.runtime.endpoint, DEFAULT_RPC_ENDPOINT);
    }

    #[test]
    fn from_cli_inner_uses_grpc_custom_endpoint() {
        let cli = Cli {
            seed: None,
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: None,
            grpc: Some("http://127.0.0.1:50052".to_string()),
        };
        let config = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect("config");
        assert_eq!(config.runtime.backend, Backend::QlnGrpc);
        assert_eq!(config.runtime.grpc_endpoint, "http://127.0.0.1:50052");
    }

    #[test]
    fn from_cli_inner_rejects_bob_and_grpc_together() {
        let cli = Cli {
            seed: None,
            max_inflight_sends: 1,
            reveal_delay_ticks: 3,
            reveal_window_ticks: 2,
            commit_amount: 10,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 10,
            rpc: Some("endpoint".to_string()),
            balance_interval_ms: 10,
            empty_tick_check_interval_ms: 10,
            reveal_check_delay_ticks: 10,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 50,
            bob: Some("http://127.0.0.1:40420/qubic".to_string()),
            grpc: Some("http://127.0.0.1:50051".to_string()),
        };
        let err = AppConfig::from_cli_inner(cli, "a".repeat(55)).expect_err("expected err");
        assert_eq!(err, "--bob and --grpc cannot be used together");
    }

    #[test]
    fn seed_expose_returns_original() {
        // Seed::expose returns the exact original string.
        let seed_value = "a".repeat(55);
        let seed = Seed::new(seed_value.clone()).expect("seed");
        assert_eq!(seed.expose(), seed_value);
    }
}
