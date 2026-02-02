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
                let (line, total) = format_balances(&entries);
                console::set_balance_line(line);
                state.set_amount(total);
            }
            Err(err) => {
                console::log_warn(format!("balance watcher error: {}", err));
                state.set_amount(0);
            }
        }
        sleep(interval).await;
    }
}

fn format_balances(entries: &[BalanceEntry]) -> (String, u64) {
    if entries.is_empty() {
        return ("empty".to_string(), 0);
    }

    let mut total = 0u64;
    let mut parts = Vec::with_capacity(entries.len());
    for entry in entries {
        let asset = console::shorten_id(&entry.asset);
        let amount = console::format_amount(entry.amount);
        parts.push(format!("{asset}={amount}"));
        total = total.saturating_add(entry.amount);
    }
    (parts.join(" | "), total)
}

#[cfg(test)]
mod tests {
    use super::{BalanceEntry, BalanceState, format_balances, run_balance_watcher};
    use crate::transport::{ScapiClient, TransportError};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::time::sleep;

    #[derive(Debug)]
    struct MockClient {
        balances: Mutex<VecDeque<Result<Vec<BalanceEntry>, TransportError>>>,
        default_entries: Vec<BalanceEntry>,
        default_error: Option<String>,
    }

    impl MockClient {
        fn new(default: Result<Vec<BalanceEntry>, TransportError>) -> Self {
            let (default_entries, default_error) = match default {
                Ok(entries) => (entries, None),
                Err(err) => (Vec::new(), Some(err.message)),
            };
            Self {
                balances: Mutex::new(VecDeque::new()),
                default_entries,
                default_error,
            }
        }

        fn push_balances(&self, entry: Result<Vec<BalanceEntry>, TransportError>) {
            self.balances
                .lock()
                .expect("lock balances")
                .push_back(entry);
        }
    }

    #[async_trait]
    impl ScapiClient for MockClient {
        async fn get_tick_info(&self) -> Result<crate::ticks::TickInfo, TransportError> {
            Err(TransportError {
                message: "unused".to_string(),
            })
        }

        async fn get_balances(&self, _identity: &str) -> Result<Vec<BalanceEntry>, TransportError> {
            self.balances
                .lock()
                .expect("lock balances")
                .pop_front()
                .unwrap_or_else(|| {
                    if let Some(message) = self.default_error.clone() {
                        Err(TransportError { message })
                    } else {
                        Ok(self.default_entries.clone())
                    }
                })
        }
    }

    #[tokio::test]
    async fn balance_watcher_sets_empty_balance() {
        // Empty balances should set total to 0.
        let client = Arc::new(MockClient::new(Ok(Vec::new())));
        client.push_balances(Ok(Vec::new()));
        let state = Arc::new(BalanceState::new());

        let handle = tokio::spawn(run_balance_watcher(
            client,
            "id".to_string(),
            Duration::from_millis(50),
            state.clone(),
        ));

        sleep(Duration::from_millis(20)).await;
        assert_eq!(state.amount(), 0);
        handle.abort();
    }

    #[tokio::test]
    async fn balance_watcher_handles_errors() {
        // Errors reset the total to 0.
        let client = Arc::new(MockClient::new(Err(TransportError {
            message: "boom".to_string(),
        })));
        client.push_balances(Err(TransportError {
            message: "boom".to_string(),
        }));
        let state = Arc::new(BalanceState::new());
        state.set_amount(99);

        let handle = tokio::spawn(run_balance_watcher(
            client,
            "id".to_string(),
            Duration::from_millis(50),
            state.clone(),
        ));

        sleep(Duration::from_millis(20)).await;
        assert_eq!(state.amount(), 0);
        handle.abort();
    }

    #[tokio::test]
    async fn balance_watcher_saturates_total() {
        // Saturating add should clamp at u64::MAX.
        let client = Arc::new(MockClient::new(Ok(vec![
            BalanceEntry {
                asset: "AAA".to_string(),
                amount: u64::MAX,
            },
            BalanceEntry {
                asset: "BBB".to_string(),
                amount: 1,
            },
        ])));
        client.push_balances(Ok(vec![
            BalanceEntry {
                asset: "AAA".to_string(),
                amount: u64::MAX,
            },
            BalanceEntry {
                asset: "BBB".to_string(),
                amount: 1,
            },
        ]));
        let state = Arc::new(BalanceState::new());

        let handle = tokio::spawn(run_balance_watcher(
            client,
            "id".to_string(),
            Duration::from_millis(50),
            state.clone(),
        ));

        sleep(Duration::from_millis(20)).await;
        assert_eq!(state.amount(), u64::MAX);
        handle.abort();
    }

    #[test]
    fn format_balances_empty() {
        // Empty list yields "empty" and zero total.
        let (line, total) = format_balances(&[]);
        assert_eq!(line, "empty");
        assert_eq!(total, 0);
    }

    #[test]
    fn format_balances_with_entries() {
        // Line contains shortened ids and formatted amounts.
        let entries = vec![
            BalanceEntry {
                asset: "AAAAAA".to_string(),
                amount: 1_000,
            },
            BalanceEntry {
                asset: "BBBBBB".to_string(),
                amount: 25,
            },
        ];
        let (line, total) = format_balances(&entries);
        assert!(line.contains("AAAAAA=1.000"));
        assert!(line.contains("BBBBBB=25"));
        assert_eq!(total, 1_025);
    }
}
