use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Semaphore;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::balance::BalanceState;
use crate::config::Config;
use crate::console;
use crate::entropy::{commit_digest, fill_secure_bits};
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;
use crate::transport::ScTransport;

#[derive(Debug)]
pub enum PipelineEvent {
    Shutdown {
        reply: oneshot::Sender<Option<RevealCommitJob>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealCommitKind {
    CommitOnly,
    Reveal,
}

#[derive(Debug)]
pub struct RevealCommitJob {
    pub input: RevealAndCommitInput,
    pub amount: u64,
    pub tick: u32,
    pub kind: RevealCommitKind,
    pub pipeline_id: usize,
}

pub type CurrentTick = Arc<AtomicU64>;

pub fn current_tick_store(current_tick: &AtomicU64, tick: u32) {
    current_tick.store(u64::from(tick).saturating_add(1), Ordering::Relaxed);
}

pub(crate) fn current_tick_load(current_tick: &AtomicU64) -> Option<u32> {
    let stored = current_tick.load(Ordering::Relaxed);
    if stored == 0 {
        return None;
    }
    u32::try_from(stored.saturating_sub(1)).ok()
}

pub struct Pipeline {
    config: Config,
    pending: Option<PendingCommit>,
    tick_info: TickInfo,
    id: usize,
    balance_state: Arc<BalanceState>,
}

#[derive(Debug, Clone)]
struct PendingCommit {
    reveal_send_at_tick: u32,
    revealed_bits: [u8; 512],
}

impl Pipeline {
    pub fn new(config: Config, id: usize, balance_state: Arc<BalanceState>) -> Self {
        Self {
            config,
            pending: None,
            tick_info: Default::default(),
            id,
            balance_state,
        }
    }

    fn id_tick_offset(&self) -> u32 {
        u32::try_from(self.id).unwrap_or(u32::MAX)
    }

    pub async fn run(
        mut self,
        mut tick_rx: broadcast::Receiver<TickInfo>,
        mut control_rx: mpsc::Receiver<PipelineEvent>,
        job_tx: mpsc::Sender<RevealCommitJob>,
    ) {
        loop {
            tokio::select! {
                event = control_rx.recv() => {
                    let Some(PipelineEvent::Shutdown { reply }) = event else {
                        break;
                    };
                    let shutdown_job = self.pending.take().map(|pending| {
                        let current_tick = self
                            .tick_info
                            .tick
                            .saturating_add(self.config.reveal_delay_ticks)
                            .saturating_add(self.id_tick_offset());
                        let reveal_tick = current_tick.max(pending.reveal_send_at_tick);
                        let committed_digest = commit_digest(&pending.revealed_bits);
                        let reveal_input = RevealAndCommitInput {
                            revealed_bits: pending.revealed_bits,
                            committed_digest,
                        };
                        let reveal_job = RevealCommitJob {
                            input: reveal_input,
                            amount: 0,
                            tick: reveal_tick,
                            kind: RevealCommitKind::Reveal,
                            pipeline_id: self.id,
                        };

                        console::log_info(format!(
                            "pipeline[{id}] shutdown reveal: now_tick={now_tick} reveal_tick={reveal_tick} amount=0",
                            id = self.id,
                            now_tick = self.tick_info.tick,
                        ));
                        reveal_job
                    });
                    let _ = reply.send(shutdown_job);
                    break;
                }
                event = tick_rx.recv() => {
                    let tick_info = match event {
                        Ok(tick) => tick,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    let reveal_delay = self.config.reveal_delay_ticks;
                    let id_tick_offset = self.id_tick_offset();

                    if tick_info.tick <= self.tick_info.tick {
                        continue;
                    }

                    let commit_amount = self.config.commit_amount;
                    if commit_amount > 0 {
                        let balance = self.balance_state.amount();
                        if balance < commit_amount {
                            console::log_warn(format!(
                                "pipeline[{id}] paused: balance={balance} < amount={commit_amount}",
                                id = self.id,
                                balance = balance,
                                commit_amount = commit_amount
                            ));

                            continue;
                        }
                    }

                    self.tick_info = tick_info.clone();

                    match &mut self.pending {
                        None => {
                            let scheduled_tick = tick_info
                                .tick
                                .saturating_add(reveal_delay)
                                .saturating_add(id_tick_offset)
                                .saturating_add(self.config.reveal_send_guard_ticks);
                            let mut revealed_bits = [0u8; 512];
                            fill_secure_bits(&mut revealed_bits);
                            let committed_digest = commit_digest(&revealed_bits);

                            let commit_input = RevealAndCommitInput {
                                revealed_bits: [0u8; 512],
                                committed_digest,
                            };
                            let commit_job = RevealCommitJob {
                                input: commit_input,
                                amount: self.config.commit_amount,
                                tick: scheduled_tick,
                                kind: RevealCommitKind::CommitOnly,
                                pipeline_id: self.id,
                            };

                            console::log_info(format!(
                                "pipeline[{id}] commit-only: commit_tick={commit_tick} amount={amount}",
                                id = self.id,
                                commit_tick = scheduled_tick,
                                amount = self.config.commit_amount,
                            ));
                            if job_tx.send(commit_job).await.is_err() {
                                break;
                            }

                            self.pending = Some(PendingCommit {
                                reveal_send_at_tick: scheduled_tick.saturating_add(reveal_delay),
                                revealed_bits,
                            });
                        }
                        Some(pending) => {
                            let reveal_send_guard = self.config.reveal_send_guard_ticks;
                            if tick_info.tick >= pending.reveal_send_at_tick {
                                let rescheduled = tick_info
                                    .tick
                                    .saturating_add(reveal_delay)
                                    .saturating_add(id_tick_offset)
                                    .saturating_add(reveal_send_guard);

                                console::record_reveal_result(false);
                                console::log_warn(format!(
                                    "pipeline[{id}] reschedule reveal: old_reveal_tick={old_reveal_tick} new_reveal_tick={new_reveal_tick}",
                                    id = self.id,
                                    old_reveal_tick = pending.reveal_send_at_tick,
                                    new_reveal_tick = rescheduled
                                ));
                                pending.reveal_send_at_tick = rescheduled;
                                continue;
                            }
                            let send_window_start = pending
                                .reveal_send_at_tick
                                .saturating_sub(reveal_send_guard);
                            if tick_info.tick < send_window_start {
                                console::log_info(format!(
                                    "pipeline[{id}] waiting: send_from_tick={send_from_tick} reveal_send_at_tick={reveal_send_at_tick}",
                                    id = self.id,
                                    send_from_tick = send_window_start,
                                    reveal_send_at_tick = pending.reveal_send_at_tick,
                                ));
                                continue;
                            }

                            let next_reveal_tick =
                                pending.reveal_send_at_tick.saturating_add(reveal_delay);
                            let mut next_bits = [0u8; 512];
                            fill_secure_bits(&mut next_bits);
                            let next_digest = commit_digest(&next_bits);

                            let reveal_input = RevealAndCommitInput {
                                revealed_bits: pending.revealed_bits,
                                committed_digest: next_digest,
                            };
                            let reveal_job = RevealCommitJob {
                                input: reveal_input,
                                amount: self.config.commit_amount,
                                tick: pending.reveal_send_at_tick,
                                kind: RevealCommitKind::Reveal,
                                pipeline_id: self.id,
                            };

                            console::log_info(format!(
                                "pipeline[{id}] reveal+commit: next_reveal_tick={next_reveal_tick} amount={amount}",
                                id = self.id,
                                amount = self.config.commit_amount,
                            ));
                            if job_tx.send(reveal_job).await.is_err() {
                                break;
                            }

                            self.pending = Some(PendingCommit {
                                reveal_send_at_tick: next_reveal_tick,
                                revealed_bits: next_bits,
                            });
                        }
                    }
                }
            }
        }
    }
}

pub async fn run_job_dispatcher(
    mut job_rx: mpsc::Receiver<RevealCommitJob>,
    transport: Arc<dyn ScTransport>,
    senders: usize,
    current_tick: CurrentTick,
    tick_data_tx: mpsc::Sender<u32>,
) {
    let senders = senders.max(1);
    let semaphore = Arc::new(Semaphore::new(senders));

    while let Some(job) = job_rx.recv().await {
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let transport = transport.clone();
        let current_tick = current_tick.clone();
        let tick_data_tx = tick_data_tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let is_reveal = job.kind == RevealCommitKind::Reveal;
            let late_tick =
                current_tick_load(&current_tick).is_some_and(|current| current > job.tick);
            if late_tick {
                if is_reveal {
                    console::record_reveal_result(false);
                }
                console::log_warn(format!(
                    "skip RevealAndCommit: current_tick > job_tick (job_tick={})",
                    job.tick
                ));
                return;
            }

            let result = transport
                .send_reveal_and_commit(job.input, job.amount, job.tick, job.pipeline_id)
                .await;

            match result {
                Ok(_) => {
                    if is_reveal {
                        console::record_reveal_result(true);
                        let _ = tick_data_tx.send(job.tick).await;
                    }
                }
                Err(err) => {
                    if is_reveal && is_broadcast_error(&err) {
                        console::record_reveal_result(false);
                    }
                    console::log_warn(format!("send RevealAndCommit failed: {err}"));
                }
            }
        });
    }
}

fn is_broadcast_error(err: &crate::transport::TransportError) -> bool {
    err.message.contains("broadcast transaction failed")
}

#[cfg(test)]
mod tests {
    use super::{Pipeline, PipelineEvent, RevealCommitJob, RevealCommitKind, run_job_dispatcher};
    use crate::balance::BalanceState;
    use crate::config::Config;
    use crate::protocol::RevealAndCommitInput;
    use crate::transport::{ScTransport, TransportError};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;
    use tokio::sync::broadcast;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::time::{sleep, timeout};

