use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::console;
use crate::transport::ScapiClient;

#[derive(Debug, Clone)]
pub struct BalanceEntry {
    pub asset: String,
    pub amount: u64,
}

pub async fn run_balance_watcher(
    client: Arc<dyn ScapiClient>,
    identity: String,
    interval: Duration,
) {
    loop {
        match client.get_balances(&identity).await {
            Ok(entries) => {
                if entries.is_empty() {
                    console::set_balance_line("empty");
                } else {
                    let mut parts = Vec::with_capacity(entries.len());
                    for entry in entries {
                        parts.push(format!("{}={}", entry.asset, entry.amount));
                    }
                    console::set_balance_line(parts.join(" | "));
                }
            }
            Err(err) => {
                console::log_warn(format!("balance watcher error: {}", err));
            }
        }
        sleep(interval).await;
    }
}
