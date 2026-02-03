use async_trait::async_trait;

use std::borrow::Cow;
use std::sync::Arc;

use crate::balance::{BalanceEntry, BalanceState};
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
        pipeline_id: usize,
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

fn map_tick_info_response(response: scapi::rpc::get::TickInfoResponse) -> TickInfo {
    let info = response.tick_info;
    TickInfo {
        epoch: info.epoch,
        tick: info.tick,
    }
}

fn map_balance_response(
    response: scapi::rpc::get::BalanceResponse,
) -> Result<Vec<BalanceEntry>, TransportError> {
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

#[async_trait]
impl ScapiClient for ScapiRpcClient {
    async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
        let info = get_tick_info_with(&self.rpc)
            .await
            .map_err(|err| TransportError {
                message: err.to_string(),
            })?;

        Ok(map_tick_info_response(info))
    }

    async fn get_balances(&self, identity: &str) -> Result<Vec<BalanceEntry>, TransportError> {
        let response =
            get_balance_with(&self.rpc, identity)
                .await
                .map_err(|err| TransportError {
                    message: err.to_string(),
                })?;

        map_balance_response(response)
    }
}

#[derive(Debug, Clone)]
pub struct ScapiContractTransport {
    rpc: RpcClient,
    contract_id: QubicId,
    input_type: u16,
    wallet: QubicWallet,
    balance_state: Arc<BalanceState>,
}

impl ScapiContractTransport {
    pub fn new(
        base_url: String,
        wallet: QubicWallet,
        contract_id: QubicId,
        input_type: u16,
        balance_state: Arc<BalanceState>,
    ) -> Self {
        let rpc = RpcClient::with_base_url(Cow::Owned(base_url));
        Self {
            rpc,
            contract_id,
            input_type,
            wallet,
            balance_state,
        }
    }
}

fn build_payload(input: &RevealAndCommitInput) -> Vec<u8> {
    let mut payload = Vec::with_capacity(544);
    payload.extend_from_slice(&input.revealed_bits);
    payload.extend_from_slice(&input.committed_digest);
    payload
}

fn build_tx_bytes(
    wallet: &QubicWallet,
    contract_id: QubicId,
    amount: u64,
    tick: u32,
    input_type: u16,
    payload: Vec<u8>,
) -> Result<Vec<u8>, TransportError> {
    build_ticket_tx_bytes(wallet, contract_id, amount, tick, input_type, payload).map_err(|err| {
        TransportError {
            message: format!("failed to build transaction: {}", err),
        }
    })
}

#[async_trait]
impl ScTransport for ScapiContractTransport {
    async fn send_reveal_and_commit(
        &self,
        input: RevealAndCommitInput,
        amount: u64,
        tick: u32,
        pipeline_id: usize,
    ) -> Result<String, TransportError> {
        if amount > 0 {
            let balance = self.balance_state.amount();
            if balance < amount {
                return Err(TransportError {
                    message: format!("insufficient balance: have {} need {}", balance, amount),
                });
            }
        }

        let payload = build_payload(&input);

        console::log_info(format!(
            "scapi tx[{pipeline_id}]: build+send amount={amount} tick={tick} input_type={input_type}",
            input_type = self.input_type,
        ));

        let tx_bytes = build_tx_bytes(
            &self.wallet,
            self.contract_id,
            amount,
            tick,
            self.input_type,
            payload,
        )?;

        let encoded = BASE64_STANDARD.encode(tx_bytes);
        let response = broadcast_transaction_with(&self.rpc, encoded)
            .await
            .map_err(|err| {
                console::log_warn(format!(
                    "scapi tx[{pipeline_id}]: broadcast failed: {}",
                    err
                ));
                TransportError {
                    message: format!("broadcast transaction failed: {}", err),
                }
            })?;

        console::log_info(format!("scapi tx[{pipeline_id}]: broadcast ok"));

        Ok(response.transaction_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{ScTransport, ScapiContractTransport, build_payload, build_tx_bytes};
    use crate::balance::BalanceState;
    use crate::protocol::RevealAndCommitInput;
    use scapi::rpc::get::{BalanceInfo, BalanceResponse, TickInfo, TickInfoResponse};
    use scapi::{QubicId, QubicWallet};
    use std::str::FromStr;
    use std::sync::Arc;

    #[tokio::test]
    async fn build_payload_is_544_bytes() {
        // Payload is 512 revealed bytes + 32 digest bytes.
        let input = RevealAndCommitInput {
            revealed_bits: [1u8; 512],
            committed_digest: [2u8; 32],
        };
        let payload = build_payload(&input);
        assert_eq!(payload.len(), 544);
        assert_eq!(&payload[..512], &input.revealed_bits);
        assert_eq!(&payload[512..], &input.committed_digest);
    }

    #[test]
    fn map_tick_info_response_maps() {
        let response = TickInfoResponse {
            tick_info: TickInfo {
                tick: 12,
                duration: 10,
                epoch: 3,
                initial_tick: 1,
            },
        };
        let info = super::map_tick_info_response(response);
        assert_eq!(info.tick, 12);
        assert_eq!(info.epoch, 3);
    }

    #[test]
    fn map_balance_response_maps() {
        let response = BalanceResponse {
            balance: BalanceInfo {
                id: "ID".to_string(),
                balance: "42".to_string(),
                valid_for_tick: 1,
                latest_incoming_transfer_tick: 2,
                latest_outgoing_transfer_tick: 3,
            },
        };

        let balances = super::map_balance_response(response).expect("balances");
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].amount, 42);
    }

    #[test]
    fn map_balance_response_rejects_invalid_balance() {
        let response = BalanceResponse {
            balance: BalanceInfo {
                id: "ID".to_string(),
                balance: "NaN".to_string(),
                valid_for_tick: 1,
                latest_incoming_transfer_tick: 2,
                latest_outgoing_transfer_tick: 3,
            },
        };

        let err = super::map_balance_response(response).expect_err("expected error");
        assert!(err.message.contains("invalid balance value"));
    }

    #[tokio::test]
    async fn scapi_contract_transport_fails_on_insufficient_balance() {
        // Balance below amount should fail.
        let seed = "a".repeat(55);
        let wallet = QubicWallet::from_seed(&seed).expect("wallet");
        let contract_id =
            QubicId::from_str("DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG")
                .expect("contract");
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(0);
        let transport = ScapiContractTransport::new(
            "http://127.0.0.1:0".to_string(),
            wallet,
            contract_id,
            1,
            balance_state,
        );

        let input = RevealAndCommitInput {
            revealed_bits: [0u8; 512],
            committed_digest: [0u8; 32],
        };
        let err = transport
            .send_reveal_and_commit(input, 1, 10, 0)
            .await
            .expect_err("expected error");
        assert!(err.message.contains("insufficient balance"));
    }

    #[test]
    fn build_tx_bytes_rejects_oversized_payload() {
        // Payload larger than u16::MAX should error.
        let seed = "a".repeat(55);
        let wallet = QubicWallet::from_seed(&seed).expect("wallet");
        let contract_id =
            QubicId::from_str("DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG")
                .expect("contract");
        let payload = vec![0u8; (u16::MAX as usize) + 1];
        let err =
            build_tx_bytes(&wallet, contract_id, 1, 10, 1, payload).expect_err("expected error");
        assert!(err.message.contains("payload too large"));
    }
}
