use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, Timelike, Utc, Weekday};
use tokio::sync::Semaphore;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::balance::BalanceState;
use crate::config::Config;
use crate::console;
use crate::entropy::{commit_digest, fill_secure_bits};
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;
use crate::transport::ScTransport;

#[cfg(test)]
static RESCHEDULE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn record_reschedule() {
    RESCHEDULE_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
fn reschedule_count() -> usize {
    RESCHEDULE_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
fn reset_reschedule_count() {
    RESCHEDULE_COUNT.store(0, Ordering::Relaxed);
}

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
    stop_window_handled_epoch: Option<u32>,
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
            stop_window_handled_epoch: None,
        }
    }

    fn id_tick_offset(&self) -> u32 {
        u32::try_from(self.id).unwrap_or(u32::MAX)
    }

    fn build_shutdown_job(&mut self) -> Option<RevealCommitJob> {
        self.pending.take().map(|pending| {
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
        })
    }

    fn is_outdated_tick(&self, tick_info: &TickInfo) -> bool {
        tick_info.tick <= self.tick_info.tick
    }

    fn reset_on_epoch_change(&mut self, tick_info: &TickInfo) {
        if self.tick_info.epoch != 0 && tick_info.epoch != self.tick_info.epoch {
            self.pending = None;
            self.stop_window_handled_epoch = None;
        }
    }

    async fn process_stop_window(
        &mut self,
        tick_info: &TickInfo,
        _job_tx: &mpsc::Sender<RevealCommitJob>,
    ) -> Result<bool, ()> {
        let in_stop_window =
            is_epoch_stop_window(Utc::now(), self.config.epoch_stop_lead_time_secs);
        if !in_stop_window {
            return Ok(false);
        }

        if self.stop_window_handled_epoch != Some(tick_info.epoch) {
            console::log_info(format!(
                "pipeline[{id}] entered stop window: epoch={epoch} tick={tick}",
                id = self.id,
                epoch = tick_info.epoch,
                tick = tick_info.tick
            ));
            self.stop_window_handled_epoch = Some(tick_info.epoch);
        }

        self.tick_info = tick_info.clone();
        Ok(true)
    }

    fn warmup_complete(&self, tick_info: &TickInfo) -> bool {
        tick_info.tick.saturating_sub(tick_info.initial_tick)
            >= self.config.epoch_resume_delay_ticks
    }

    fn has_sufficient_balance(&self) -> bool {
        let commit_amount = self.config.commit_amount;
        if commit_amount == 0 {
            return true;
        }

        let balance = self.balance_state.amount();
        if balance >= commit_amount {
            return true;
        }

        console::log_warn(format!(
            "pipeline[{id}] paused: balance={balance} < amount={commit_amount}",
            id = self.id,
            balance = balance,
            commit_amount = commit_amount
        ));
        false
    }

    async fn emit_commit_only_job(
        &mut self,
        tick_info: &TickInfo,
        job_tx: &mpsc::Sender<RevealCommitJob>,
    ) -> bool {
        let reveal_delay = self.config.reveal_delay_ticks;
        let scheduled_tick = tick_info
            .tick
            .saturating_add(reveal_delay)
            .saturating_add(self.id_tick_offset())
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
            return false;
        }

        self.pending = Some(PendingCommit {
            reveal_send_at_tick: scheduled_tick.saturating_add(reveal_delay),
            revealed_bits,
        });
        true
    }

    async fn emit_reveal_and_commit_job(
        &mut self,
        tick_info: &TickInfo,
        job_tx: &mpsc::Sender<RevealCommitJob>,
    ) -> bool {
        let id_tick_offset = self.id_tick_offset();
        let reveal_delay = self.config.reveal_delay_ticks;
        let reveal_send_guard = self.config.reveal_send_guard_ticks;
        let Some(pending) = self.pending.as_ref() else {
            return true;
        };
        let reveal_send_at_tick = pending.reveal_send_at_tick;
        let revealed_bits = pending.revealed_bits;
        let in_stop_window =
            is_epoch_stop_window(Utc::now(), self.config.epoch_stop_lead_time_secs);
        if in_stop_window {
            let reveal_job = RevealCommitJob {
                input: RevealAndCommitInput {
                    revealed_bits,
                    committed_digest: [0u8; 32],
                },
                amount: 0,
                tick: reveal_send_at_tick,
                kind: RevealCommitKind::Reveal,
                pipeline_id: self.id,
            };

            console::log_info(format!(
                "pipeline[{id}] reveal-only in stop window: reveal_tick={reveal_tick} amount=0",
                id = self.id,
                reveal_tick = reveal_send_at_tick,
            ));
            if job_tx.send(reveal_job).await.is_err() {
                return false;
            }

            self.pending = None;
            return true;
        }

        if tick_info.tick >= reveal_send_at_tick {
            let rescheduled = tick_info
                .tick
                .saturating_add(reveal_delay)
                .saturating_add(id_tick_offset)
                .saturating_add(reveal_send_guard);

            console::record_reveal_result(false);
            #[cfg(test)]
            record_reschedule();
            console::log_warn(format!(
                "pipeline[{id}] reschedule reveal: old_reveal_tick={old_reveal_tick} new_reveal_tick={new_reveal_tick}",
                id = self.id,
                old_reveal_tick = reveal_send_at_tick,
                new_reveal_tick = rescheduled
            ));
            if let Some(pending) = self.pending.as_mut() {
                pending.reveal_send_at_tick = rescheduled;
            }
            return true;
        }

        let send_window_start = reveal_send_at_tick.saturating_sub(reveal_send_guard);
        if tick_info.tick < send_window_start {
            console::log_info(format!(
                "pipeline[{id}] waiting: send_from_tick={send_from_tick} reveal_send_at_tick={reveal_send_at_tick}",
                id = self.id,
                send_from_tick = send_window_start,
                reveal_send_at_tick = reveal_send_at_tick,
            ));
            return true;
        }

        let next_reveal_tick = reveal_send_at_tick.saturating_add(reveal_delay);
        let mut next_bits = [0u8; 512];
        fill_secure_bits(&mut next_bits);
        let next_digest = commit_digest(&next_bits);
        let reveal_tick = reveal_send_at_tick;

        let reveal_job = RevealCommitJob {
            input: RevealAndCommitInput {
                revealed_bits,
                committed_digest: next_digest,
            },
            amount: self.config.commit_amount,
            tick: reveal_tick,
            kind: RevealCommitKind::Reveal,
            pipeline_id: self.id,
        };

        console::log_info(format!(
            "pipeline[{id}] reveal+commit: next_reveal_tick={next_reveal_tick} amount={amount}",
            id = self.id,
            amount = self.config.commit_amount,
        ));
        if job_tx.send(reveal_job).await.is_err() {
            return false;
        }

        self.pending = Some(PendingCommit {
            reveal_send_at_tick: next_reveal_tick,
            revealed_bits: next_bits,
        });
        true
    }

    async fn process_tick(
        &mut self,
        tick_info: TickInfo,
        job_tx: &mpsc::Sender<RevealCommitJob>,
    ) -> bool {
        if self.is_outdated_tick(&tick_info) {
            return true;
        }

        self.reset_on_epoch_change(&tick_info);

        let in_stop_window = match self.process_stop_window(&tick_info, job_tx).await {
            Ok(in_stop_window) => in_stop_window,
            Err(()) => return false,
        };
        if in_stop_window {
            if self.pending.is_some() {
                return self.emit_reveal_and_commit_job(&tick_info, job_tx).await;
            }
            return true;
        }

        if !self.warmup_complete(&tick_info) {
            let id = self.id;
            let current_tick = tick_info.tick;
            let initial_tick = tick_info.initial_tick;
            let warmup_required = self.config.epoch_resume_delay_ticks;
            let warmup_elapsed = current_tick.saturating_sub(initial_tick);
            let warmup_remaining = warmup_required.saturating_sub(warmup_elapsed);
            console::log_info(format!(
                "pipeline[{id}] warmup: elapsed={warmup_elapsed}/{warmup_required} remaining={warmup_remaining} current_tick={current_tick} initial_tick={initial_tick}"
            ));
            self.tick_info = tick_info;
            return true;
        }

        if !self.has_sufficient_balance() {
            return true;
        }

        self.tick_info = tick_info.clone();
        if self.pending.is_none() {
            return self.emit_commit_only_job(&tick_info, job_tx).await;
        }

        self.emit_reveal_and_commit_job(&tick_info, job_tx).await
    }

    pub async fn run(
        mut self,
        mut tick_rx: broadcast::Receiver<TickInfo>,
        mut control_rx: mpsc::Receiver<PipelineEvent>,
        job_tx: mpsc::Sender<RevealCommitJob>,
    ) {
        loop {
            tokio::select! {
                biased;
                event = control_rx.recv() => {
                    let Some(PipelineEvent::Shutdown { reply }) = event else {
                        break;
                    };
                    let shutdown_job = self.build_shutdown_job();
                    let _ = reply.send(shutdown_job);
                    break;
                }
                event = tick_rx.recv() => {
                    let tick_info = match event {
                        Ok(tick) => tick,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    if !self.process_tick(tick_info, &job_tx).await {
                        break;
                    }
                }
            }
        }
    }
}

