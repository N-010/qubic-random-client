use clap::Parser;

const DEFAULT_CONTRACT_ID: &str = "DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG";
const DEFAULT_COMMIT_AMOUNT: u64 = 10000;
const DEFAULT_REVEAL_DELAY_TICKS: u32 = 3;
const DEFAULT_TX_TICK_OFFSET: u32 = 5;
const DEFAULT_TICK_POLL_INTERVAL_MS: u64 = 100;
const DEFAULT_PIPELINE_SLEEP_MS: u64 = 200;
const DEFAULT_PIPELINE_COUNT: usize = 3;
const DEFAULT_ENDPOINT: &str = "https://rpc.qubic.org/live/v1/";

const DEFAULT_BALANCE_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Parser)]
#[command(name = "random-client", version, about = "Random SC client")]
pub struct Cli {
    #[arg(long)]
    pub seed: String,

    #[arg(long, default_value_t = 0)]
    pub workers: usize,

    #[arg(long, default_value_t = DEFAULT_REVEAL_DELAY_TICKS)]
    pub reveal_delay_ticks: u32,

    #[arg(long, default_value_t = DEFAULT_TX_TICK_OFFSET)]
    pub tx_tick_offset: u32,

    #[arg(long, default_value_t = DEFAULT_COMMIT_AMOUNT)]
    pub commit_amount: u64,

    #[arg(long, default_value_t = DEFAULT_PIPELINE_SLEEP_MS)]
    pub pipeline_sleep_ms: u64,

    #[arg(long, default_value_t = DEFAULT_PIPELINE_COUNT)]
    pub pipeline_count: usize,

    #[arg(long, default_value_t = DEFAULT_TICK_POLL_INTERVAL_MS)]
    pub tick_poll_interval_ms: u64,

    #[arg(long, default_value = DEFAULT_CONTRACT_ID)]
    pub contract_id: String,

    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    pub endpoint: String,

    #[arg(long, default_value_t = false)]
    pub persist_pending: bool,

    #[arg(long, default_value_t = DEFAULT_BALANCE_INTERVAL_MS)]
    pub balance_interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub seed: String,
    pub workers: usize,
    pub reveal_delay_ticks: u32,
    pub tx_tick_offset: u32,
    pub commit_amount: u64,
    pub pipeline_sleep_ms: u64,
    pub pipeline_count: usize,
    pub tick_poll_interval_ms: u64,
    pub contract_id: String,
    pub endpoint: String,
    pub balance_interval_ms: u64,
}

impl Config {
    pub fn from_cli() -> Result<Self, String> {
        let cli = Cli::parse();
        validate_seed(&cli.seed)?;
        let workers = if cli.workers == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cli.workers
        };

        Ok(Self {
            seed: cli.seed,
            workers,
            reveal_delay_ticks: cli.reveal_delay_ticks,
            tx_tick_offset: cli.tx_tick_offset,
            commit_amount: cli.commit_amount,
            pipeline_sleep_ms: cli.pipeline_sleep_ms,
            pipeline_count: cli.pipeline_count,
            tick_poll_interval_ms: cli.tick_poll_interval_ms,
            contract_id: cli.contract_id,
            endpoint: cli.endpoint,
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
