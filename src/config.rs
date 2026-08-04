use std::fmt;
use std::io::{IsTerminal as _, Write as _};

use clap::{Parser, ValueEnum};
use zeroize::Zeroize;

const DEFAULT_RPC_ENDPOINT: &str = "https://rpc.qubic.org";
const DEFAULT_BOB_ENDPOINT: &str = scapi::bob::DEFAULT_BOB_RPC_ENDPOINT;
const DEFAULT_GRPC_ENDPOINT: &str = "http://127.0.0.1:50051";
const DEFAULT_COLLATERAL: u64 = 10_000;
const DEFAULT_EMPTY_TICK_CHECK_INTERVAL_MS: u64 = 600;
const DEFAULT_REVEAL_CHECK_DELAY_TICKS: u32 = 10;
const DEFAULT_EPOCH_STOP_LEAD_TIME_SECS: u64 = 600;
const DEFAULT_EPOCH_RESUME_DELAY_TICKS: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendKind {
    Rpc,
    Bob,
    #[value(name = "grpc", alias = "qln")]
    Grpc,
}

impl BackendKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Bob => "bob",
            Self::Grpc => "grpc",
        }
    }

    fn default_endpoint(self) -> &'static str {
        match self {
            Self::Rpc => DEFAULT_RPC_ENDPOINT,
            Self::Bob => DEFAULT_BOB_ENDPOINT,
            Self::Grpc => DEFAULT_GRPC_ENDPOINT,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "random-client",
    version,
    about = "Qubic Random provider client"
)]
struct Cli {
    #[arg(
        long,
        value_name = "SEED",
        help = "Seed; reads securely from stdin if omitted"
    )]
    seed: Option<String>,

    #[arg(long, value_enum, default_value_t = BackendKind::Rpc)]
    backend: BackendKind,

    #[arg(long, value_name = "URL", help = "Endpoint for the selected backend")]
    endpoint: Option<String>,

    #[arg(long, default_value_t = DEFAULT_COLLATERAL)]
    collateral: u64,

    #[arg(
        long = "empty-check-ms",
        default_value_t = DEFAULT_EMPTY_TICK_CHECK_INTERVAL_MS,
        value_parser = parse_positive_u64
    )]
    empty_tick_check_interval_ms: u64,

    #[arg(
        long = "reveal-verify-after",
        default_value_t = DEFAULT_REVEAL_CHECK_DELAY_TICKS,
        value_parser = parse_positive_u32
    )]
    reveal_check_delay_ticks: u32,

    #[arg(
        long = "stop-before-epoch-end-secs",
        default_value_t = DEFAULT_EPOCH_STOP_LEAD_TIME_SECS
    )]
    epoch_stop_lead_time_secs: u64,

    #[arg(
        long = "resume-after-epoch-start-ticks",
        default_value_t = DEFAULT_EPOCH_RESUME_DELAY_TICKS
    )]
    epoch_resume_delay_ticks: u32,
}

pub struct AppConfig {
    pub seed: Seed,
    pub backend: BackendKind,
    pub endpoint: String,
    pub collateral: u64,
    pub empty_tick_check_interval_ms: u64,
    pub reveal_check_delay_ticks: u32,
    pub epoch_stop_lead_time_secs: u64,
    pub epoch_resume_delay_ticks: u32,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("seed", &self.seed)
            .field("backend", &self.backend)
            .field("endpoint", &redacted_endpoint(&self.endpoint))
            .field("collateral", &self.collateral)
            .field(
                "empty_tick_check_interval_ms",
                &self.empty_tick_check_interval_ms,
            )
            .field("reveal_check_delay_ticks", &self.reveal_check_delay_ticks)
            .field("epoch_stop_lead_time_secs", &self.epoch_stop_lead_time_secs)
            .field("epoch_resume_delay_ticks", &self.epoch_resume_delay_ticks)
            .finish()
    }
}

impl AppConfig {
    pub fn from_cli() -> Result<Self, String> {
        let mut cli = Cli::parse();
        let seed = Seed::new(match cli.seed.take() {
            Some(seed) => seed,
            None => read_seed_from_stdin()?,
        })?;
        validate_collateral(cli.collateral)?;
        let endpoint = normalize_endpoint(
            cli.backend,
            cli.endpoint
                .unwrap_or_else(|| cli.backend.default_endpoint().to_string()),
        );

        Ok(Self {
            seed,
            backend: cli.backend,
            endpoint,
            collateral: cli.collateral,
            empty_tick_check_interval_ms: cli.empty_tick_check_interval_ms,
            reveal_check_delay_ticks: cli.reveal_check_delay_ticks,
            epoch_stop_lead_time_secs: cli.epoch_stop_lead_time_secs,
            epoch_resume_delay_ticks: cli.epoch_resume_delay_ticks,
        })
    }
}

fn parse_positive_u64(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|err| format!("invalid positive integer: {err}"))
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "value must be greater than zero".to_string())
        })
}

fn parse_positive_u32(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|err| format!("invalid positive integer: {err}"))
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "value must be greater than zero".to_string())
        })
}

pub struct Seed(LockedSeed);

