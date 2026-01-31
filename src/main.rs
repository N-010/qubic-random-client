mod app;
mod balance;
mod config;
mod console;
mod entropy;
mod four_q;
mod pipeline;
mod protocol;
mod ticks;
mod transport;

use app::{AppResult, run};
use config::Config;

fn main() -> AppResult<()> {
    let config = Config::from_cli().map_err(|err| {
        eprintln!("config error: {}", err);
        err
    })?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(run(config))?;
    Ok(())
}
