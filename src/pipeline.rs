use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::console;
use crate::entropy::{commit_digest, fill_secure_bits};
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;
use crate::transport::ScTransport;

#[derive(Debug)]
pub struct RevealCommitJob {
    pub input: RevealAndCommitInput,
    pub amount: u64,
    pub tick: u32,
}

pub struct Pipeline {
    config: Config,
    next_allowed_tick: Option<u32>,
}

impl Pipeline {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            next_allowed_tick: None,
        }
    }

    pub async fn run(
        mut self,
        mut tick_rx: mpsc::Receiver<TickInfo>,
        job_tx: mpsc::Sender<RevealCommitJob>,
    ) {
        while let Some(tick) = tick_rx.recv().await {
            if let Some(next_tick) = self.next_allowed_tick {
                if tick.tick < next_tick {
                    continue;
                }
            }

            let scheduled_tick = tick.tick.saturating_add(self.config.tx_tick_offset);
            let reveal_tick = scheduled_tick.saturating_add(self.config.reveal_delay_ticks);

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
            };

            if job_tx.send(commit_job).await.is_err() {
                break;
            }

            let reveal_input = RevealAndCommitInput {
                revealed_bits,
                committed_digest,
            };
            let reveal_job = RevealCommitJob {
                input: reveal_input,
                amount: 0,
                tick: reveal_tick,
            };

            if job_tx.send(reveal_job).await.is_err() {
                break;
            }

            if self.config.commit_interval_ticks > 0 {
                self.next_allowed_tick =
                    Some(reveal_tick.saturating_add(self.config.commit_interval_ticks));
            }
        }
    }
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
                console::log_warn(format!("send RevealAndCommit failed: {}", err));
            }
        });
    }
}
