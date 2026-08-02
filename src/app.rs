use scapi::QubicWallet;

use crate::backend::create_backend;
use crate::config::{AppConfig, redacted_endpoint};
use crate::engine::ProviderEngine;

pub type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub async fn run(config: AppConfig) -> AppResult<()> {
    crate::console::init();
    crate::console::set_backend(config.backend.name());
    let wallet = QubicWallet::from_seed(config.seed.expose())?;
    let backend = create_backend(&config)?;
    crate::console::log_info(format!(
        "Using {} backend at {}",
        config.backend.name(),
        redacted_endpoint(&config.endpoint)
    ));
    let engine = ProviderEngine::new(
        backend,
        wallet,
        config.collateral,
        config.epoch_stop_lead_time_secs,
        config.epoch_resume_delay_ticks,
    );
    drop(config.seed);
    engine.run(wait_for_shutdown_signal()).await
}

async fn wait_for_shutdown_signal() -> AppResult<()> {
    #[cfg(windows)]
    {
        use tokio::signal::windows;

        let mut ctrl_c = windows::ctrl_c()?;
        let mut ctrl_break = windows::ctrl_break()?;
        let mut ctrl_close = windows::ctrl_close()?;
        let mut ctrl_logoff = windows::ctrl_logoff()?;
        let mut ctrl_shutdown = windows::ctrl_shutdown()?;
        tokio::select! {
            _ = ctrl_c.recv() => {}
            _ = ctrl_break.recv() => {}
            _ = ctrl_close.recv() => {}
            _ = ctrl_logoff.recv() => {}
            _ = ctrl_shutdown.recv() => {}
        }
        Ok(())
    }

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut quit = signal(SignalKind::quit())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
            _ = quit.recv() => {}
            _ = hangup.recv() => {}
        }
        Ok(())
    }

    #[cfg(not(any(windows, unix)))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}
