mod app;
mod balance;
mod config;
mod console;
mod entropy;
mod pipeline;
mod protocol;
mod ticks;
mod transport;

use app::{AppResult, run};
use config::AppConfig;

fn main() -> AppResult<()> {
    let config = AppConfig::from_cli().map_err(|err| {
        eprintln!("config error: {}", err);
        err
    })?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(run(config))?;
    Ok(())
}
