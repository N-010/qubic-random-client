use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::balance::run_balance_watcher;
use crate::config::Config;
use crate::console;
use crate::pipeline::{Pipeline, run_job_dispatcher};
use crate::ticks::ScapiTickSource;
use crate::transport::{NullTransport, ScTransport, ScapiClient, ScapiRpcClient};
use crate::wallet::QubicWallet;

pub type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub async fn run(config: Config) -> AppResult<()> {
    console::init();
    let identity = match config.seed.as_deref() {
        Some(seed) => match QubicWallet::from_seed(seed) {
            Ok(wallet) => Some(wallet.get_identity()),
            Err(err) => {
                console::log_warn(format!("failed to derive identity from seed: {}", err));
                None
            }
        },
        None => None,
    };

    let client: Arc<dyn ScapiClient> = Arc::new(ScapiRpcClient::new(
        config.endpoint.clone(),
        identity.clone(),
    ));
    let transport: Arc<dyn ScTransport> = Arc::new(NullTransport::default());

    let (tick_tx, tick_rx) = mpsc::channel(64);
    let (job_tx, job_rx) = mpsc::channel(128);

    let tick_source = ScapiTickSource::new(client.clone(), Duration::from_millis(200));
    tokio::spawn(tick_source.run(tick_tx));

    if identity.is_some() {
        tokio::spawn(run_balance_watcher(
            client.clone(),
            Duration::from_millis(config.balance_interval_ms),
        ));
    } else {
        console::log_warn("balance watcher disabled: --seed not set");
    }

    let pipeline = Pipeline::new(config.clone());
    tokio::spawn(pipeline.run(tick_rx, job_tx));

    tokio::spawn(run_job_dispatcher(job_rx, transport, config.workers));

    tokio::signal::ctrl_c().await?;
    Ok(())
}