fn is_epoch_stop_window(now: chrono::DateTime<Utc>, stop_lead_time_secs: u64) -> bool {
    if now.weekday() != Weekday::Wed {
        return false;
    }

    let seconds_from_midnight = now.num_seconds_from_midnight();
    let epoch_end_seconds: u32 = 12 * 60 * 60;
    let stop_lead_time_secs = u32::try_from(stop_lead_time_secs).unwrap_or(u32::MAX);
    let stop_start_seconds = epoch_end_seconds.saturating_sub(stop_lead_time_secs);

    (stop_start_seconds..epoch_end_seconds).contains(&seconds_from_midnight)
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
    let mut tasks = JoinSet::new();

    while let Some(job) = job_rx.recv().await {
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let transport = transport.clone();
        let current_tick = current_tick.clone();
        let tick_data_tx = tick_data_tx.clone();
        tasks.spawn(async move {
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

    while let Some(result) = tasks.join_next().await {
        if let Err(err) = result {
            console::log_warn(format!("job sender task failed: {err}"));
        }
    }
}

fn is_broadcast_error(err: &crate::transport::TransportError) -> bool {
    err.message.contains("broadcast transaction failed")
}

#[cfg(test)]
mod tests {
    use super::{
        Pipeline, PipelineEvent, RevealCommitJob, RevealCommitKind, current_tick_store,
        is_epoch_stop_window, reschedule_count, reset_reschedule_count, run_job_dispatcher,
    };
    use crate::balance::BalanceState;
    use crate::config::Config;
    use crate::console;
    use crate::protocol::RevealAndCommitInput;
    use crate::transport::{ScTransport, TransportError};
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
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
            tick_data_min_delay_ticks: 1,
            epoch_stop_lead_time_secs: 600,
            epoch_resume_delay_ticks: 0,
            use_bob: false,
            bob_endpoint: "bob".to_string(),
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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

    #[test]
    fn stop_window_matches_wednesday_noon_utc() {
        let in_window = Utc
            .with_ymd_and_hms(2026, 2, 4, 11, 55, 0)
            .single()
            .expect("timestamp");
        let out_window = Utc
            .with_ymd_and_hms(2026, 2, 4, 12, 1, 0)
            .single()
            .expect("timestamp");
        let wrong_day = Utc
            .with_ymd_and_hms(2026, 2, 3, 11, 55, 0)
            .single()
            .expect("timestamp");

        assert!(is_epoch_stop_window(in_window, 600));
        assert!(!is_epoch_stop_window(out_window, 600));
        assert!(!is_epoch_stop_window(wrong_day, 600));
    }

    #[tokio::test]
    async fn pipeline_waits_for_warmup_ticks() {
        let mut config = test_config();
        config.epoch_stop_lead_time_secs = 0;
        config.epoch_resume_delay_ticks = 50;
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config, 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 20,
                initial_tick: 1,
            })
            .expect("send tick");
        let no_job = timeout(Duration::from_millis(20), job_rx.recv()).await;
        assert!(no_job.is_err());

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 51,
                initial_tick: 1,
            })
            .expect("send tick");
        let job = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");
        assert_eq!(job.kind, RevealCommitKind::CommitOnly);

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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 16,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("job");

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 9,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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
        console::init();
        console::reset_reveal_stats();
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        // reveal_send_at_tick = 19 for pipeline id 1 (base offset 4).
        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 20,
                initial_tick: 1,
            })
            .expect("send tick");
        let no_job = timeout(Duration::from_millis(20), job_rx.recv()).await;
        assert!(no_job.is_err());

        // Rescheduled reveal tick should be 26; send tick within guard window.
        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 24,
                initial_tick: 1,
            })
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

    #[tokio::test]
    async fn reveal_enqueue_on_success() {
        // Successful reveal should enqueue tick for tick-data watcher.
        console::init();
        console::reset_reveal_stats();
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 5);
        let (job_tx, job_rx) = mpsc::channel(4);
        let (tick_data_tx, mut tick_data_rx) = mpsc::channel(4);

        #[derive(Debug, Default)]
        struct OkTransport;

        #[async_trait]
        impl ScTransport for OkTransport {
            async fn send_reveal_and_commit(
                &self,
                _input: RevealAndCommitInput,
                _amount: u64,
                _tick: u32,
                _pipeline_id: usize,
            ) -> Result<String, TransportError> {
                Ok("tx".to_string())
            }
        }

        let transport = Arc::new(OkTransport);
        let dispatcher = tokio::spawn(run_job_dispatcher(
            job_rx,
            transport,
            1,
            current_tick.clone(),
            tick_data_tx,
        ));

        let job = RevealCommitJob {
            input: RevealAndCommitInput {
                revealed_bits: [0u8; 512],
                committed_digest: [0u8; 32],
            },
            amount: 1,
            tick: 5,
            kind: RevealCommitKind::Reveal,
            pipeline_id: 0,
        };
        job_tx.send(job).await.expect("send job");

        let queued = with_timeout(Duration::from_millis(200), tick_data_rx.recv())
            .await
            .expect("queued tick");
        assert_eq!(queued, 5);

        drop(job_tx);
        let _ = dispatcher.await;
    }

    #[tokio::test]
    async fn reschedule_increments_fail_count() {
        console::init();
        let _guard = console::test_stats_guard();
        console::reset_reveal_stats();
        reset_reschedule_count();
        let mut config = test_config();
        config.reveal_delay_ticks = 3;
        config.reveal_send_guard_ticks = 2;
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let pipeline = Pipeline::new(config.clone(), 0, balance_state);
        let (tick_tx, _) = broadcast::channel(4);
        let tick_rx = tick_tx.subscribe();
        let (pipeline_tx, pipeline_rx) = mpsc::channel(4);
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let handle = tokio::spawn(pipeline.run(tick_rx, pipeline_rx, job_tx));

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
            .expect("send tick");
        let _ = with_timeout(Duration::from_millis(200), job_rx.recv())
            .await
            .expect("commit job");

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 20,
                initial_tick: 1,
            })
            .expect("send tick");

        let initial_reschedule = reschedule_count();
        with_timeout(Duration::from_millis(200), async {
            loop {
                let reschedules = reschedule_count();
                if reschedules >= initial_reschedule.saturating_add(1) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;

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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
            .expect("send tick");
        tokio::time::sleep(Duration::from_millis(10)).await;

        tick_tx
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 16,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 10,
                initial_tick: 1,
            })
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
            .send(crate::ticks::TickInfo {
                epoch: 1,
                tick: 11,
                initial_tick: 1,
            })
            .expect("send tick");
        tokio::time::sleep(Duration::from_millis(10)).await;

        drop(pipeline_tx);
        pipeline_handle.abort();
        dispatcher.abort();

        let calls = transport.calls.lock().expect("lock calls").clone();
        assert!(!calls.is_empty());
    }
}
