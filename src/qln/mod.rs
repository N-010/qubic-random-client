use std::sync::Arc;

use async_trait::async_trait;
use scapi::{QubicId, QubicWallet};
use tonic::Request;
use tonic::transport::{Channel, Endpoint};

use crate::balance::{BalanceEntry, BalanceState};
use crate::console;
use crate::protocol::RevealAndCommitInput;
use crate::tick_data_watcher::TickDataFetcher;
use crate::ticks::TickInfo;
use crate::transport::{
    ScTransport, ScapiClient, TransportError, build_reveal_and_commit_tx_bytes,
    ensure_amount_available,
};

pub mod lightnodepb {
    tonic::include_proto!("lightnode");
}

#[derive(Debug)]
pub struct QlnGrpcClient {
    inner: lightnodepb::light_node_client::LightNodeClient<Channel>,
}

impl QlnGrpcClient {
    pub fn new(endpoint: &str) -> Result<Self, String> {
        let endpoint = Endpoint::from_shared(endpoint.to_string())
            .map_err(|err| format!("invalid gRPC endpoint: {err}"))?;
        let channel = endpoint.connect_lazy();
        let inner = lightnodepb::light_node_client::LightNodeClient::new(channel);
        Ok(Self { inner })
    }

    async fn get_status(&self) -> Result<lightnodepb::GetStatusResponse, String> {
        let mut client = self.inner.clone();
        client
            .get_status(Request::new(lightnodepb::GetStatusRequest {}))
            .await
            .map(|response| response.into_inner())
            .map_err(|err| format!("gRPC get_status failed: {err}"))
    }

    async fn get_balance(&self, wallet: &str) -> Result<lightnodepb::GetBalanceResponse, String> {
        let mut client = self.inner.clone();
        client
            .get_balance(Request::new(lightnodepb::GetBalanceRequest {
                wallet: wallet.to_string(),
            }))
            .await
            .map(|response| response.into_inner())
            .map_err(|err| format!("gRPC get_balance failed: {err}"))
    }

    async fn get_tick_transactions(
        &self,
        tick: u32,
    ) -> Result<lightnodepb::GetTickTransactionsResponse, String> {
        let mut client = self.inner.clone();
        client
            .get_tick_transactions(Request::new(lightnodepb::GetTickTransactionsRequest {
                tick,
            }))
            .await
            .map(|response| response.into_inner())
            .map_err(|err| format!("gRPC get_tick_transactions failed: {err}"))
    }

    async fn broadcast_transaction(
        &self,
        tx_bytes: Vec<u8>,
    ) -> Result<lightnodepb::BroadcastTransactionResponse, String> {
        let mut client = self.inner.clone();
        client
            .broadcast_transaction(Request::new(lightnodepb::BroadcastTransactionRequest {
                tx_bytes,
            }))
            .await
            .map(|response| response.into_inner())
            .map_err(|err| format!("gRPC broadcast_transaction failed: {err}"))
    }
}

#[derive(Debug, Clone)]
pub struct QlnScapiClient {
    grpc: Arc<QlnGrpcClient>,
}

impl QlnScapiClient {
    pub fn new(grpc: Arc<QlnGrpcClient>) -> Self {
        Self { grpc }
    }
}

#[derive(Debug, Clone)]
pub struct QlnTickDataFetcher {
    grpc: Arc<QlnGrpcClient>,
}

impl QlnTickDataFetcher {
    pub fn new(grpc: Arc<QlnGrpcClient>) -> Self {
        Self { grpc }
    }
}

#[derive(Debug, Clone)]
pub struct QlnContractTransport {
    grpc: Arc<QlnGrpcClient>,
    contract_id: QubicId,
    input_type: u16,
    wallet: QubicWallet,
    balance_state: Arc<BalanceState>,
}

impl QlnContractTransport {
    pub fn new(
        grpc: Arc<QlnGrpcClient>,
        wallet: QubicWallet,
        contract_id: QubicId,
        input_type: u16,
        balance_state: Arc<BalanceState>,
    ) -> Self {
        Self {
            grpc,
            contract_id,
            input_type,
            wallet,
            balance_state,
        }
    }
}

