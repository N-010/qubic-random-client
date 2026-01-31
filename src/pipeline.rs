use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::console;
use crate::entropy::{commit_digest, seed_to_rng_seed, XorShift64};
use crate::pipeline::state::{PendingItem, PendingState};
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;
use crate::transport::ScTransport;

pub mod state;

#[derive(Debug)]
pub struct RevealCommitJob {
    pub input: RevealAndCommitInput,
    pub amount: u64,
}

pub struct Pipeline {
    config: Config,
    rng: XorShift64,
    pending: PendingState,
}

impl Pipeline {
    pub fn new(config: Config) -> Self {
        let seed = match config.seed.as_deref() {
            Some(seed) => seed_to_rng_seed(seed).unwrap_or(0x5a17_91f3_0b2d_22a1),
            None => 0x5a17_91f3_0b2d_22a1,
        };
        Self {
            config,
            rng: XorShift64::new(seed),
            pending: PendingState::new(),
        }
    }

    pub async fn run(
        mut self,
        mut tick_rx: mpsc::Receiver<TickInfo>,
        job_tx: mpsc::Sender<RevealCommitJob>,
    ) {
        while let Some(tick) = tick_rx.recv().await {
            let new_pending = self.next_pending(tick.tick);
            let reveal = self
                .pending
                .pop_revealable(tick.tick, self.config.reveal_delay_ticks);

            let reveal_bits = match reveal {
                Some(item) => item.revealed_bits,
                None => {
                    if self.pending.len() == 0 {
                        console::log_info("pipeline bootstrap: using zero reveal");
                    }
                    [0u8; 512]
                }
            };

            let input = RevealAndCommitInput {
                revealed_bits: reveal_bits,
                committed_digest: new_pending.committed_digest,
            };
            let job = RevealCommitJob {
                input,
                amount: self.config.deposit_amount,
            };

            if job_tx.send(job).await.is_err() {
                break;
            }
            self.pending.push(new_pending);
        }
    }

    fn next_pending(&mut self, tick: u32) -> PendingItem {
        let mut bits = [0u8; 512];
        self.rng.next_bytes(&mut bits);
        let digest = commit_digest(&bits);

        PendingItem {
            commit_tick: tick,
            revealed_bits: bits,
            committed_digest: digest,
            amount: self.config.deposit_amount,
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
                .send_reveal_and_commit(job.input, job.amount)
                .await
            {
                console::log_warn(format!("send RevealAndCommit failed: {}", err));
            }
        });
    }
}
