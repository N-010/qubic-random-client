use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use scapi::{QubicId as ScapiQubicId, QubicWallet as ScapiQubicWallet};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::balance::{BalanceState, run_balance_watcher};
use crate::config::AppConfig;
use crate::console;
use crate::heap_prof;
use crate::pipeline::{Pipeline, PipelineEvent, run_job_dispatcher};
use crate::ticks::ScapiTickSource;
use crate::transport::{ScTransport, ScapiClient, ScapiContractTransport, ScapiRpcClient};

pub type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const DEFAULT_CONTRACT_ID: &str = "DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG";

pub async fn run(config: AppConfig) -> AppResult<()> {
    console::init();
    let AppConfig { seed, runtime } = config;
    let scapi_wallet = wallet_from_seed(seed.expose())?;
    drop(seed);
    let contract_id = default_contract_id()?;
    let identity = scapi_wallet.get_identity();

    let balance_state = Arc::new(BalanceState::new());
    let client: Arc<dyn ScapiClient> = Arc::new(ScapiRpcClient::new(runtime.endpoint.clone()));
    let transport: Arc<dyn ScTransport> = Arc::new(ScapiContractTransport::new(
        runtime.endpoint.clone(),
        scapi_wallet,
        contract_id,
        1,
        balance_state.clone(),
    ));
    run_with_components(
        runtime,
        identity,
        client,
        transport,
        balance_state,
        wait_for_shutdown_signal(),
    )
    .await
}

async fn run_with_components(
    runtime: crate::config::Config,
    identity: String,
    client: Arc<dyn ScapiClient>,
    transport: Arc<dyn ScTransport>,
    balance_state: Arc<BalanceState>,
    shutdown: impl std::future::Future<Output = AppResult<()>>,
) -> AppResult<()> {
    let (tick_tx, mut tick_rx) = mpsc::channel(64);
    let (job_tx, job_rx) = mpsc::channel(128);

    let tick_source = ScapiTickSource::new(
        client.clone(),
        Duration::from_millis(runtime.tick_poll_interval_ms),
    );
    tokio::spawn(tick_source.run(tick_tx));

    tokio::spawn(run_balance_watcher(
        client.clone(),
        identity,
        Duration::from_millis(runtime.balance_interval_ms),
        balance_state.clone(),
    ));

    let pipeline_count = runtime.commit_reveal_pipeline_count.max(1);
    let mut pipeline_txs = Vec::with_capacity(pipeline_count);
    for id in 0..pipeline_count {
        let (pipeline_tx, pipeline_rx) = mpsc::channel(64);
        let pipeline = Pipeline::new(runtime.clone(), id, balance_state.clone());
        tokio::spawn(pipeline.run(pipeline_rx, job_tx.clone()));
        pipeline_txs.push(pipeline_tx);
    }

    let forward_txs = pipeline_txs.clone();
    tokio::spawn(async move {
        let mut pipeline_txs = forward_txs;
        while let Some(tick) = tick_rx.recv().await {
            pipeline_txs.retain(|tx| !tx.is_closed());
            for tx in &pipeline_txs {
                if tx.send(PipelineEvent::Tick(tick.clone())).await.is_err() {
                    // Dropped pipelines will be cleaned up on the next tick.
                }
            }
        }
    });

    tokio::spawn(run_job_dispatcher(
        job_rx,
        transport.clone(),
        runtime.senders,
    ));

    let result = shutdown.await;
    shutdown_pipelines(&pipeline_txs, transport.clone()).await;
    console::shutdown().await;
    result
}

async fn shutdown_pipelines(
    pipeline_txs: &[mpsc::Sender<PipelineEvent>],
    transport: Arc<dyn ScTransport>,
) {
    for tx in pipeline_txs {
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await
            .is_err()
        {
            continue;
        }
        if let Ok(Some(job)) = reply_rx.await
            && let Err(err) = transport
                .send_reveal_and_commit(job.input, job.amount, job.tick)
                .await
        {
            console::log_warn(format!("shutdown RevealAndCommit failed: {err}"));
        }
    }
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

        heap_prof::on_shutdown();
        Ok(())
    }

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigquit = signal(SignalKind::quit())?;
        let mut sighup = signal(SignalKind::hangup())?;

        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
            _ = sigquit.recv() => {}
            _ = sighup.recv() => {}
        }

        heap_prof::on_shutdown();
        Ok(())
    }

    #[cfg(not(any(windows, unix)))]
    {
        tokio::signal::ctrl_c().await?;
        heap_prof::on_shutdown();
        Ok(())
    }
}

fn wallet_from_seed(seed: &str) -> AppResult<ScapiQubicWallet> {
    ScapiQubicWallet::from_seed(seed).map_err(|err| {
        console::log_warn(format!("failed to derive scapi wallet from seed: {}", err));
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into()
    })
}

