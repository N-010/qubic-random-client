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
    pub tick_duration_ms: u32,
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
