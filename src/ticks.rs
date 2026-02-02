use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::console;
use crate::transport::ScapiClient;

#[derive(Debug, Default, Clone)]
pub struct TickInfo {
    pub epoch: u32,
    pub tick: u32,
}

pub struct ScapiTickSource {
    client: Arc<dyn ScapiClient>,
    poll_interval: Duration,
    last: Option<(u32, u32)>,
}

impl ScapiTickSource {
    pub fn new(client: Arc<dyn ScapiClient>, poll_interval: Duration) -> Self {
        Self {
            client,
            poll_interval,
            last: None,
        }
    }

    pub async fn run(mut self, tx: mpsc::Sender<TickInfo>) {
        loop {
            match self.client.get_tick_info().await {
                Ok(info) => {
                    let key = (info.epoch, info.tick);
                    if self.last != Some(key) {
                        self.last = Some(key);
                        console::set_tick_line(format!("{}", info.tick));
                        if tx.send(info).await.is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    console::log_warn(format!("tick source error: {}", err));
                }
            }
            sleep(self.poll_interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScapiTickSource, TickInfo};
    use crate::transport::{ScapiClient, TransportError};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    #[derive(Debug, Default)]
    struct MockClient {
        ticks: Mutex<VecDeque<Result<TickInfo, TransportError>>>,
    }

    impl MockClient {
        fn push_tick(&self, tick: Result<TickInfo, TransportError>) {
            self.ticks.lock().expect("lock ticks").push_back(tick);
        }
    }

    #[async_trait]
    impl ScapiClient for MockClient {
        async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
            self.ticks
                .lock()
                .expect("lock ticks")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(TransportError {
                        message: "no more ticks".to_string(),
                    })
                })
        }

        async fn get_balances(
            &self,
            _identity: &str,
        ) -> Result<Vec<crate::balance::BalanceEntry>, TransportError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn tick_source_skips_duplicates() {
        // Only changes in (epoch, tick) should be forwarded.
        let client = Arc::new(MockClient::default());
        client.push_tick(Ok(TickInfo { epoch: 1, tick: 10 }));
        client.push_tick(Ok(TickInfo { epoch: 1, tick: 10 }));
        client.push_tick(Ok(TickInfo { epoch: 1, tick: 11 }));

        let (tx, mut rx) = mpsc::channel(4);
        let tick_source = ScapiTickSource::new(client, Duration::from_millis(1));
        let handle = tokio::spawn(tick_source.run(tx));

        let first = timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("first tick timeout")
            .expect("first tick");
        assert_eq!(first.tick, 10);

        let second = timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("second tick timeout")
            .expect("second tick");
        assert_eq!(second.tick, 11);

        drop(rx);
        handle.abort();
    }

    #[tokio::test]
    async fn tick_source_continues_after_error() {
        // Errors should be logged but not stop the loop.
        let client = Arc::new(MockClient::default());
        client.push_tick(Err(TransportError {
            message: "boom".to_string(),
        }));
        client.push_tick(Ok(TickInfo { epoch: 1, tick: 5 }));

        let (tx, mut rx) = mpsc::channel(4);
        let tick_source = ScapiTickSource::new(client, Duration::from_millis(1));
        let handle = tokio::spawn(tick_source.run(tx));

        let tick = timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("tick timeout")
            .expect("tick");
        assert_eq!(tick.tick, 5);

        drop(rx);
        handle.abort();
    }

    #[tokio::test]
    async fn tick_source_stops_when_channel_closed() {
        // Closing the receiver should stop the source loop.
        let client = Arc::new(MockClient::default());
        client.push_tick(Ok(TickInfo { epoch: 1, tick: 5 }));

        let (tx, rx) = mpsc::channel(4);
        let tick_source = ScapiTickSource::new(client, Duration::from_millis(1));
        let handle = tokio::spawn(tick_source.run(tx));

        drop(rx);

        let result = timeout(Duration::from_millis(50), handle).await;
        assert!(result.is_ok());
    }
}
