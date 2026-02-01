use std::fmt;
use std::io::Write as _;

use atty::Stream;
use clap::Parser;
use rpassword;
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
        let seed_value = if let Some(seed) = cli.seed {
            seed
        } else {
            read_seed_from_stdin()?
        };
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
    if !seed.bytes().all(|b| (b'a'..=b'z').contains(&b)) {
        return Err("seed must contain only a-z characters".to_string());
    }
    Ok(())
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
        let mut input = String::new();
        let bytes = std::io::stdin()
            .read_line(&mut input)
            .map_err(|err| format!("failed to read seed from stdin: {err}"))?;
        if bytes == 0 {
            return Err("seed from stdin is empty".to_string());
        }
        let seed = input.trim().to_string();
        input.zeroize();
        seed
    };
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
