use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::sleep;

use crate::console;
use crate::transport::ScapiClient;

#[derive(Debug, Clone)]
pub struct BalanceEntry {
    pub asset: String,
    pub amount: u64,
}

#[derive(Debug, Default)]
pub struct BalanceState {
    amount: AtomicU64,
}

impl BalanceState {
    pub fn new() -> Self {
        Self {
            amount: AtomicU64::new(0),
        }
    }

    pub fn amount(&self) -> u64 {
        self.amount.load(Ordering::Relaxed)
    }

    pub fn set_amount(&self, amount: u64) {
        self.amount.store(amount, Ordering::Relaxed);
    }
}

pub async fn run_balance_watcher(
    client: Arc<dyn ScapiClient>,
    identity: String,
    interval: Duration,
    state: Arc<BalanceState>,
) {
    loop {
        match client.get_balances(&identity).await {
            Ok(entries) => {
                if entries.is_empty() {
                    console::set_balance_line("empty");
                    state.set_amount(0);
                } else {
                    let mut total = 0u64;
                    let mut parts = Vec::with_capacity(entries.len());
                    for entry in &entries {
                        let asset = console::shorten_id(&entry.asset);
                        let amount = console::format_amount(entry.amount);
                        parts.push(format!("{asset}={amount}"));
                        total = total.saturating_add(entry.amount);
                    }
                    console::set_balance_line(parts.join(" | "));
                    state.set_amount(total);
                }
            }
            Err(err) => {
                console::log_warn(format!("balance watcher error: {}", err));
                state.set_amount(0);
            }
        }
        sleep(interval).await;
    }
}
