use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::sleep;

use crate::config::Config;
use crate::console;
use crate::entropy::{commit_digest, fill_secure_bits};
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;
use crate::transport::ScTransport;

#[derive(Debug)]
pub enum PipelineEvent {
    Tick(TickInfo),
    Shutdown {
        reply: oneshot::Sender<Option<RevealCommitJob>>,
    },
}

#[derive(Debug)]
pub struct RevealCommitJob {
    pub input: RevealAndCommitInput,
    pub amount: u64,
    pub tick: u32,
}

pub struct Pipeline {
    config: Config,
    pending: Option<PendingCommit>,
    tick: TickInfo,
    id: usize,
}

#[derive(Debug, Clone)]
struct PendingCommit {
    reveal_send_at_tick: u32,
    revealed_bits: [u8; 512],
}

impl Pipeline {
    pub fn new(config: Config, id: usize) -> Self {
        Self {
            config,
            pending: None,
            tick: Default::default(),
            id,
        }
    }

    pub async fn run(
        mut self,
        mut tick_rx: mpsc::Receiver<PipelineEvent>,
        job_tx: mpsc::Sender<RevealCommitJob>,
    ) {
        let sleep_duration = Duration::from_millis(self.config.pipeline_sleep_ms);
        while let Some(event) = tick_rx.recv().await {
            match event {
                PipelineEvent::Tick(tick) => {
                    let reveal_delay = self.config.reveal_delay_ticks;

                    if tick.tick <= self.tick.tick {
                        // skip old tick
                        continue;
                    }

                    self.tick = tick.clone();

                    match &self.pending {
                        None => {
                            let scheduled_tick =
                                tick.tick.saturating_add(self.config.tx_tick_offset);
                            let mut revealed_bits = [0u8; 512];
                            fill_secure_bits(&mut revealed_bits);
                            let committed_digest = commit_digest(&revealed_bits);
                            let commit = format_commit(&committed_digest);

                            let commit_input = RevealAndCommitInput {
                                revealed_bits: [0u8; 512],
                                committed_digest,
                            };
                            let commit_job = RevealCommitJob {
                                input: commit_input,
                                amount: self.config.commit_amount,
                                tick: scheduled_tick,
                            };

                            console::log_info(format!(
                                "pipeline[{id}] commit-only: now_tick={now_tick} commit_tick={commit_tick} amount={amount} commit={commit}",
                                id = self.id,
                                now_tick = tick.tick,
                                commit_tick = scheduled_tick,
                                amount = self.config.commit_amount,
                                commit = commit
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
                            if tick.tick < pending.reveal_send_at_tick - 2 {
                                console::log_info(format!(
                                    "pipeline[{id}] waiting: now_tick={now_tick} reveal_send_at_tick={reveal_send_at_tick}",
                                    id = self.id,
                                    now_tick = tick.tick,
                                    reveal_send_at_tick = pending.reveal_send_at_tick,
                                ));
                                continue;
                            }

                            let next_reveal_tick =
                                pending.reveal_send_at_tick.saturating_add(reveal_delay);
                            let mut next_bits = [0u8; 512];
                            fill_secure_bits(&mut next_bits);
                            let next_digest = commit_digest(&next_bits);
                            let commit = format_commit(&next_digest);

                            let reveal_input = RevealAndCommitInput {
                                revealed_bits: pending.revealed_bits,
                                committed_digest: next_digest,
                            };
                            let reveal_job = RevealCommitJob {
                                input: reveal_input,
                                amount: self.config.commit_amount,
                                tick: pending.reveal_send_at_tick,
                            };

                            console::log_info(format!(
                                "pipeline[{id}] reveal+commit: now_tick={now_tick} next_reveal_tick={next_reveal_tick} amount={amount} commit={commit}",
                                id = self.id,
                                now_tick = tick.tick,
                                next_reveal_tick = next_reveal_tick,
                                amount = self.config.commit_amount,
                                commit = commit
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

                    sleep(sleep_duration).await;
                }
                PipelineEvent::Shutdown { reply } => {
                    let shutdown_job = self.pending.take().map(|pending| {
                        let current_tick =
                            self.tick.tick.saturating_add(self.config.tx_tick_offset);
                        let reveal_tick = current_tick.max(pending.reveal_send_at_tick);
                        let committed_digest = commit_digest(&pending.revealed_bits);
                        let commit = format_commit(&committed_digest);
                        let reveal_input = RevealAndCommitInput {
                            revealed_bits: pending.revealed_bits,
                            committed_digest,
                        };
                        let reveal_job = RevealCommitJob {
                            input: reveal_input,
                            amount: 0,
                            tick: reveal_tick,
                        };

                        console::log_info(format!(
                            "pipeline[{id}] shutdown reveal: now_tick={now_tick} reveal_tick={reveal_tick} amount=0 commit={commit}",
                            id = self.id,
                            now_tick = self.tick.tick,
                            reveal_tick = reveal_tick,
                            commit = commit
                        ));
                        reveal_job
                    });
                    let _ = reply.send(shutdown_job);
                    break;
                }
            }
        }
    }
}

fn format_commit(commit: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in commit {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub async fn run_job_dispatcher(
    mut job_rx: mpsc::Receiver<RevealCommitJob>,
    transport: Arc<dyn ScTransport>,
    workers: usize,
) {
    let workers = workers.max(1);
    let semaphore = Arc::new(Semaphore::new(workers));

    while let Some(job) = job_rx.recv().await {
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let transport = transport.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(err) = transport
                .send_reveal_and_commit(job.input, job.amount, job.tick)
                .await
            {
                console::log_warn(format!("send RevealAndCommit failed: {err}"));
            }
        });
    }
}
