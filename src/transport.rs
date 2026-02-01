use async_trait::async_trait;

use std::borrow::Cow;

use crate::balance::BalanceEntry;
use crate::protocol::RevealAndCommitInput;
use crate::ticks::TickInfo;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use scapi::rpc::RpcClient;
use scapi::rpc::get_balance_with;
use scapi::rpc::get_tick_info_with;
use scapi::rpc::post::broadcast_transaction_with;
use scapi::{QubicId, QubicWallet, build_ticket_tx_bytes};

use crate::console;
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
    async fn get_balances(&self, identity: &str) -> Result<Vec<BalanceEntry>, TransportError>;
}

#[async_trait]
pub trait ScTransport: Send + Sync {
    async fn send_reveal_and_commit(
        &self,
        input: RevealAndCommitInput,
        amount: u64,
        tick: u32,
    ) -> Result<String, TransportError>;
}

#[derive(Debug, Clone)]
pub struct ScapiRpcClient {
    rpc: RpcClient,
}

impl ScapiRpcClient {
    pub fn new(base_url: String) -> Self {
        let rpc = RpcClient::with_base_url(Cow::Owned(base_url));
        Self { rpc }
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

    async fn get_balances(&self, identity: &str) -> Result<Vec<BalanceEntry>, TransportError> {
        let response =
            get_balance_with(&self.rpc, identity)
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

#[derive(Debug, Clone)]
pub struct ScapiContractTransport {
    rpc: RpcClient,
    contract_id: QubicId,
    input_type: u16,
    wallet: QubicWallet,
    identity: String,
}

impl ScapiContractTransport {
    pub fn new(
        base_url: String,
        wallet: QubicWallet,
        contract_id: QubicId,
        input_type: u16,
    ) -> Self {
        let rpc = RpcClient::with_base_url(Cow::Owned(base_url));
        let identity = wallet.public_key.get_identity();
        Self {
            rpc,
            contract_id,
            input_type,
            wallet,
            identity,
        }
    }
}

#[async_trait]
impl ScTransport for ScapiContractTransport {
    async fn send_reveal_and_commit(
        &self,
        input: RevealAndCommitInput,
        amount: u64,
        tick: u32,
    ) -> Result<String, TransportError> {
        if amount > 0 {
            let response = get_balance_with(&self.rpc, &self.identity)
                .await
                .map_err(|err| TransportError {
                    message: format!("failed to fetch balance: {}", err),
                })?;
            let balance =
                response
                    .balance
                    .balance
                    .parse::<u64>()
                    .map_err(|err| TransportError {
                        message: format!("invalid balance value: {}", err),
                    })?;
            if balance < amount {
                return Err(TransportError {
                    message: format!("insufficient balance: have {} need {}", balance, amount),
                });
            }
        }

        let mut payload = Vec::with_capacity(544);
        payload.extend_from_slice(&input.revealed_bits);
        payload.extend_from_slice(&input.committed_digest);

        console::log_info(format!(
            "scapi tx: build+send amount={amount} tick={tick} input_type={input_type} contract={contract_id}",
            amount = amount,
            tick = tick,
            input_type = self.input_type,
            contract_id = console::shorten_id(&self.contract_id.to_string())
        ));

        let tx_bytes = build_ticket_tx_bytes(
            &self.wallet,
            self.contract_id,
            amount,
            tick,
            self.input_type,
            payload,
        )
        .map_err(|err| TransportError {
            message: format!("failed to build transaction: {}", err),
        })?;

        let encoded = BASE64_STANDARD.encode(tx_bytes);
        let response = broadcast_transaction_with(&self.rpc, encoded)
            .await
            .map_err(|err| {
                console::log_warn(format!("scapi tx: broadcast failed: {}", err));
                TransportError {
                    message: format!("broadcast transaction failed: {}", err),
                }
            })?;

        console::log_info(format!(
            "scapi tx: broadcast ok tx_id={}",
            console::shorten_id(&response.transaction_id)
        ));

        Ok(response.transaction_id)
    }
}