impl Seed {
    fn new(mut seed: String) -> Result<Self, String> {
        if seed.len() != 55 || !seed.bytes().all(|byte| byte.is_ascii_lowercase()) {
            seed.zeroize();
            return Err("seed must contain exactly 55 lowercase a-z characters".to_string());
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

fn normalize_endpoint(backend: BackendKind, endpoint: String) -> String {
    let mut endpoint = endpoint.trim().trim_end_matches('/').to_string();
    if backend == BackendKind::Rpc {
        for suffix in ["/live/v1", "/query/v1", "/v1"] {
            if let Some(base) = endpoint.strip_suffix(suffix) {
                endpoint = base.trim_end_matches('/').to_string();
                break;
            }
        }
    } else if backend == BackendKind::Bob
        && let Some(base) = endpoint.strip_suffix("/qubic")
    {
        endpoint = base.trim_end_matches('/').to_string();
    }
    if endpoint.contains("://") {
        endpoint
    } else {
        format!("http://{endpoint}")
    }
}

pub fn redacted_endpoint(endpoint: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(endpoint) else {
        return "<redacted-endpoint>".to_string();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string().trim_end_matches('/').to_string()
}

fn validate_collateral(amount: u64) -> Result<(), String> {
    if matches!(
        amount,
        1 | 10
            | 100
            | 1_000
            | 10_000
            | 100_000
            | 1_000_000
            | 10_000_000
            | 100_000_000
            | 1_000_000_000
    ) {
        Ok(())
    } else {
        Err("--collateral must be a power of ten from 1 through 1000000000".to_string())
    }
}

fn read_seed_from_stdin() -> Result<String, String> {
    let mut input = if std::io::stdin().is_terminal() {
        print!("seed: ");
        std::io::stdout()
            .flush()
            .map_err(|err| format!("failed to flush stdout: {err}"))?;
        rpassword::read_password().map_err(|err| format!("failed to read seed: {err}"))?
    } else {
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|err| format!("failed to read seed: {err}"))?;
        input
    };
    let seed = input.trim().to_string();
    input.zeroize();
    if seed.is_empty() {
        Err("seed from stdin is empty".to_string())
    } else {
        Ok(seed)
    }
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
        std::str::from_utf8(&self.bytes).expect("validated seed is ASCII")
    }
}

impl Drop for LockedSeed {
    fn drop(&mut self) {
        self.bytes.as_mut_slice().zeroize();
        unlock_bytes(&self.bytes);
    }
}

#[cfg(unix)]
fn lock_bytes(bytes: &[u8]) -> Result<(), String> {
    // SAFETY: `bytes` is a valid allocation for the supplied length and remains alive.
    let result = unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) };
    (result == 0)
        .then_some(())
        .ok_or_else(|| format!("mlock failed: {}", std::io::Error::last_os_error()))
}

#[cfg(unix)]
fn unlock_bytes(bytes: &[u8]) {
    // SAFETY: this releases the same live allocation previously passed to `mlock`.
    let _ = unsafe { libc::munlock(bytes.as_ptr().cast(), bytes.len()) };
}

#[cfg(windows)]
fn lock_bytes(bytes: &[u8]) -> Result<(), String> {
    // SAFETY: `bytes` is a valid allocation for the supplied length and remains alive.
    let result = unsafe {
        windows_sys::Win32::System::Memory::VirtualLock(bytes.as_ptr().cast(), bytes.len())
    };
    (result != 0)
        .then_some(())
        .ok_or_else(|| format!("VirtualLock failed: {}", std::io::Error::last_os_error()))
}

#[cfg(windows)]
fn unlock_bytes(bytes: &[u8]) {
    // SAFETY: this releases the same live allocation previously passed to `VirtualLock`.
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
    use super::*;

    #[test]
    fn endpoint_normalization_is_backend_specific() {
        assert_eq!(
            normalize_endpoint(BackendKind::Rpc, "rpc.qubic.org/live/v1/".to_string()),
            "http://rpc.qubic.org"
        );
        assert_eq!(
            normalize_endpoint(BackendKind::Bob, "http://localhost:40420/qubic".to_string()),
            "http://localhost:40420"
        );
    }

    #[test]
    fn collateral_accepts_only_contract_tiers() {
        assert!(validate_collateral(1_000_000_000).is_ok());
        assert!(validate_collateral(0).is_err());
        assert!(validate_collateral(11).is_err());
    }

    #[test]
    fn monitoring_options_have_stable_defaults_and_reject_zero() {
        use clap::Parser as _;

        let seed = "a".repeat(55);
        let cli = Cli::try_parse_from(["random-client", "--seed", &seed]).expect("defaults");
        assert_eq!(cli.empty_tick_check_interval_ms, 600);
        assert_eq!(cli.reveal_check_delay_ticks, 10);

        assert!(
            Cli::try_parse_from(["random-client", "--seed", &seed, "--empty-check-ms", "0",])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "random-client",
                "--seed",
                &seed,
                "--reveal-verify-after",
                "0",
            ])
            .is_err()
        );
    }

    #[test]
    fn debug_never_exposes_seed() {
        let seed = Seed::new("a".repeat(55)).expect("valid seed");
        assert_eq!(format!("{seed:?}"), "Seed(REDACTED)");
    }

    #[test]
    fn endpoint_redaction_removes_credentials_and_query() {
        assert_eq!(
            redacted_endpoint("https://alice:secret@example.invalid:8443/api?token=secret#part"),
            "https://example.invalid:8443/api"
        );
    }
}
