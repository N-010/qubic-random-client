use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::http::{
    BoundedBobClient as BobRpcClient, BoundedRpcClient as RpcClient, broadcast_transaction_with,
    get_tick_data_with, get_tick_info_with,
};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use tonic::Request;
use tonic::transport::{Channel, Endpoint};

use crate::bob::{extract_result, extract_string_field, extract_u64_field};
use crate::config::{AppConfig, BackendKind};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);
const BOB_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_TICK_DURATION_MS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickTransactions {
    pub has_transactions: bool,
    // None means the backend did not supply a complete authenticated digest set.
    pub transaction_digests: Option<Vec<[u8; 32]>>,
}

impl From<bool> for TickTransactions {
    fn from(has_transactions: bool) -> Self {
        Self {
            has_transactions,
            transaction_digests: None,
        }
    }
}

impl TickTransactions {
    pub fn contains(&self, digest: &[u8; 32]) -> Option<bool> {
        self.transaction_digests
            .as_ref()
            .map(|digests| digests.contains(digest))
    }
}

pub(crate) fn transaction_digest(bytes: &[u8]) -> [u8; 32] {
    use tiny_keccak::{Hasher, KangarooTwelve};
    let mut hasher = KangarooTwelve::new(b"");
    hasher.update(bytes);
    let mut digest = [0; 32];
    hasher.finalize(&mut digest);
    digest
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickInfo {
    pub epoch: u32,
    pub tick: u32,
    pub initial_tick: u32,
    pub tick_duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFunctionRequest {
    pub contract_index: u32,
    pub input_type: u16,
    pub input: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError(String);

impl BackendError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

#[async_trait]
pub trait NetworkBackend: Send + Sync {
    async fn tick_info(&self) -> Result<TickInfo, BackendError>;
    async fn tick_has_transactions(&self, tick: u32) -> Result<TickTransactions, BackendError>;
    async fn query_contract_function(
        &self,
        request: ContractFunctionRequest,
    ) -> Result<Vec<u8>, BackendError>;
    async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError>;
}

pub fn create_backend(config: &AppConfig) -> Result<Arc<dyn NetworkBackend>, BackendError> {
    match config.backend {
        BackendKind::Rpc => Ok(Arc::new(RpcBackend::new(&config.endpoint))),
        BackendKind::Bob => Ok(Arc::new(BobBackend::new(&config.endpoint))),
        BackendKind::Grpc => Ok(Arc::new(QlnBackend::new(&config.endpoint)?)),
    }
}

#[derive(Clone)]
struct RpcBackend {
    live: RpcClient,
    query: RpcClient,
}

impl std::fmt::Debug for RpcBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcBackend").finish_non_exhaustive()
    }
}

impl RpcBackend {
    fn new(root: &str) -> Self {
        Self {
            live: RpcClient::with_base_url(Cow::Owned(crate::config::append_endpoint_path(
                root, "live/v1",
            ))),
            query: RpcClient::with_base_url(Cow::Owned(crate::config::append_endpoint_path(
                root, "query/v1",
            ))),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RpcContractQuery<'a> {
    contract_index: u32,
    input_type: u16,
    input_size: usize,
    request_data: &'a str,
}

#[async_trait]
impl NetworkBackend for RpcBackend {
    async fn tick_info(&self) -> Result<TickInfo, BackendError> {
        let response = get_tick_info_with(&self.live)
            .await
            .map_err(|_| BackendError::new("RPC tick-info failed"))?;
        Ok(TickInfo {
            epoch: response.tick_info.epoch,
            tick: response.tick_info.tick,
            initial_tick: response.tick_info.initial_tick,
            tick_duration_ms: normalize_rpc_tick_duration_ms(response.tick_info.duration),
        })
    }

    async fn tick_has_transactions(&self, tick: u32) -> Result<TickTransactions, BackendError> {
        let response = get_tick_data_with(&self.query, tick)
            .await
            .map_err(|_| BackendError::new("RPC tick-data query failed"))?;
        match response.tick_data {
            Some(data) if data.tick_number != tick => {
                Err(BackendError::new("RPC returned a different tick"))
            }
            Some(data) => Ok((!data.transaction_hashes.is_empty()).into()),
            None => Ok(false.into()),
        }
    }

    async fn query_contract_function(
        &self,
        request: ContractFunctionRequest,
    ) -> Result<Vec<u8>, BackendError> {
        let input = BASE64_STANDARD.encode(&request.input);
        let payload = RpcContractQuery {
            contract_index: request.contract_index,
            input_type: request.input_type,
            input_size: request.input.len(),
            request_data: &input,
        };
        let value = self
            .live
            .post_json_value("querySmartContract", &payload)
            .await
            .map_err(|_| BackendError::new("RPC contract query failed"))?;
        decode_rpc_contract_response(value)
    }

    async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError> {
        let response = broadcast_transaction_with(&self.live, BASE64_STANDARD.encode(tx_bytes))
            .await
            .map_err(|_| BackendError::new("RPC broadcast request failed"))?;
        match response.peers_broadcasted {
            peers if peers > 0 => validate_transaction_id(response.transaction_id),
            0 => Err(BackendError::new("RPC broadcast reached no peers")),
            peers => Err(BackendError::new(format!(
                "RPC broadcast returned invalid peersBroadcasted value: {peers}"
            ))),
        }
    }
}

fn decode_rpc_contract_response(value: Value) -> Result<Vec<u8>, BackendError> {
    let encoded = ["responseData", "data", "result"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .ok_or_else(|| BackendError::new("RPC contract response contains no base64 output"))?;
    BASE64_STANDARD
        .decode(encoded)
        .map_err(|err| BackendError::new(format!("RPC returned invalid base64 output: {err}")))
}

struct BobBackend {
    rpc: Arc<BobRpcClient>,
    next_nonce: AtomicU64,
}

impl std::fmt::Debug for BobBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BobBackend").finish_non_exhaustive()
    }
}

impl BobBackend {
    fn new(endpoint: &str) -> Self {
        let initial_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        Self {
            rpc: Arc::new(BobRpcClient::with_base_url(endpoint)),
            next_nonce: AtomicU64::new(initial_nonce.max(1)),
        }
    }
}

#[async_trait]
impl NetworkBackend for BobBackend {
    async fn tick_info(&self) -> Result<TickInfo, BackendError> {
        let result = extract_result(
            self.rpc
                .qubic_status()
                .await
                .map_err(|_| BackendError::new("Bob status failed"))?,
        )
        .map_err(BackendError::new)?;
        let epoch = checked_u32_field(&result, "currentProcessingEpoch")?;
        let tick = checked_u32_field(&result, "currentVerifyLoggingTick")?;
        let initial_tick = checked_u32_field(&result, "initialTick")?;
        let tick_duration_ms = ["tickDurationMs", "tickDuration", "duration"]
            .into_iter()
            .find_map(|key| optional_u32_field(&result, key))
            .unwrap_or(DEFAULT_TICK_DURATION_MS)
            .max(1);
        Ok(TickInfo {
            epoch,
            tick,
            initial_tick,
            tick_duration_ms,
        })
    }

    async fn tick_has_transactions(&self, tick: u32) -> Result<TickTransactions, BackendError> {
        let result = extract_result(
            self.rpc
                .qubic_get_transfers(json!({
                    "identity": "",
                    "fromTick": tick,
                    "toTick": tick,
                }))
                .await
                .map_err(|_| BackendError::new("Bob tick-data query failed"))?,
        )
        .map_err(BackendError::new)?;
        bob_tick_has_transactions(&result).map(Into::into)
    }

    async fn query_contract_function(
        &self,
        request: ContractFunctionRequest,
    ) -> Result<Vec<u8>, BackendError> {
        let nonce = self.next_nonce.fetch_add(1, Ordering::Relaxed);
        let params = json!([{
            "nonce": nonce,
            "scIndex": request.contract_index,
            "funcNumber": request.input_type,
            "data": format!("0x{}", bytes_to_hex(&request.input)),
        }]);
        let deadline = Instant::now() + NETWORK_TIMEOUT;
        loop {
            let result = extract_result(
                self.rpc
                    .call("qubic_querySmartContract", params.clone())
                    .await
                    .map_err(|_| BackendError::new("Bob contract query failed"))?,
            )
            .map_err(BackendError::new)?;
            if let Some(data) = extract_string_field(&result, &["data"]) {
                return decode_hex(data.trim_start_matches("0x"));
            }
            if extract_string_field(&result, &["error"]).is_some() {
                return Err(BackendError::new("Bob contract query failed"));
            }
            if Instant::now() >= deadline {
                return Err(BackendError::new(format!(
                    "Bob contract query timed out after {} ms",
                    NETWORK_TIMEOUT.as_millis()
                )));
            }
            sleep(BOB_POLL_INTERVAL).await;
        }
    }

    async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError> {
        let result = extract_result(
            self.rpc
                .qubic_broadcast_transaction(bytes_to_hex(&tx_bytes))
                .await
                .map_err(|_| BackendError::new("Bob broadcast request failed"))?,
        )
        .map_err(|_| BackendError::new("Bob broadcast response was invalid"))?;
        let transaction_id = result
            .as_str()
            .map(str::to_string)
            .or_else(|| extract_string_field(&result, &["transactionId", "txId", "hash", "id"]))
            .ok_or_else(|| BackendError::new("Bob broadcast returned no transaction id"))?;
        validate_transaction_id(transaction_id)
    }
}

fn bob_tick_has_transactions(result: &Value) -> Result<bool, BackendError> {
    match result {
        Value::Array(transfers) => Ok(!transfers.is_empty()),
        Value::Object(map) => map
            .get("transfers")
            .and_then(Value::as_array)
            .map(|transfers| !transfers.is_empty())
            .ok_or_else(|| BackendError::new("Bob transfers response is missing transfers array")),
        _ => Err(BackendError::new(
            "Bob transfers response has an unexpected shape",
        )),
    }
}

fn checked_u32_field(value: &Value, key: &str) -> Result<u32, BackendError> {
    let value = extract_u64_field(value, &[key])
        .ok_or_else(|| BackendError::new(format!("Bob response missing {key}")))?;
    u32::try_from(value)
        .map_err(|_| BackendError::new(format!("Bob response {key} is out of range: {value}")))
}

fn optional_u32_field(value: &Value, key: &str) -> Option<u32> {
    let value = value.get(key)?;
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

pub mod lightnodepb {
    tonic::include_proto!("lightnode");
}

#[derive(Debug, Default)]
struct QlnTickFallback {
    epoch: Option<u32>,
    initial_tick: u32,
}

#[derive(Debug, Clone)]
struct QlnBackend {
    client: lightnodepb::light_node_client::LightNodeClient<Channel>,
    tick_fallback: Arc<Mutex<QlnTickFallback>>,
}

impl QlnBackend {
    fn new(endpoint: &str) -> Result<Self, BackendError> {
        let endpoint = Endpoint::from_shared(endpoint.to_string())
            .map_err(|err| BackendError::new(format!("invalid QLN endpoint: {err}")))?
            .timeout(NETWORK_TIMEOUT)
            .connect_timeout(NETWORK_TIMEOUT);
        Ok(Self {
            client: lightnodepb::light_node_client::LightNodeClient::new(endpoint.connect_lazy()),
            tick_fallback: Arc::new(Mutex::new(QlnTickFallback::default())),
        })
    }
}

fn normalize_qln_tick_info(
    status: lightnodepb::TickStatus,
    fallback: &mut QlnTickFallback,
) -> TickInfo {
    let initial_tick = if status.initial_tick != 0 {
        fallback.epoch = Some(status.epoch);
        fallback.initial_tick = status.initial_tick;
        status.initial_tick
    } else {
        if fallback.epoch != Some(status.epoch) {
            fallback.epoch = Some(status.epoch);
            fallback.initial_tick = status.tick;
        }
        fallback.initial_tick
    };
    let tick_duration_ms = if status.tick_duration_ms == 0 {
        DEFAULT_TICK_DURATION_MS
    } else {
        status.tick_duration_ms
    };

    TickInfo {
        epoch: status.epoch,
        tick: status.tick,
        initial_tick,
        tick_duration_ms,
    }
}

#[async_trait]
impl NetworkBackend for QlnBackend {
    async fn tick_info(&self) -> Result<TickInfo, BackendError> {
        let mut client = self.client.clone();
        let response = client
            .get_status(Request::new(lightnodepb::GetStatusRequest {}))
            .await
            .map_err(|err| BackendError::new(format!("QLN status failed: {}", err.code())))?
            .into_inner();
        if !response.ok {
            return Err(BackendError::new("QLN status failed"));
        }
        let status = response
            .status
            .ok_or_else(|| BackendError::new("QLN status response is missing status"))?;
        let mut fallback = self
            .tick_fallback
            .lock()
            .map_err(|_| BackendError::new("QLN tick fallback lock is poisoned"))?;
        Ok(normalize_qln_tick_info(status, &mut fallback))
    }

    async fn tick_has_transactions(&self, tick: u32) -> Result<TickTransactions, BackendError> {
        let mut client = self.client.clone();
        let response = client
            .get_tick_transactions(Request::new(lightnodepb::GetTickTransactionsRequest {
                tick,
            }))
            .await
            .map_err(|err| {
                BackendError::new(format!("QLN tick-data query failed: {}", err.code()))
            })?
            .into_inner();
        qln_tick_has_transactions(response, tick)
    }

    async fn query_contract_function(
        &self,
        request: ContractFunctionRequest,
    ) -> Result<Vec<u8>, BackendError> {
        let mut client = self.client.clone();
        let response = client
            .query_contract_function(Request::new(lightnodepb::QueryContractFunctionRequest {
                contract_index: request.contract_index,
                input_type: u32::from(request.input_type),
                input: request.input,
            }))
            .await
            .map_err(|err| BackendError::new(format!("QLN contract query failed: {}", err.code())))?
            .into_inner();
        if response.ok {
            Ok(response.output)
        } else {
            Err(BackendError::new("QLN contract query failed"))
        }
    }

    async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError> {
        let mut client = self.client.clone();
        if let Some(bytes) = tx_bytes.get(72..76) {
            let tick = u32::from_le_bytes(bytes.try_into().expect("four-byte tick slice"));
            println!(
                "Broadcast digest for target {tick}: {}",
                bytes_to_hex(&transaction_digest(&tx_bytes))
            );
        }
        let response = client
            .broadcast_transaction(Request::new(lightnodepb::BroadcastTransactionRequest {
                tx_bytes,
            }))
            .await
            .map_err(|_| BackendError::new("QLN broadcast request failed"))?
            .into_inner();
        if response.ok {
            validate_transaction_id(response.tx_id)
        } else {
            Err(BackendError::new("QLN rejected broadcast request"))
        }
    }
}

fn qln_tick_has_transactions(
    response: lightnodepb::GetTickTransactionsResponse,
    requested_tick: u32,
) -> Result<TickTransactions, BackendError> {
    if !response.ok {
        return Err(BackendError::new("QLN tick-data query failed"));
    }
    if response.tick != requested_tick {
        return Err(BackendError::new("QLN returned a different tick"));
    }
    if !response.digests_complete {
        if !response.transaction_digests.is_empty() {
            return Err(BackendError::new("QLN returned an incomplete digest list"));
        }
        return Ok(response.has_transactions.into());
    }
    if response.transaction_digests.len() > 4096
        || response.has_transactions == response.transaction_digests.is_empty()
    {
        return Err(BackendError::new("QLN returned inconsistent tick digests"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut digests = Vec::with_capacity(response.transaction_digests.len());
    for bytes in response.transaction_digests {
        let digest: [u8; 32] = bytes
            .try_into()
            .map_err(|_| BackendError::new("QLN returned an invalid digest size"))?;
        if digest == [0; 32] || !seen.insert(digest) {
            return Err(BackendError::new("QLN returned a zero or duplicate digest"));
        }
        digests.push(digest);
    }
    Ok(TickTransactions {
        has_transactions: response.has_transactions,
        transaction_digests: Some(digests),
    })
}

fn validate_transaction_id(transaction_id: String) -> Result<String, BackendError> {
    if transaction_id.trim().is_empty() {
        Err(BackendError::new(
            "broadcast returned an empty transaction id",
        ))
    } else {
        Ok(transaction_id)
    }
}

fn normalize_rpc_tick_duration_ms(duration_ms: u32) -> u32 {
    if duration_ms == 0 {
        DEFAULT_TICK_DURATION_MS
    } else {
        duration_ms
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0F)] as char);
    }
    output
}

fn decode_hex(value: &str) -> Result<Vec<u8>, BackendError> {
    if !value.len().is_multiple_of(2) {
        return Err(BackendError::new("hex output has an odd length"));
    }
    if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BackendError::new("hex output contains a non-hex digit"));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk)
                .map_err(|err| BackendError::new(format!("invalid hex output: {err}")))?;
            u8::from_str_radix(text, 16)
                .map_err(|err| BackendError::new(format!("invalid hex output: {err}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn backend_debug_omits_transport_credentials() {
        let endpoint =
            "http://test-user:test-password@localhost:50051?token=test-token#test-fragment";
        let outputs = [
            format!("{:?}", RpcBackend::new(endpoint)),
            format!("{:?}", BobBackend::new(endpoint)),
        ];
        for output in outputs {
            for secret in ["test-user", "test-password", "test-token", "test-fragment"] {
                assert!(
                    !output.contains(secret),
                    "Debug exposed endpoint credentials"
                );
            }
        }
    }

    #[test]
    fn malformed_responses_do_not_echo_remote_content() {
        let value = json!({"error": "credential=example-secret"});
        assert_eq!(
            decode_rpc_contract_response(value.clone())
                .unwrap_err()
                .to_string(),
            "RPC contract response contains no base64 output"
        );
        assert_eq!(
            checked_u32_field(&value, "currentProcessingEpoch")
                .unwrap_err()
                .to_string(),
            "Bob response missing currentProcessingEpoch"
        );
        assert_eq!(
            qln_tick_has_transactions(
                lightnodepb::GetTickTransactionsResponse {
                    error: "credential=example-secret".into(),
                    ..Default::default()
                },
                1
            )
            .unwrap_err()
            .to_string(),
            "QLN tick-data query failed"
        );
    }

    #[tokio::test]
    async fn http_backends_reject_oversized_json_before_decoding() {
        for chunked in [false, true] {
            for bob in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let endpoint = format!("http://{}", listener.local_addr().unwrap());
                let server = std::thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    read_http_request(&mut stream);
                    let mut body = br#"{"responseData":"AA==","result":{"data":"0x00"}}"#.to_vec();
                    body.resize(16 * 1024 * 1024 + 1, b' ');
                    if chunked {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                        let _ = stream.write_all(b"\r\n0\r\n\r\n");
                    } else {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                });
                let backend: Box<dyn NetworkBackend> = if bob {
                    Box::new(BobBackend::new(&endpoint))
                } else {
                    Box::new(RpcBackend::new(&endpoint))
                };
                let result = backend
                    .query_contract_function(ContractFunctionRequest {
                        contract_index: 3,
                        input_type: 2,
                        input: vec![0; 32],
                    })
                    .await;
                server.join().unwrap();
                assert!(result.is_err(), "oversized JSON was accepted");
            }
        }
    }

    #[tokio::test]
    async fn http_read_errors_do_not_echo_response_bodies() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                read_http_request(&mut stream);
                let body = "credential=example-secret";
                write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let endpoint = format!("http://{address}");
        let backends: Vec<Box<dyn NetworkBackend>> = vec![
            Box::new(RpcBackend::new(&endpoint)),
            Box::new(BobBackend::new(&endpoint)),
        ];
        let mut errors = Vec::new();
        for backend in backends {
            errors.push(backend.tick_info().await.unwrap_err());
            errors.push(backend.tick_has_transactions(123).await.unwrap_err());
            errors.push(
                backend
                    .query_contract_function(ContractFunctionRequest {
                        contract_index: 3,
                        input_type: 1,
                        input: vec![0; 32],
                    })
                    .await
                    .unwrap_err(),
            );
        }
        server.join().unwrap();
        for error in errors {
            assert!(
                !format!("{error} {error:?}").contains("example-secret"),
                "remote content leaked"
            );
        }
    }

    #[test]
    fn rpc_response_decodes_supported_output_field() {
        assert_eq!(
            decode_rpc_contract_response(json!({"responseData": "AQID"})).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn hex_codec_roundtrips() {
        let bytes = [0, 1, 15, 16, 255];
        assert_eq!(decode_hex(&bytes_to_hex(&bytes)).unwrap(), bytes);
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("zz").is_err());
    }

    #[test]
    fn hex_output_accepts_only_ascii_hex_digit_pairs() {
        for invalid in ["+0", "+f", "00+a", "-1", " 0", "0 ", "é", "😀", "0\n"] {
            assert!(decode_hex(invalid).is_err(), "accepted non-hex input");
        }
        assert_eq!(decode_hex("00aAFF10").unwrap(), vec![0, 170, 255, 16]);
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn transaction_ids_must_not_be_empty() {
        assert!(validate_transaction_id(String::new()).is_err());
        assert!(validate_transaction_id("   ".to_string()).is_err());
        assert_eq!(validate_transaction_id("abc".to_string()).unwrap(), "abc");
    }

    #[test]
    fn bob_tick_data_accepts_array_and_wrapped_shapes() {
        assert!(!bob_tick_has_transactions(&json!([])).unwrap());
        assert!(bob_tick_has_transactions(&json!([{"tick": 1}])).unwrap());
        assert!(!bob_tick_has_transactions(&json!({"transfers": []})).unwrap());
        assert!(
            bob_tick_has_transactions(&json!({
                "transfers": [{"tick": 1}]
            }))
            .unwrap()
        );
        assert!(bob_tick_has_transactions(&json!({})).is_err());
    }

    #[test]
    fn qln_tick_data_distinguishes_empty_non_empty_and_error() {
        let response =
            |ok, has_transactions, error: &str| lightnodepb::GetTickTransactionsResponse {
                ok,
                tick: 1,
                has_transactions,
                error: error.to_string(),
                ..Default::default()
            };

        assert_eq!(
            qln_tick_has_transactions(response(true, false, ""), 1).unwrap(),
            false.into()
        );
        assert_eq!(
            qln_tick_has_transactions(response(true, true, ""), 1).unwrap(),
            true.into()
        );
        assert!(qln_tick_has_transactions(response(false, false, "unavailable"), 1).is_err());
        assert!(qln_tick_has_transactions(response(true, true, ""), 2).is_err());
    }

    #[test]
    fn qln_full_tick_digest_set_survives_protobuf_roundtrip() {
        use prost::Message;

        let digests: Vec<[u8; 32]> = (1_u32..=4096)
            .map(|slot| {
                let mut digest = [0; 32];
                digest[..4].copy_from_slice(&slot.to_le_bytes());
                digest
            })
            .collect();
        let mut response = lightnodepb::GetTickTransactionsResponse {
            ok: true,
            tick: u32::MAX,
            has_transactions: true,
            transaction_digests: digests.iter().map(|digest| digest.to_vec()).collect(),
            digests_complete: true,
            error: String::new(),
        };
        let encoded = response.encode_to_vec();
        assert_eq!(encoded.len(), 139_276);
        // tonic 0.12's default uncompressed receive limit is 4 MiB.
        assert!(encoded.len() < 4 * 1024 * 1024);
        let decoded = lightnodepb::GetTickTransactionsResponse::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(
            qln_tick_has_transactions(decoded, u32::MAX).unwrap(),
            TickTransactions {
                has_transactions: true,
                transaction_digests: Some(digests),
            }
        );
        let mut extra = vec![0; 32];
        extra[..4].copy_from_slice(&4097_u32.to_le_bytes());
        response.transaction_digests.push(extra);
        assert!(qln_tick_has_transactions(response, u32::MAX).is_err());
    }

    #[test]
    fn qln_complete_digests_compare_the_entire_signed_transaction_hash() {
        let digest = transaction_digest(b"signed transaction");
        let mut different = digest;
        different[31] ^= 1;
        let response = lightnodepb::GetTickTransactionsResponse {
            ok: true,
            tick: 42,
            has_transactions: true,
            transaction_digests: vec![digest.to_vec()],
            digests_complete: true,
            error: String::new(),
        };
        let observation = qln_tick_has_transactions(response.clone(), 42).unwrap();
        assert_eq!(
            observation,
            TickTransactions {
                has_transactions: true,
                transaction_digests: Some(vec![digest]),
            }
        );
        assert_eq!(observation.contains(&digest), Some(true));
        assert_eq!(observation.contains(&different), Some(false));
        assert_eq!(TickTransactions::from(true).contains(&digest), None);
        assert_ne!(digest, transaction_digest(b"signed transaction!"));

        let empty = qln_tick_has_transactions(
            lightnodepb::GetTickTransactionsResponse {
                has_transactions: false,
                transaction_digests: vec![],
                ..response.clone()
            },
            42,
        )
        .unwrap();
        assert_eq!(
            empty,
            TickTransactions {
                has_transactions: false,
                transaction_digests: Some(vec![])
            }
        );
        assert_eq!(empty.contains(&digest), Some(false));

        for digests in [
            vec![vec![1; 31]],
            vec![vec![0; 32]],
            vec![digest.to_vec(); 2],
            vec![],
            vec![vec![1; 32]; 4097],
        ] {
            assert!(
                qln_tick_has_transactions(
                    lightnodepb::GetTickTransactionsResponse {
                        transaction_digests: digests,
                        ..response.clone()
                    },
                    42
                )
                .is_err()
            );
        }
        assert!(
            qln_tick_has_transactions(
                lightnodepb::GetTickTransactionsResponse {
                    digests_complete: false,
                    ..response.clone()
                },
                42
            )
            .is_err()
        );
        assert!(
            qln_tick_has_transactions(
                lightnodepb::GetTickTransactionsResponse {
                    has_transactions: false,
                    ..response
                },
                42
            )
            .is_err()
        );
    }

    #[test]
    fn rpc_missing_tick_duration_uses_stable_fallback() {
        assert_eq!(normalize_rpc_tick_duration_ms(0), DEFAULT_TICK_DURATION_MS);
        assert_eq!(normalize_rpc_tick_duration_ms(750), 750);
    }

    #[test]
    fn qln_missing_tick_metadata_uses_stable_epoch_fallbacks() {
        let mut fallback = QlnTickFallback::default();

        let first = normalize_qln_tick_info(qln_tick_status(7, 1_000, 0, 0), &mut fallback);
        let later = normalize_qln_tick_info(qln_tick_status(7, 1_020, 0, 0), &mut fallback);

        assert_eq!(
            first,
            TickInfo {
                epoch: 7,
                tick: 1_000,
                initial_tick: 1_000,
                tick_duration_ms: DEFAULT_TICK_DURATION_MS,
            }
        );
        assert_eq!(
            later,
            TickInfo {
                epoch: 7,
                tick: 1_020,
                initial_tick: 1_000,
                tick_duration_ms: DEFAULT_TICK_DURATION_MS,
            }
        );
    }

    #[test]
    fn qln_tick_fallback_resets_for_a_new_epoch() {
        let mut fallback = QlnTickFallback::default();
        let _ = normalize_qln_tick_info(qln_tick_status(7, 1_000, 0, 0), &mut fallback);

        let next_epoch = normalize_qln_tick_info(qln_tick_status(8, 2_000, 0, 0), &mut fallback);

        assert_eq!(next_epoch.initial_tick, 2_000);
        assert_eq!(next_epoch.tick_duration_ms, DEFAULT_TICK_DURATION_MS);
    }

    #[test]
    fn qln_reported_tick_metadata_takes_precedence() {
        let mut fallback = QlnTickFallback::default();

        let reported = normalize_qln_tick_info(qln_tick_status(7, 1_020, 970, 750), &mut fallback);
        let later_missing = normalize_qln_tick_info(qln_tick_status(7, 1_021, 0, 0), &mut fallback);

        assert_eq!(
            reported,
            TickInfo {
                epoch: 7,
                tick: 1_020,
                initial_tick: 970,
                tick_duration_ms: 750,
            }
        );
        assert_eq!(later_missing.initial_tick, 970);
        assert_eq!(later_missing.tick_duration_ms, DEFAULT_TICK_DURATION_MS);
    }

    fn qln_tick_status(
        epoch: u32,
        tick: u32,
        initial_tick: u32,
        tick_duration_ms: u32,
    ) -> lightnodepb::TickStatus {
        lightnodepb::TickStatus {
            epoch,
            tick,
            initial_tick,
            tick_duration_ms,
            aligned_votes: 451,
            misaligned_votes: 0,
        }
    }

    #[tokio::test]
    async fn rpc_contract_query_uses_live_route() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_http_request(&mut stream);
            let body = r#"{"responseData":"AQID"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        let backend = RpcBackend::new(&format!("http://{address}"));

        let output = backend
            .query_contract_function(ContractFunctionRequest {
                contract_index: 3,
                input_type: 2,
                input: vec![1, 2, 3],
            })
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(output, vec![1, 2, 3]);
        assert!(request.starts_with("POST /live/v1/querySmartContract HTTP/1.1\r\n"));
        assert!(request.contains(r#""contractIndex":3"#));
        assert!(request.contains(r#""inputType":2"#));
        assert!(request.contains(r#""inputSize":3"#));
        assert!(request.contains(r#""requestData":"AQID""#));
    }

    #[tokio::test]
    async fn rpc_broadcast_requires_at_least_one_peer_and_a_transaction_id() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            [
                r#"{"peersBroadcasted":1,"encodedTransaction":"AQID","transactionId":"abc"}"#,
                r#"{"peersBroadcasted":0,"encodedTransaction":"AQID","transactionId":"def"}"#,
                r#"{"peersBroadcasted":-1,"encodedTransaction":"AQID","transactionId":"ghi"}"#,
                r#"{"peersBroadcasted":1,"encodedTransaction":"AQID","transactionId":""}"#,
            ]
            .into_iter()
            .map(|body| {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                request
            })
            .collect::<Vec<_>>()
        });
        let backend = RpcBackend::new(&format!("http://{address}"));

        assert_eq!(
            backend.broadcast_transaction(vec![1, 2, 3]).await.unwrap(),
            "abc"
        );
        assert_eq!(
            backend
                .broadcast_transaction(vec![1, 2, 3])
                .await
                .unwrap_err()
                .to_string(),
            "RPC broadcast reached no peers"
        );
        assert_eq!(
            backend
                .broadcast_transaction(vec![1, 2, 3])
                .await
                .unwrap_err()
                .to_string(),
            "RPC broadcast returned invalid peersBroadcasted value: -1"
        );
        assert_eq!(
            backend
                .broadcast_transaction(vec![1, 2, 3])
                .await
                .unwrap_err()
                .to_string(),
            "broadcast returned an empty transaction id"
        );
        for request in server.join().unwrap() {
            assert!(request.starts_with("POST /live/v1/broadcast-transaction HTTP/1.1\r\n"));
            assert!(request.contains(r#""encodedTransaction":"AQID""#));
        }
    }

    #[tokio::test]
    async fn rpc_tick_data_rejects_a_different_tick_even_when_empty() {
        for hashes in [json!([]), json!(["abc"])] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                read_http_request(&mut stream);
                let body = json!({"tickData": {
                    "tickNumber": 124, "epoch": 1, "computorIndex": 0,
                    "timestamp": "", "varStruct": "", "timeLock": "",
                    "transactionHashes": hashes, "contractFees": [], "signature": "",
                }})
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let result = RpcBackend::new(&endpoint).tick_has_transactions(123).await;
            server.join().unwrap();
            assert_eq!(
                result,
                Err(BackendError::new("RPC returned a different tick"))
            );
        }
    }

    #[tokio::test]
    async fn rpc_tick_data_query_uses_query_route() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            [
                r#"{"tickData":null}"#,
                r#"{"tickData":{"tickNumber":123,"epoch":1,"computorIndex":0,"timestamp":"","varStruct":"","timeLock":"","transactionHashes":[],"contractFees":[],"signature":""}}"#,
                r#"{"tickData":{"tickNumber":123,"epoch":1,"computorIndex":0,"timestamp":"","varStruct":"","timeLock":"","transactionHashes":["abc"],"contractFees":[],"signature":""}}"#,
            ]
            .into_iter()
            .map(|body| {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                request
            })
            .collect::<Vec<_>>()
        });
        let backend = RpcBackend::new(&format!("http://{address}"));

        assert_eq!(
            backend.tick_has_transactions(123).await.unwrap(),
            false.into()
        );
        assert_eq!(
            backend.tick_has_transactions(123).await.unwrap(),
            false.into()
        );
        assert_eq!(
            backend.tick_has_transactions(123).await.unwrap(),
            true.into()
        );
        for request in server.join().unwrap() {
            assert!(request.starts_with("POST /query/v1/getTickData HTTP/1.1\r\n"));
            assert!(request.contains(r#""tickNumber":123"#));
        }
    }

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        let (header_end, content_length) = loop {
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0, "connection closed before request headers");
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let header_end = header_end + 4;
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .or_else(|| line.strip_prefix("Content-Length:"))
                    })
                    .map(str::trim)
                    .map(str::parse::<usize>)
                    .transpose()
                    .unwrap()
                    .unwrap_or_default();
                break (header_end, content_length);
            }
        };
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0, "connection closed before request body");
            request.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(request).unwrap()
    }
}
