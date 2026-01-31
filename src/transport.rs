use async_trait::async_trait;

use std::borrow::Cow;

use crate::balance::BalanceEntry;
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;

use scapi::rpc::get_balance_with;
use scapi::rpc::get_tick_info_with;
use scapi::rpc::RpcClient;

#[derive(Debug)]
pub struct TransportError {
    pub message: String,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TransportError {}

#[async_trait]
pub trait ScapiClient: Send + Sync {
    async fn get_tick_info(&self) -> Result<TickInfo, TransportError>;
    async fn get_balances(&self) -> Result<Vec<BalanceEntry>, TransportError>;
}

#[async_trait]
pub trait ScTransport: Send + Sync {
    async fn send_reveal_and_commit(
        &self,
        input: RevealAndCommitInput,
        amount: u64,
    ) -> Result<String, TransportError>;
}

#[derive(Debug, Default)]
pub struct NullScapiClient;

#[async_trait]
impl ScapiClient for NullScapiClient {
    async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
        Err(TransportError {
            message: "SCAPI client not configured".to_string(),
        })
    }

    async fn get_balances(&self) -> Result<Vec<BalanceEntry>, TransportError> {
        Err(TransportError {
            message: "SCAPI client not configured".to_string(),
        })
    }
}

#[derive(Debug, Default)]
pub struct NullTransport;

#[async_trait]
impl ScTransport for NullTransport {
    async fn send_reveal_and_commit(
        &self,
        _input: RevealAndCommitInput,
        _amount: u64,
    ) -> Result<String, TransportError> {
        Err(TransportError {
            message: "Transport not configured".to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ScapiRpcClient {
    rpc: RpcClient,
    identity: Option<String>,
}

impl ScapiRpcClient {
    pub fn new(base_url: String, identity: Option<String>) -> Self {
        let rpc = RpcClient::with_base_url(Cow::Owned(base_url));
        Self { rpc, identity }
    }
}

#[async_trait]
impl ScapiClient for ScapiRpcClient {
    async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
        let info = get_tick_info_with(&self.rpc)
            .await
            .map_err(|err| TransportError {
                message: err.to_string(),
            })?
            .tick_info;

        Ok(TickInfo {
            epoch: info.epoch,
            tick: info.tick,
            tick_duration_ms: info.duration,
        })
    }

    async fn get_balances(&self) -> Result<Vec<BalanceEntry>, TransportError> {
        let identity = match &self.identity {
            Some(identity) => identity,
            None => return Ok(Vec::new()),
        };

        let response = get_balance_with(&self.rpc, identity)
            .await
            .map_err(|err| TransportError {
                message: err.to_string(),
            })?;

        let amount = response
            .balance
            .balance
            .parse::<u64>()
            .map_err(|err| TransportError {
                message: format!("invalid balance value: {}", err),
            })?;

        Ok(vec![BalanceEntry {
            asset: response.balance.id,
            amount,
        }])
    }
}