    async fn with_timeout<T>(
        duration: Duration,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        timeout(duration, future).await.expect("timeout")
    }

    fn test_config() -> Config {
        Config {
            senders: 1,
            reveal_delay_ticks: 3,
            reveal_send_guard_ticks: 2,
            commit_amount: 10,
            commit_reveal_pipeline_count: 1,
            runtime_threads: 1,
            heap_dump: false,
            heap_stats: false,
            heap_dump_interval_secs: 0,
            tick_poll_interval_ms: 1,
            endpoint: "http://localhost".to_string(),
            balance_interval_ms: 1,
            tick_data_check_interval_ms: 1,
        }
    }

    #[tokio::test]
    async fn pipeline_emits_commit_only_job() {
        // First tick creates a commit-only job with empty reveal.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");

        assert_eq!(job.amount, config.commit_amount);
        assert_eq!(job.tick, 15);
        assert!(job.input.revealed_bits.iter().all(|b| *b == 0));
        assert!(job.input.committed_digest.iter().any(|b| *b != 0));

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_emits_reveal_and_commit_job() {
        // Second relevant tick reveals previous bits and commits new ones.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 16 })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("reveal job");

        assert_eq!(job.amount, config.commit_amount);
        assert_eq!(job.tick, 18);
        assert!(job.input.committed_digest.iter().any(|b| *b != 0));

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_pauses_on_low_balance() {
        // If balance < amount, no job should be emitted.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(0);
        let pipeline = Pipeline::new(config, 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let mut got_job = false;
        tokio::select! {
            _ = sleep(Duration::from_millis(20)) => {}
            _ = async { job_rx.recv().await } => {
                got_job = true;
            }
        }
        assert!(!got_job);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_shutdown_returns_reveal_job() {
        // Shutdown returns a pending reveal with amount=0.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        let (reply_tx, reply_rx) = oneshot::channel();
        pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await
            .expect("send shutdown");
        let job = reply_rx.await.expect("reply");
        let job = job.expect("shutdown job");

        assert_eq!(job.amount, 0);
        assert_eq!(job.tick, 18);

        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_ignores_old_ticks() {
        // Ticks older than the last seen should be ignored.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 9 })
            .expect("send old tick");
        let mut got_job = false;
        tokio::select! {
            _ = sleep(Duration::from_millis(20)) => {}
            _ = async { job_rx.recv().await } => {
                got_job = true;
            }
        }
        assert!(!got_job);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_skips_balance_check_when_amount_zero() {
        // commit_amount=0 disables balance checks and still emits a job.
        let mut config = test_config();
        config.commit_amount = 0;
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(0);
        let pipeline = Pipeline::new(config, 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");
        assert_eq!(job.amount, 0);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_offset_includes_pipeline_id() {
        // base_tick_offset = pipeline id.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 2, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");

        assert_eq!(job.tick, 17);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_shutdown_without_pending_returns_none() {
        // If no pending commit exists, shutdown returns None.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        let pipeline = Pipeline::new(config, 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, _job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        let (reply_tx, reply_rx) = oneshot::channel();
        pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await
            .expect("shutdown");
        let job = reply_rx.await.expect("reply");
        assert!(job.is_none());
        handle.abort();
    }

    #[tokio::test]
    async fn pipeline_reschedules_when_reveal_tick_passed() {
        // If current tick surpasses reveal_send_at_tick, reschedule based on base offset + guard.
        let mut config = test_config();
        config.reveal_delay_ticks = 3;
        config.reveal_send_guard_ticks = 2;
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 1, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        // reveal_send_at_tick = 19 for pipeline id 1 (base offset 4).
        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 20 })
            .expect("send tick");
        let no_job = timeout(Duration::from_millis(20), job_rx.recv()).await;
        assert!(no_job.is_err());

        // Rescheduled reveal tick should be 26; send tick within guard window.
        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 24 })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("reveal job");
        assert_eq!(job.tick, 26);
        assert_eq!(job.kind, RevealCommitKind::Reveal);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = pipeline_tx
            .send(PipelineEvent::Shutdown { reply: reply_tx })
            .await;
        handle.abort();
    }

    #[derive(Debug)]
    struct MockTransport {
        active: AtomicUsize,
        max_active: AtomicUsize,
        hold: Arc<Semaphore>,
        started_tx: mpsc::Sender<()>,
    }

    impl MockTransport {
        fn new(hold: Arc<Semaphore>, started_tx: mpsc::Sender<()>) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                hold,
                started_tx,
            }
        }

        fn max_active(&self) -> usize {
            self.max_active.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl ScTransport for MockTransport {
        async fn send_reveal_and_commit(
            &self,
            _input: RevealAndCommitInput,
            _amount: u64,
            _tick: u32,
            _pipeline_id: usize,
        ) -> Result<String, TransportError> {
            let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
            self.max_active.fetch_max(active, Ordering::Relaxed);
            let _ = self.started_tx.send(()).await;
            let _permit = self.hold.acquire().await.expect("permit");
            self.active.fetch_sub(1, Ordering::Relaxed);
            Ok("tx".to_string())
        }
    }

    #[tokio::test]
    async fn job_dispatcher_respects_sender_limit() {
        // Semaphore enforces max in-flight sends.
        let (job_tx, job_rx) = mpsc::channel(4);
        let (started_tx, mut started_rx) = mpsc::channel(4);
        let hold = Arc::new(Semaphore::new(0));
        let transport = Arc::new(MockTransport::new(hold.clone(), started_tx));
        let current_tick = Arc::new(AtomicU64::new(0));

        let (tick_data_tx, _tick_data_rx) = mpsc::channel(4);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport.clone(),
            1,
            current_tick,
            tick_data_tx,
        ));

        for tick in [1u32, 2u32] {
            let job = RevealCommitJob {
                input: RevealAndCommitInput {
                    revealed_bits: [0u8; 512],
                    committed_digest: [0u8; 32],
                },
                amount: 1,
                tick,
                kind: RevealCommitKind::CommitOnly,
                pipeline_id: 0,
            };
            job_tx.send(job).await.expect("send job");
        }

        with_timeout(Duration::from_millis(200), started_rx.recv())
            .await
            .expect("first start");

        let mut got_second = false;
        tokio::select! {
            _ = sleep(Duration::from_millis(20)) => {}
            _ = async { started_rx.recv().await } => {
                got_second = true;
            }
        }
        assert!(!got_second);

        hold.add_permits(1);

        with_timeout(Duration::from_millis(200), started_rx.recv())
            .await
            .expect("second start");

        hold.add_permits(1);
        drop(job_tx);
        let _ = dispatcher.await;

        assert_eq!(transport.max_active(), 1);
    }

    #[tokio::test]
    async fn job_dispatcher_defaults_to_one_sender() {
        // senders=0 should behave like senders=1.
        let (job_tx, job_rx) = mpsc::channel(4);
        let (started_tx, mut started_rx) = mpsc::channel(4);
        let hold = Arc::new(Semaphore::new(0));
        let transport = Arc::new(MockTransport::new(hold.clone(), started_tx));
        let current_tick = Arc::new(AtomicU64::new(0));

        let (tick_data_tx, _tick_data_rx) = mpsc::channel(4);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport.clone(),
            0,
            current_tick,
            tick_data_tx,
        ));

        let job = RevealCommitJob {
            input: RevealAndCommitInput {
                revealed_bits: [0u8; 512],
                committed_digest: [0u8; 32],
            },
            amount: 1,
            tick: 1,
            kind: RevealCommitKind::CommitOnly,
            pipeline_id: 0,
        };
        job_tx.send(job).await.expect("send job");

        with_timeout(Duration::from_millis(200), started_rx.recv())
            .await
            .expect("start");

        hold.add_permits(1);
        drop(job_tx);
        let _ = dispatcher.await;

        assert_eq!(transport.max_active(), 1);
    }

    #[tokio::test]
    async fn job_dispatcher_continues_after_error() {
        // Errors from transport shouldn't stop processing remaining jobs.
        #[derive(Debug, Default)]
        struct ErrorTransport {
            started: AtomicUsize,
        }

        #[async_trait]
        impl ScTransport for ErrorTransport {
            async fn send_reveal_and_commit(
                &self,
                _input: RevealAndCommitInput,
                _amount: u64,
                _tick: u32,
                _pipeline_id: usize,
            ) -> Result<String, TransportError> {
                self.started.fetch_add(1, Ordering::Relaxed);
                Err(TransportError {
                    message: "boom".to_string(),
                })
            }
        }

        let (job_tx, job_rx) = mpsc::channel(4);
        let transport = Arc::new(ErrorTransport::default());
        let current_tick = Arc::new(AtomicU64::new(0));
        let (tick_data_tx, _tick_data_rx) = mpsc::channel(4);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport.clone(),
            1,
            current_tick,
            tick_data_tx,
        ));

