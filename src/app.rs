use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use scapi::{QubicId as ScapiQubicId, QubicWallet as ScapiQubicWallet};
use tokio::sync::mpsc;

use crate::balance::run_balance_watcher;
use crate::config::Config;
use crate::console;
use crate::pipeline::{Pipeline, run_job_dispatcher};
use crate::ticks::ScapiTickSource;
use crate::transport::{ScTransport, ScapiClient, ScapiContractTransport, ScapiRpcClient};
use crate::wallet::QubicWallet;

pub type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub async fn run(config: Config) -> AppResult<()> {
    console::init();
    let wallet = match QubicWallet::from_seed(&config.seed) {
        Ok(wallet) => wallet,
        Err(err) => {
            console::log_warn(format!("failed to derive wallet from seed: {}", err));
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into());
        }
    };
    let scapi_wallet = match ScapiQubicWallet::from_seed(&config.seed) {
        Ok(wallet) => wallet,
        Err(err) => {
            console::log_warn(format!("failed to derive scapi wallet from seed: {}", err));
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into());
        }
    };
    let contract_id = match ScapiQubicId::from_str(&config.contract_id) {
        Ok(contract_id) => contract_id,
        Err(err) => {
            console::log_warn(format!("invalid contract id: {}", err));
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into());
        }
    };
    let wallet = Arc::new(wallet);
    let identity = wallet.get_identity();

    let client: Arc<dyn ScapiClient> = Arc::new(ScapiRpcClient::new(config.endpoint.clone()));
    let transport: Arc<dyn ScTransport> = Arc::new(ScapiContractTransport::new(
        config.endpoint.clone(),
        scapi_wallet,
        contract_id,
        1,
    ));

    let (tick_tx, tick_rx) = mpsc::channel(64);
    let (job_tx, job_rx) = mpsc::channel(128);

    let tick_source = ScapiTickSource::new(
        client.clone(),
        Duration::from_millis(config.tick_poll_interval_ms),
    );
    tokio::spawn(tick_source.run(tick_tx));

    tokio::spawn(run_balance_watcher(
        client.clone(),
        identity,
        Duration::from_millis(config.balance_interval_ms),
    ));

    let pipeline = Pipeline::new(config.clone());
    tokio::spawn(pipeline.run(tick_rx, job_tx));

    tokio::spawn(run_job_dispatcher(job_rx, transport, config.workers));

    tokio::signal::ctrl_c().await?;
    Ok(())
}
