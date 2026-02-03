use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use async_trait::async_trait;
use scapi::rpc::get_tick_data;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::console;
use crate::pipeline::current_tick_load;

pub struct TickDataWatcher {
    current_tick: Arc<AtomicU64>,
    tick_rx: mpsc::Receiver<u32>,
    interval_ms: u64,
    fetcher: Arc<dyn TickDataFetcher>,
}

impl TickDataWatcher {
    pub fn new(
        current_tick: Arc<AtomicU64>,
        tick_rx: mpsc::Receiver<u32>,
        interval_ms: u64,
    ) -> Self {
        Self::new_with_fetcher(
            current_tick,
            tick_rx,
            interval_ms,
            Arc::new(ScapiTickDataFetcher),
        )
    }

    pub fn new_with_fetcher(
        current_tick: Arc<AtomicU64>,
        tick_rx: mpsc::Receiver<u32>,
        interval_ms: u64,
        fetcher: Arc<dyn TickDataFetcher>,
    ) -> Self {
        Self {
            current_tick,
            tick_rx,
            interval_ms,
            fetcher,
        }
    }

    pub async fn run(self) {
        let state = Arc::new(Mutex::new(State::default()));
        let interval_ms = self.interval_ms.max(1);

        let rx_state = state.clone();
        let mut tick_rx = self.tick_rx;
        let rx_task = tokio::spawn(async move {
            while let Some(tick) = tick_rx.recv().await {
                let mut state = rx_state.lock().await;
                state.ticks.insert(tick);
            }
            let mut state = rx_state.lock().await;
            state.rx_closed = true;
        });

        let check_state = state.clone();
        let current_tick = self.current_tick.clone();
        let fetcher = self.fetcher.clone();
        let check_task = tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(interval_ms));
            loop {
                ticker.tick().await;
                let ticks = {
                    let mut state = check_state.lock().await;
                    if state.ticks.is_empty() && state.rx_closed {
                        return;
                    }
                    let Some(current) = current_tick_load(&current_tick) else {
                        continue;
                    };
                    let mut ready = Vec::new();
                    let mut iter = state.ticks.iter().copied();
                    for tick in &mut iter {
                        if current < tick.saturating_add(1) {
                            break;
                        }
                        ready.push(tick);
                    }
                    for tick in &ready {
                        state.ticks.remove(tick);
                    }
                    ready
                };

                for tick in ticks {
                    match fetcher.fetch(tick).await {
                        Ok(None) => {
                            console::log_warn(format!("tick data empty: tick={tick}"));
                            console::record_reveal_empty();
                        }
                        Ok(Some(())) => {
                            console::log_info(format!("tick data ok: tick={tick}"));
                        }
                        Err(err) => {
                            console::log_warn(format!("tick data check failed: {err}"));
                            let mut state = check_state.lock().await;
                            state.ticks.insert(tick);
                        }
                    }
                }
            }
        });

        let _ = tokio::join!(rx_task, check_task);
    }
}

#[derive(Default)]
struct State {
    ticks: BTreeSet<u32>,
    rx_closed: bool,
}

#[async_trait]
pub trait TickDataFetcher: Send + Sync {
    async fn fetch(&self, tick: u32) -> Result<Option<()>, String>;
}

struct ScapiTickDataFetcher;