fn status_to_tick_info(status: lightnodepb::TickStatus) -> TickInfo {
    TickInfo {
        epoch: status.epoch,
        tick: status.tick,
        initial_tick: status.initial_tick,
    }
}

fn map_status_response(response: lightnodepb::GetStatusResponse) -> Result<TickInfo, String> {
    if !response.ok {
        let warning = response.warning.trim();
        let error = response.error.trim();
        let source = response.source.trim();
        return Err(format!(
            "QLN GetStatus not ok: source={source} warning={warning} error={error}"
        ));
    }

    let status = response
        .status
        .ok_or_else(|| "QLN GetStatus missing status".to_string())?;
    Ok(status_to_tick_info(status))
}

fn map_balance_response(
    response: lightnodepb::GetBalanceResponse,
) -> Result<Vec<BalanceEntry>, String> {
    if !response.ok {
        return Err(format!(
            "QLN GetBalance not ok: error={}",
            response.error.trim()
        ));
    }

    let balance = response
        .balance
        .ok_or_else(|| "QLN GetBalance missing balance".to_string())?;
    let amount = u64::try_from(balance.balance).map_err(|_| {
        format!(
            "QLN GetBalance returned negative amount: {}",
            balance.balance
        )
    })?;

    Ok(vec![BalanceEntry {
        asset: "QUBIC".to_string(),
        amount,
    }])
}

fn map_tick_transactions_response(
    response: lightnodepb::GetTickTransactionsResponse,
) -> Result<Option<()>, String> {
    if !response.ok {
        return Err(format!(
            "QLN GetTickTransactions not ok: error={}",
            response.error.trim()
        ));
    }

    if response.transactions.is_empty() {
        Ok(None)
    } else {
        Ok(Some(()))
    }
}

fn map_broadcast_transaction_response(
    response: lightnodepb::BroadcastTransactionResponse,
) -> Result<String, String> {
    if !response.ok {
        let error = response.error.trim();
        let message = if error.is_empty() {
            "QLN BroadcastTransaction not ok: unknown error".to_string()
        } else {
            format!("QLN BroadcastTransaction not ok: error={error}")
        };
        return Err(message);
    }

    let tx_id = response.tx_id.trim();
    if tx_id.is_empty() {
        return Err("QLN BroadcastTransaction returned empty tx_id".to_string());
    }

    Ok(tx_id.to_string())
}

#[async_trait]
impl ScapiClient for QlnScapiClient {
    async fn get_tick_info(&self) -> Result<TickInfo, TransportError> {
        let response = self
            .grpc
            .get_status()
            .await
            .map_err(|message| TransportError { message })?;
        map_status_response(response).map_err(|message| TransportError { message })
    }

    async fn get_balances(&self, identity: &str) -> Result<Vec<BalanceEntry>, TransportError> {
        let response = self
            .grpc
            .get_balance(identity)
            .await
            .map_err(|message| TransportError { message })?;
        map_balance_response(response).map_err(|message| TransportError { message })
    }
}

#[async_trait]
impl TickDataFetcher for QlnTickDataFetcher {
    async fn fetch(&self, tick: u32) -> Result<Option<()>, String> {
        let response = self.grpc.get_tick_transactions(tick).await?;
        map_tick_transactions_response(response)
    }
}

