use scapi::bob::BobRpcClient;
use scapi::{QubicId as ScapiQubicId, QubicWallet as ScapiQubicWallet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::balance::{BalanceState, run_balance_watcher};
use crate::config::{AppConfig, Backend};
use crate::console;
use crate::pipeline::{Pipeline, PipelineEvent, run_job_dispatcher};
use crate::qln::{QlnGrpcClient, QlnScapiClient, QlnTickDataFetcher};
use crate::tick_data_watcher::{BobTickDataFetcher, TickDataFetcher, TickDataWatcher};
use crate::ticks::ScapiTickSource;
use crate::transport::{
    BobContractTransport, BobScapiClient, ScTransport, ScapiClient, ScapiContractTransport,
    ScapiRpcClient,
};

pub type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const DEFAULT_CONTRACT_ID: &str = "DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG";

pub async fn run(config: AppConfig) -> AppResult<()> {
    console::init();
    let AppConfig { seed, runtime } = config;
    let backend_name = match runtime.backend {
        Backend::Rpc => "rpc",
        Backend::Bob => "bob",
        Backend::QlnGrpc => "grpc",
    };
    console::set_backend(backend_name);
    console::log_info(format!("backend selected: {backend_name}"));
    let scapi_wallet = wallet_from_seed(seed.expose())?;
    drop(seed);
    let contract_id = default_contract_id()?;
    let identity = scapi_wallet.get_identity();

    let balance_state = Arc::new(BalanceState::new());
    let (client, transport, tick_data_fetcher): (
        Arc<dyn ScapiClient>,
        Arc<dyn ScTransport>,
        Arc<dyn TickDataFetcher>,
    ) = match runtime.backend {
        Backend::Bob => {
            let bob_rpc = Arc::new(BobRpcClient::with_base_url(&runtime.bob_endpoint));
            (
                Arc::new(BobScapiClient::new(bob_rpc.clone())),
                Arc::new(BobContractTransport::new(
                    bob_rpc.clone(),
                    scapi_wallet,
                    contract_id,
                    1,
                    balance_state.clone(),
                )),
                Arc::new(BobTickDataFetcher::new(bob_rpc)),
            )
        }
        Backend::QlnGrpc => {
            let qln = Arc::new(QlnGrpcClient::new(&runtime.grpc_endpoint).map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("failed to create qln grpc client: {err}"),
                )
            })?);
            (
                Arc::new(QlnScapiClient::new(qln.clone())),
                Arc::new(ScapiContractTransport::new(
                    runtime.endpoint.clone(),
                    scapi_wallet,
                    contract_id,
                    1,
                    balance_state.clone(),
                )),
                Arc::new(QlnTickDataFetcher::new(qln)),
            )
        }
        Backend::Rpc => (
            Arc::new(ScapiRpcClient::new(runtime.endpoint.clone())),
            Arc::new(ScapiContractTransport::new(
                runtime.endpoint.clone(),
                scapi_wallet,
                contract_id,
                1,
                balance_state.clone(),
            )),
            Arc::new(crate::tick_data_watcher::ScapiTickDataFetcher::new(
                runtime.endpoint.clone(),
            )),
        ),
    };
    run_with_components(
        runtime,
        identity,
        client,
        transport,
        tick_data_fetcher,
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
    tick_data_fetcher: Arc<dyn TickDataFetcher>,
    balance_state: Arc<BalanceState>,
    shutdown: impl std::future::Future<Output = AppResult<()>>,
) -> AppResult<()> {
    let (tick_tx, _tick_rx) = broadcast::channel(64);
    let (job_tx, job_rx) = mpsc::channel(128);
    let (tick_data_tx, tick_data_rx) = mpsc::channel(256);
    let current_tick = Arc::new(AtomicU64::new(0));

    let tick_source = ScapiTickSource::new(
        client.clone(),
        Duration::from_millis(runtime.tick_poll),
        current_tick.clone(),
    );
    tokio::spawn(tick_source.run(tick_tx.clone()));

    tokio::spawn(run_balance_watcher(
        client.clone(),
        identity,
        Duration::from_millis(runtime.balance_interval_ms),
        balance_state.clone(),
    ));

    let pipeline_count = runtime.pipeline_count.max(1);
    let mut pipeline_txs = Vec::with_capacity(pipeline_count);
    for id in 0..pipeline_count {
        let (pipeline_tx, pipeline_rx) = mpsc::channel(64);
        let tick_rx = tick_tx.subscribe();
        let pipeline = Pipeline::new(runtime.clone(), id, balance_state.clone());
        tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx.clone()));
        pipeline_txs.push(pipeline_tx);
    }

    let dispatcher_handle = tokio::spawn(run_job_dispatcher(
        job_rx,
        transport.clone(),
        runtime.max_inflight_sends,
        current_tick.clone(),
        tick_data_tx,
    ));
    tokio::spawn(
        TickDataWatcher::new_with_fetcher(
            current_tick.clone(),
            tick_data_rx,
            runtime.empty_tick_check_interval_ms,
            runtime.reveal_check_delay_ticks,
            tick_data_fetcher,
        )
        .run(),
    );

    let result = shutdown.await;
    shutdown_pipelines(&pipeline_txs, transport.clone()).await;
    drop(job_tx);
    if let Err(err) = dispatcher_handle.await {
        console::log_warn(format!("job dispatcher task failed: {err}"));
    }
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
                .send_reveal_and_commit(job.input, job.amount, job.tick, job.pipeline_id)
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

        Ok(())
    }

    #[cfg(not(any(windows, unix)))]
    {
        tokio::signal::ctrl_c().await?;
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
    use crate::tick_data_watcher::TickDataFetcher;
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
            _pipeline_id: usize,
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
                    kind: crate::pipeline::RevealCommitKind::Reveal,
                    pipeline_id: 0,
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
                    initial_tick: 1,
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
                _pipeline_id: usize,
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
        #[derive(Debug)]
        struct MockTickDataFetcher;

        #[async_trait]
        impl TickDataFetcher for MockTickDataFetcher {
            async fn fetch(&self, _tick: u32) -> Result<Option<()>, String> {
                Ok(Some(()))
            }
        }

        let runtime = crate::config::Config {
            max_inflight_sends: 1,
            reveal_delay_ticks: 1,
            reveal_window_ticks: 2,
            commit_amount: 1,
            pipeline_count: 1,
            worker_threads: 1,
            tick_poll: 1,
            endpoint: "endpoint".to_string(),
            backend: crate::config::Backend::Rpc,
            balance_interval_ms: 1,
            empty_tick_check_interval_ms: 1,
            reveal_check_delay_ticks: 1,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 0,
            bob_endpoint: "bob".to_string(),
            grpc_endpoint: "http://127.0.0.1:50051".to_string(),
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
            Arc::new(MockTickDataFetcher),
            balance_state,
            shutdown,
        )
        .await
        .expect("run");
    }
}
