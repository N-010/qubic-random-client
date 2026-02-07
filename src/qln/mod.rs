use std::sync::Arc;

use async_trait::async_trait;
use tonic::Request;
use tonic::transport::{Channel, Endpoint};

use crate::balance::BalanceEntry;
use crate::tick_data_watcher::TickDataFetcher;
use crate::ticks::TickInfo;
use crate::transport::{ScapiClient, TransportError};

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

#[cfg(test)]
mod tests {
    use super::{
        lightnodepb, map_balance_response, map_status_response, map_tick_transactions_response,
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
}