        for tick in [1u32, 2u32] {
            let job = RevealCommitJob {
                input: RevealAndCommitInput {
                    revealed_bits: [0u8; 512],
                    committed_digest: [0u8; 32],
                },
                amount: 1,
                tick,
                kind: RevealCommitKind::CommitOnly,
                pipeline_id: 0,
            };
            job_tx.send(job).await.expect("send job");
        }
        drop(job_tx);
        let _ = dispatcher.await;

        assert_eq!(transport.started.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn e2e_commit_then_reveal_cycle() {
        // Full cycle: commit-only then reveal+commit jobs reach transport.
        let config = test_config();
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);

        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(8);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(8);
        let (job_tx, job_rx) = mpsc::channel(8);

        #[derive(Debug, Default)]
        struct RecordingTransport {
            calls: std::sync::Mutex<Vec<(u64, u32)>>,
        }

        #[async_trait]
        impl ScTransport for RecordingTransport {
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

        let transport = Arc::new(RecordingTransport::default());
        let current_tick = Arc::new(AtomicU64::new(0));
        let (tick_data_tx, _tick_data_rx) = mpsc::channel(4);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport.clone(),
            1,
            current_tick,
            tick_data_tx,
        ));
        let pipeline_handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        tokio::time::sleep(Duration::from_millis(10)).await;

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 16 })
            .expect("send tick");
        tokio::time::sleep(Duration::from_millis(10)).await;

        drop(pipeline_tx);
        pipeline_handle.abort();
        dispatcher.abort();

        let calls = transport.calls.lock().expect("lock calls").clone();
        assert!(calls.contains(&(10, 15)));
        assert!(calls.contains(&(10, 18)));
    }

    #[tokio::test]
    async fn e2e_pause_and_resume_on_balance() {
        // Low balance pauses, then resume when balance increases.
        let mut config = test_config();
        config.commit_amount = 10;
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(0);

        let pipeline = Pipeline::new(config, 0, balance_state.clone());
        let (tick_tx, _) = broadcast::channel(8);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(8);
        let (job_tx, job_rx) = mpsc::channel(8);

        #[derive(Debug, Default)]
        struct RecordingTransport {
            calls: std::sync::Mutex<Vec<(u64, u32)>>,
        }

        #[async_trait]
        impl ScTransport for RecordingTransport {
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

        let transport = Arc::new(RecordingTransport::default());
        let current_tick = Arc::new(AtomicU64::new(0));
        let (tick_data_tx, _tick_data_rx) = mpsc::channel(4);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport.clone(),
            1,
            current_tick,
            tick_data_tx,
        ));
        let pipeline_handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 10 })
            .expect("send tick");
        let no_job = timeout(Duration::from_millis(20), async {
            loop {
                if !transport.calls.lock().expect("lock calls").is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(no_job.is_err());

        balance_state.set_amount(20);
        tick_tx
            .send(crate::ticks::TickInfo { epoch: 1, tick: 11 })
            .expect("send tick");
        tokio::time::sleep(Duration::from_millis(10)).await;

        drop(pipeline_tx);
        pipeline_handle.abort();
        dispatcher.abort();

        let calls = transport.calls.lock().expect("lock calls").clone();
        assert!(!calls.is_empty());
    }
}