#[async_trait]
impl ScTransport for QlnContractTransport {
    async fn send_reveal_and_commit(
        &self,
        input: RevealAndCommitInput,
        amount: u64,
        tick: u32,
        pipeline_id: usize,
    ) -> Result<String, TransportError> {
        ensure_amount_available(&self.balance_state, amount)?;

        console::log_info(format!(
            "qln tx[{pipeline_id}]: build+send amount={amount} tick={tick} input_type={input_type}",
            input_type = self.input_type,
        ));
        let tx_bytes = build_reveal_and_commit_tx_bytes(
            &self.wallet,
            self.contract_id,
            amount,
            tick,
            self.input_type,
            &input,
        )?;
        let response = self
            .grpc
            .broadcast_transaction(tx_bytes)
            .await
            .map_err(|message| {
                console::log_warn(format!(
                    "qln tx[{pipeline_id}]: broadcast failed: {message}"
                ));
                TransportError { message }
            })?;
        let tx_id = map_broadcast_transaction_response(response).map_err(|message| {
            console::log_warn(format!(
                "qln tx[{pipeline_id}]: broadcast rejected: {message}"
            ));
            TransportError { message }
        })?;

        console::log_info(format!(
            "qln tx[{pipeline_id}]: broadcast ok tx_id={tx_id}",
            tx_id = console::shorten_id(&tx_id)
        ));
        Ok(tx_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lightnodepb, map_balance_response, map_broadcast_transaction_response, map_status_response,
        map_tick_transactions_response,
    };

    #[test]
    fn map_status_response_maps_tick_info() {
        let response = lightnodepb::GetStatusResponse {
            ok: true,
            source: "live".to_string(),
            status: Some(lightnodepb::TickStatus {
                epoch: 1,
                tick: 42,
                initial_tick: 10,
                tick_duration_ms: 0,
                aligned_votes: 0,
                misaligned_votes: 0,
            }),
            warning: String::new(),
            error: String::new(),
        };

        let info = map_status_response(response).expect("tick info");
        assert_eq!(info.epoch, 1);
        assert_eq!(info.tick, 42);
        assert_eq!(info.initial_tick, 10);
    }

    #[test]
    fn map_balance_response_maps_amount() {
        let response = lightnodepb::GetBalanceResponse {
            ok: true,
            balance: Some(lightnodepb::Balance {
                wallet: "WALLET".to_string(),
                public_key_hex: "abc".to_string(),
                tick: 1,
                spectrum_index: 0,
                incoming_amount: 100,
                outgoing_amount: 50,
                balance: 50,
                number_of_incoming_transfers: 0,
                number_of_outgoing_transfers: 0,
                latest_incoming_transfer_tick: 0,
                latest_outgoing_transfer_tick: 0,
            }),
            error: String::new(),
        };

        let balances = map_balance_response(response).expect("balances");
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].asset, "QUBIC");
        assert_eq!(balances[0].amount, 50);
    }

    #[test]
    fn map_tick_transactions_response_handles_empty_and_non_empty() {
        let empty = lightnodepb::GetTickTransactionsResponse {
            ok: true,
            tick: 1,
            transactions: Vec::new(),
            error: String::new(),
        };
        let non_empty = lightnodepb::GetTickTransactionsResponse {
            ok: true,
            tick: 1,
            transactions: vec![lightnodepb::Transaction {
                source_public_key_hex: "1".to_string(),
                destination_public_key_hex: "2".to_string(),
                amount: 1,
                tick: 1,
                input_type: 1,
                input_size: 0,
                input_hex: String::new(),
                signature_hex: String::new(),
            }],
            error: String::new(),
        };

        assert_eq!(map_tick_transactions_response(empty).expect("empty"), None);
        assert_eq!(
            map_tick_transactions_response(non_empty).expect("non empty"),
            Some(())
        );
    }

    #[test]
    fn map_broadcast_transaction_response_maps_success_and_errors() {
        let ok = lightnodepb::BroadcastTransactionResponse {
            ok: true,
            tx_id: "abc123".to_string(),
            error: String::new(),
        };
        let not_ok = lightnodepb::BroadcastTransactionResponse {
            ok: false,
            tx_id: String::new(),
            error: "failed".to_string(),
        };
        let missing_tx = lightnodepb::BroadcastTransactionResponse {
            ok: true,
            tx_id: "  ".to_string(),
            error: String::new(),
        };

        assert_eq!(
            map_broadcast_transaction_response(ok).expect("ok response"),
            "abc123"
        );
        assert!(
            map_broadcast_transaction_response(not_ok)
                .expect_err("not ok")
                .contains("failed")
        );
        assert!(
            map_broadcast_transaction_response(missing_tx)
                .expect_err("missing tx")
                .contains("empty tx_id")
        );
    }
}
