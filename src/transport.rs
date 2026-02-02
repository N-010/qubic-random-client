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
            "scapi tx: build+send amount={amount} tick={tick} input_type={input_type} contract={contract_id}",
            amount = amount,
            tick = tick,
            input_type = self.input_type,
            contract_id = console::shorten_id(&self.contract_id.to_string())
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

#[cfg(test)]
mod tests {
    use super::{
        ScTransport, ScapiClient, ScapiContractTransport, ScapiRpcClient, build_payload,
        build_tx_bytes,
    };
    use crate::balance::BalanceState;
    use crate::protocol::RevealAndCommitInput;
    use scapi::{QubicId, QubicWallet};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    struct MockServer {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    #[derive(Clone)]
    struct MockResponse {
        status: u16,
        body: String,
    }

    impl MockServer {
        fn start(responses: Arc<Mutex<HashMap<(String, String), MockResponse>>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let handle = thread::spawn(move || {
                listener.set_nonblocking(true).expect("set nonblocking");
                while !stop_flag.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let responses = responses.clone();
                            thread::spawn(move || handle_connection(stream, responses));
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                addr,
                stop,
                handle: Some(handle),
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn handle_connection(
        mut stream: TcpStream,
        responses: Arc<Mutex<HashMap<(String, String), MockResponse>>>,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
        let mut buffer = Vec::new();
        let mut temp = [0u8; 1024];
        loop {
            match stream.read(&mut temp) {
                Ok(0) => break,
                Ok(n) => {
                    buffer.extend_from_slice(&temp[..n]);
                    if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => return,
            }
        }
        let request = String::from_utf8_lossy(&buffer);
        let mut lines = request.lines();
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        let mut content_length = 0usize;
        let mut is_chunked = false;
        let mut expect_continue = false;
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim().to_ascii_lowercase();
                if name == "content-length" {
                    content_length = value.parse::<usize>().unwrap_or(0);
                }
                if name == "transfer-encoding" && value.contains("chunked") {
                    is_chunked = true;
                }
                if name == "expect" && value.contains("100-continue") {
                    expect_continue = true;
                }
            }
        }

        let header_len = buffer
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
            .unwrap_or(buffer.len());
        let mut body = buffer[header_len..].to_vec();
        if expect_continue {
            let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
            let _ = stream.flush();
        }
        if content_length > 0 {
            while body.len() < content_length {
                match stream.read(&mut temp) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&temp[..n]),
                    Err(_) => break,
                }
            }
        } else if is_chunked {
            let end_marker = b"\r\n0\r\n\r\n";
            while !body.windows(end_marker.len()).any(|w| w == end_marker) {
                match stream.read(&mut temp) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&temp[..n]),
                    Err(_) => break,
                }
            }
        } else {
            loop {
                match stream.read(&mut temp) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&temp[..n]),
                    Err(_) => break,
                }
            }
        }

        let response = responses
            .lock()
            .expect("lock responses")
            .get(&(method.clone(), path.clone()))
            .cloned()
            .unwrap_or(MockResponse {
                status: 404,
                body: "not found".to_string(),
            });

        let status_text = match response.status {
            200 => "OK",
            400 => "Bad Request",
            500 => "Internal Server Error",
            _ => "Error",
        };
        let body = response.body;
        let response_text = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.status,
            status_text,
            body.len(),
            body
        );
        let _ = stream.write_all(response_text.as_bytes());
        let _ = stream.flush();
    }

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

    #[tokio::test]
    async fn scapi_rpc_client_maps_tick_info() {
        // Mock /tick-info response should map into TickInfo.
        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("GET".to_string(), "/tick-info".to_string()),
            MockResponse {
                status: 200,
                body: r#"{"tickInfo":{"tick":12,"duration":10,"epoch":3,"initialTick":1}}"#
                    .to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let client = ScapiRpcClient::new(server.base_url());

        let info = client.get_tick_info().await.expect("tick info");
        assert_eq!(info.tick, 12);
        assert_eq!(info.epoch, 3);
    }

    #[tokio::test]
    async fn scapi_rpc_client_maps_balance() {
        // Mock /balances response should parse to u64.
        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("GET".to_string(), "/balances/IDENTITY".to_string()),
            MockResponse {
                status: 200,
                body: r#"{"balance":{"id":"ID","balance":"42","validForTick":1,"latestIncomingTransferTick":2,"latestOutgoingTransferTick":3}}"#
                    .to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let client = ScapiRpcClient::new(server.base_url());

        let balances = client.get_balances("IDENTITY").await.expect("balances");
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].amount, 42);
    }

    #[tokio::test]
    async fn scapi_rpc_client_rejects_invalid_balance() {
        // Non-numeric balance should error.
        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("GET".to_string(), "/balances/IDENTITY".to_string()),
            MockResponse {
                status: 200,
                body: r#"{"balance":{"id":"ID","balance":"NaN","validForTick":1,"latestIncomingTransferTick":2,"latestOutgoingTransferTick":3}}"#
                    .to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let client = ScapiRpcClient::new(server.base_url());

        let err = client
            .get_balances("IDENTITY")
            .await
            .expect_err("expected error");
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
            .send_reveal_and_commit(input, 1, 10)
            .await
            .expect_err("expected error");
        assert!(err.message.contains("insufficient balance"));
    }

    #[tokio::test]
    async fn scapi_contract_transport_fails_on_broadcast_error() {
        // Broadcast endpoint errors should surface.
        let seed = "a".repeat(55);
        let wallet = QubicWallet::from_seed(&seed).expect("wallet");
        let contract_id =
            QubicId::from_str("DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG")
                .expect("contract");

        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("POST".to_string(), "/broadcast-transaction".to_string()),
            MockResponse {
                status: 500,
                body: "boom".to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let transport =
            ScapiContractTransport::new(server.base_url(), wallet, contract_id, 1, balance_state);

        let input = RevealAndCommitInput {
            revealed_bits: [0u8; 512],
            committed_digest: [0u8; 32],
        };
        let err = transport
            .send_reveal_and_commit(input, 1, 10)
            .await
            .expect_err("expected error");
        assert!(err.message.contains("broadcast transaction failed"));
    }

    #[tokio::test]
    async fn scapi_contract_transport_succeeds() {
        // Successful balance check and broadcast returns tx id.
        let seed = "a".repeat(55);
        let wallet = QubicWallet::from_seed(&seed).expect("wallet");
        let contract_id =
            QubicId::from_str("DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG")
                .expect("contract");

        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("POST".to_string(), "/broadcast-transaction".to_string()),
            MockResponse {
                status: 200,
                body:
                    r#"{"peersBroadcasted":1,"encodedTransaction":"AAA","transactionId":"tx123"}"#
                        .to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let balance_state = Arc::new(BalanceState::new());
        balance_state.set_amount(100);
        let transport =
            ScapiContractTransport::new(server.base_url(), wallet, contract_id, 1, balance_state);

        let input = RevealAndCommitInput {
            revealed_bits: [0u8; 512],
            committed_digest: [0u8; 32],
        };
        let tx_id = transport
            .send_reveal_and_commit(input, 1, 10)
            .await
            .expect("tx id");
        assert_eq!(tx_id, "tx123");
    }

    #[tokio::test]
    async fn scapi_rpc_client_reports_http_errors() {
        // Non-2xx should be surfaced as RPC HTTP error.
        let responses = Arc::new(Mutex::new(HashMap::from([(
            ("GET".to_string(), "/tick-info".to_string()),
            MockResponse {
                status: 500,
                body: "boom".to_string(),
            },
        )])));
        let server = MockServer::start(responses);
        let client = ScapiRpcClient::new(server.base_url());

        let err = client.get_tick_info().await.expect_err("expected error");
        let message = err.message;
        assert!(
            message.contains("RPC HTTP error") || message.contains("error sending request"),
            "unexpected error message: {message}"
        );
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
