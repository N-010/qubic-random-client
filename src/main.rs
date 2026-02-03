mod app;
mod balance;
mod config;
mod console;
mod entropy;
mod heap_prof;
mod pipeline;
mod protocol;
mod tick_data_watcher;
mod ticks;
mod transport;

use app::{AppResult, run};
use config::AppConfig;

#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "mimalloc", target_os = "windows", not(feature = "jemalloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> AppResult<()> {
    let config = AppConfig::from_cli().map_err(|err| {
        eprintln!("config error: {}", err);
        err
    })?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.runtime.runtime_threads)
        .enable_all()
        .build()?;

    heap_prof::setup(runtime.handle(), &config.runtime);
    runtime.block_on(run(config))?;
    Ok(())
}
