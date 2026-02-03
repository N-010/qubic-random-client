use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

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
}

impl TickDataWatcher {
    pub fn new(
        current_tick: Arc<AtomicU64>,
        tick_rx: mpsc::Receiver<u32>,
        interval_ms: u64,
    ) -> Self {
        Self {
            current_tick,
            tick_rx,
            interval_ms,
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
                        if current < tick.saturating_add(10) {
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
                    match get_tick_data(tick).await {
                        Ok(response) => {
                            if response.tick_data.is_none() {
                                console::log_info(format!("tick data empty: tick={tick}"));
                                console::record_reveal_empty();
                            } else {
                                console::log_info(format!("tick data ok: tick={tick}"));
                            }
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