fn default_contract_id() -> AppResult<ScapiQubicId> {
    ScapiQubicId::from_str(DEFAULT_CONTRACT_ID).map_err(|err| {
        console::log_warn(format!("invalid default contract id: {}", err));
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err).into()
    })
}

#[cfg(test)]
mod tests {
    use super::{run_with_components, shutdown_pipelines, wallet_from_seed};
    use crate::balance::BalanceEntry;
    use crate::pipeline::PipelineEvent;
    use crate::protocol::RevealAndCommitInput;
    use crate::ticks::TickInfo;
    use crate::transport::{ScTransport, TransportError};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    async fn with_timeout<T>(
        duration: Duration,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        timeout(duration, future).await.expect("timeout")
    }

    #[derive(Debug, Default)]
    struct MockTransport {
        calls: Mutex<Vec<(u64, u32)>>,
    }

    #[async_trait]
    impl ScTransport for MockTransport {
        async fn send_reveal_and_commit(
            &self,
            _input: RevealAndCommitInput,
            amount: u64,
            tick: u32,
        ) -> Result<String, TransportError> {
            self.calls.lock().expect("lock calls").push((amount, tick));
            Ok("tx".to_string())
        }
    }

    #[tokio::test]
    async fn shutdown_pipelines_sends_pending_reveal() {
        // Shutdown should send pending reveal job through transport.
        let (tx, mut rx) = mpsc::channel(1);
        let transport = Arc::new(MockTransport::default());

        let handle = tokio::spawn(async move {
            if let Some(PipelineEvent::Shutdown { reply }) = rx.recv().await {
                let _ = reply.send(Some(crate::pipeline::RevealCommitJob {
                    input: RevealAndCommitInput {
                        revealed_bits: [0u8; 512],
                        committed_digest: [0u8; 32],
                    },
                    amount: 0,
                    tick: 42,
                }));
            }
        });

        shutdown_pipelines(&[tx], transport.clone()).await;

        let calls = transport.calls.lock().expect("lock calls");
        assert_eq!(calls.as_slice(), &[(0, 42)]);

        handle.abort();
    }

    #[test]
    fn wallet_from_seed_rejects_invalid_seed() {
        // Invalid seed should fail wallet derivation.
        let err = wallet_from_seed("invalid").expect_err("expected error");
        assert!(!err.to_string().is_empty());
    }

    #[tokio::test]
    async fn run_with_components_starts_pipeline_and_dispatch() {
        // With a tick source and balance, at least one transport send should happen.
        #[derive(Debug, Default)]
        struct MockClient {
            tick: Mutex<u32>,
        }

        #[async_trait]
        impl crate::transport::ScapiClient for MockClient {
            async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
                let mut tick = self.tick.lock().expect("lock tick");
                *tick += 1;
                Ok(TickInfo {
                    epoch: 1,
                    tick: *tick,
                })
            }

            async fn get_balances(
                &self,
                _identity: &str,
            ) -> Result<Vec<BalanceEntry>, TransportError> {
                Ok(vec![BalanceEntry {
                    asset: "ID".to_string(),
                    amount: 100,
                }])
            }
        }

        #[derive(Debug)]
        struct NotifyTransport {
            notify: Arc<Notify>,
        }

        #[async_trait]
        impl ScTransport for NotifyTransport {
            async fn send_reveal_and_commit(
                &self,
                _input: RevealAndCommitInput,
                _amount: u64,
                _tick: u32,
            ) -> Result<String, TransportError> {
                self.notify.notify_one();
                Ok("tx".to_string())
            }
        }

        let client = Arc::new(MockClient::default());
        let notify = Arc::new(Notify::new());
        let transport = Arc::new(NotifyTransport {
            notify: notify.clone(),
        });

        let runtime = crate::config::Config {
            senders: 1,
            reveal_delay_ticks: 1,
            reveal_send_guard_ticks: 2,
            commit_amount: 1,
            commit_reveal_pipeline_count: 1,
            runtime_threads: 1,
            heap_dump: false,
            heap_stats: false,
            heap_dump_interval_secs: 0,
            tick_poll_interval_ms: 1,
            endpoint: "endpoint".to_string(),
            balance_interval_ms: 1,
        };

        let shutdown_notify = notify.clone();
        let shutdown = async move {
            with_timeout(Duration::from_millis(200), shutdown_notify.notified()).await;
            Ok(())
        };

        let balance_state = Arc::new(crate::balance::BalanceState::new());
        run_with_components(
            runtime,
            "identity".to_string(),
            client,
            transport,
            balance_state,
            shutdown,
        )
        .await
        .expect("run");
    }
}
