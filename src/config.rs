use std::path::PathBuf;

use clap::Parser;

const DEFAULT_CONTRACT_ID: &str = "DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG";
const DEFAULT_DEPOSIT_AMOUNT: u64 = 1000;
const DEFAULT_REVEAL_DELAY_TICKS: u32 = 3;
const DEFAULT_ENDPOINT: &str = "https://rpc.qubic.org/live/v1/";
const DEFAULT_DATA_DIR: &str = "data";

#[derive(Debug, Parser)]
#[command(name = "random-client", version, about = "Random SC client")]
pub struct Cli {
    #[arg(long)]
    pub seed: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub workers: usize,

    #[arg(long, default_value_t = 0)]
    pub threads: usize,

    #[arg(long, default_value_t = DEFAULT_REVEAL_DELAY_TICKS)]
    pub reveal_delay_ticks: u32,

    #[arg(long, default_value_t = DEFAULT_DEPOSIT_AMOUNT)]
    pub deposit_amount: u64,

    #[arg(long, default_value = DEFAULT_CONTRACT_ID)]
    pub contract_id: String,

    #[arg(long, default_value_t = 3)]
    pub contract_index: u32,

    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,

    #[arg(long, default_value = DEFAULT_DATA_DIR)]
    pub data_dir: PathBuf,

    #[arg(long, default_value_t = false)]
    pub persist_pending: bool,

    #[arg(long, default_value_t = 1000)]
    pub balance_interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub seed: Option<String>,
    pub workers: usize,
    pub threads: usize,
    pub reveal_delay_ticks: u32,
    pub deposit_amount: u64,
    pub contract_id: String,
    pub contract_index: u32,
    pub endpoint: String,
    pub data_dir: PathBuf,
    pub persist_pending: bool,
    pub balance_interval_ms: u64,
}

impl Config {
    pub fn from_cli() -> Result<Self, String> {
        let cli = Cli::parse();
        if let Some(seed) = cli.seed.as_deref() {
            validate_seed(seed)?;
        }
        let threads = if cli.threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.threads
        };
        let workers = if cli.workers == 0 { threads } else { cli.workers };

        Ok(Self {
            seed: cli.seed,
            workers,
            threads,
            reveal_delay_ticks: cli.reveal_delay_ticks,
            deposit_amount: cli.deposit_amount,
            contract_id: cli.contract_id,
            contract_index: cli.contract_index,
            endpoint: cli.endpoint,
            data_dir: cli.data_dir,
            persist_pending: cli.persist_pending,
            balance_interval_ms: cli.balance_interval_ms,
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