#[async_trait]
impl TickDataFetcher for ScapiTickDataFetcher {
    async fn fetch(&self, tick: u32) -> Result<Option<()>, String> {
        get_tick_data(tick)
            .await
            .map(|response| response.tick_data.map(|_| ()))
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{TickDataFetcher, TickDataWatcher};
    use crate::console;
    use crate::pipeline::current_tick_store;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::Mutex;
    use tokio::sync::Notify;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    struct MockFetcher {
        calls: AtomicUsize,
        responses: Mutex<Vec<Result<Option<()>, String>>>,
        notify: Option<Arc<Notify>>,
    }

    impl MockFetcher {
        fn new(responses: Vec<Result<Option<()>, String>>) -> Self {
            Self::new_with_notify(responses, None)
        }

        fn new_with_notify(
            responses: Vec<Result<Option<()>, String>>,
            notify: Option<Arc<Notify>>,
        ) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                responses: Mutex::new(responses),
                notify,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl TickDataFetcher for MockFetcher {
        async fn fetch(&self, _tick: u32) -> Result<Option<()>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let mut responses = self.responses.lock().await;
            if let Some(notify) = &self.notify {
                notify.notify_waiters();
            }
            responses.pop().unwrap_or(Ok(Some(())))
        }
    }

    #[tokio::test]
    async fn tick_data_empty_decrements_success() {
        console::init();
        console::reset_reveal_stats();
        console::record_reveal_result(true);
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 2);
        let (tx, rx) = mpsc::channel(4);
        let notify = Arc::new(Notify::new());
        let fetcher = Arc::new(MockFetcher::new_with_notify(
            vec![Ok(None)],
            Some(notify.clone()),
        ));
        let watcher = TickDataWatcher::new_with_fetcher(current_tick.clone(), rx, 1, fetcher);
        tokio::spawn(watcher.run());

        tx.send(1).await.expect("send");
        timeout(Duration::from_millis(200), notify.notified())
            .await
            .expect("timeout");
        let (ok, _fail, empty) = console::reveal_counts();
        assert_eq!((ok, empty), (0, 1));
    }

    #[tokio::test]
    async fn tick_data_ok_keeps_success() {
        console::init();
        console::reset_reveal_stats();
        console::record_reveal_result(true);
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 2);
        let (tx, rx) = mpsc::channel(4);
        let fetcher = Arc::new(MockFetcher::new(vec![Ok(Some(()))]));
        let watcher = TickDataWatcher::new_with_fetcher(current_tick.clone(), rx, 1, fetcher);
        tokio::spawn(watcher.run());

        tx.send(1).await.expect("send");
        timeout(Duration::from_millis(200), async {
            loop {
                let (ok, _fail, empty) = console::reveal_counts();
                if empty == 0 && ok == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("timeout");
    }

    #[tokio::test]
    async fn tick_not_ready_waits() {
        console::init();
        console::reset_reveal_stats();
        console::record_reveal_result(true);
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 1);
        let (tx, rx) = mpsc::channel(4);
        let fetcher = Arc::new(MockFetcher::new(vec![Ok(Some(()))]));
        let watcher =
            TickDataWatcher::new_with_fetcher(current_tick.clone(), rx, 10, fetcher.clone());
        tokio::spawn(watcher.run());

        tx.send(1).await.expect("send");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fetcher.calls(), 0);

        current_tick_store(&current_tick, 2);
        timeout(Duration::from_millis(200), async {
            loop {
                if fetcher.calls() >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("timeout");
    }

    #[tokio::test]
    async fn deduplicates_ticks() {
        console::init();
        console::reset_reveal_stats();
        console::record_reveal_result(true);
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 2);
        let (tx, rx) = mpsc::channel(4);
        let fetcher = Arc::new(MockFetcher::new(vec![Ok(Some(()))]));
        let watcher =
            TickDataWatcher::new_with_fetcher(current_tick.clone(), rx, 1, fetcher.clone());
        tokio::spawn(watcher.run());

        tx.send(1).await.expect("send");
        tx.send(1).await.expect("send dup");
        timeout(Duration::from_millis(200), async {
            loop {
                if fetcher.calls() >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("timeout");
        assert_eq!(fetcher.calls(), 1);
    }

    #[tokio::test]
    async fn retries_on_rpc_error() {
        console::init();
        console::reset_reveal_stats();
        console::record_reveal_result(true);
        let current_tick = Arc::new(AtomicU64::new(0));
        current_tick_store(&current_tick, 2);
        let (tx, rx) = mpsc::channel(4);
        let fetcher = Arc::new(MockFetcher::new(vec![
            Ok(Some(())),
            Err("boom".to_string()),
        ]));
        let watcher =
            TickDataWatcher::new_with_fetcher(current_tick.clone(), rx, 1, fetcher.clone());
        tokio::spawn(watcher.run());

        tx.send(1).await.expect("send");
        timeout(Duration::from_millis(400), async {
            loop {
                if fetcher.calls() >= 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("timeout");
    }
}
