//! Bounded HTTP transport; SCAPI remains the source of RPC wire models.
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::{Client, Response};
use scapi::rpc::post::{BroadcastTransactionRequest, BroadcastTransactionResponse};
use scapi::rpc::{GetTickDataRequest, GetTickDataResponse, TickInfoResponse};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
type Result<T> = std::result::Result<T, &'static str>;

async fn read_json<T: DeserializeOwned>(mut response: Response, limit: usize) -> Result<T> {
    // Error bodies are never needed and may contain echoed secrets.
    if !response.status().is_success() {
        return Err("HTTP request failed");
    }
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err("HTTP response exceeds byte limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "HTTP body read failed")?
    {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err("HTTP response exceeds byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "Invalid HTTP JSON response")
}

#[derive(Clone)]
pub(crate) struct BoundedRpcClient {
    base_url: Cow<'static, str>,
    http: Client,
}

impl BoundedRpcClient {
    pub(crate) fn with_base_url(base_url: Cow<'static, str>) -> Self {
        Self {
            base_url,
            http: Client::new(),
        }
    }

    fn url_for(&self, path: &str) -> String {
        crate::config::append_endpoint_path(&self.base_url, path)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .get(self.url_for(path))
            .send()
            .await
            .map_err(|_| "HTTP request failed")?;
        read_json(response, MAX_RESPONSE_BYTES).await
    }

    async fn post_json<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> Result<T> {
        self.post_json_url(&self.url_for(path), payload).await
    }

    async fn post_json_url<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        url: &str,
        payload: &P,
    ) -> Result<T> {
        let response = self
            .http
            .post(url)
            .json(payload)
            .send()
            .await
            .map_err(|_| "HTTP request failed")?;
        read_json(response, MAX_RESPONSE_BYTES).await
    }

    pub(crate) async fn post_json_value<P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> Result<Value> {
        self.post_json(path, payload).await
    }
}

pub(crate) async fn get_tick_info_with(client: &BoundedRpcClient) -> Result<TickInfoResponse> {
    client.get_json("tick-info").await
}

pub(crate) async fn get_tick_data_with(
    client: &BoundedRpcClient,
    tick: u32,
) -> Result<GetTickDataResponse> {
    client
        .post_json("getTickData", &GetTickDataRequest { tick_number: tick })
        .await
}

pub(crate) async fn broadcast_transaction_with(
    client: &BoundedRpcClient,
    encoded_transaction: String,
) -> Result<BroadcastTransactionResponse> {
    client
        .post_json(
            "broadcast-transaction",
            &BroadcastTransactionRequest {
                encoded_transaction,
            },
        )
        .await
}

pub(crate) struct BoundedBobClient {
    endpoint: String,
    rpc: BoundedRpcClient,
    next_id: AtomicU64,
}

impl BoundedBobClient {
    pub(crate) fn with_base_url(endpoint: &str) -> Self {
        Self {
            endpoint: crate::config::append_endpoint_path(endpoint, "qubic"),
            rpc: BoundedRpcClient::with_base_url(Cow::Borrowed("")),
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response: Value = self
            .rpc
            .post_json_url(
                &self.endpoint,
                &json!({
                    "jsonrpc": "2.0", "method": method, "params": params, "id": id,
                }),
            )
            .await?;
        let envelope = response.get("jsonrpc").is_some()
            || (response.get("id").is_some()
                && (response.get("result").is_some() || response.get("error").is_some()));
        if envelope && response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err("Bob response ID does not match request");
        }
        Ok(response)
    }

    pub(crate) async fn qubic_status(&self) -> Result<Value> {
        self.call("qubic_status", Value::Array(vec![])).await
    }

    pub(crate) async fn qubic_get_transfers(&self, params: Value) -> Result<Value> {
        self.call("qubic_getTransfers", Value::Array(vec![params]))
            .await
    }

    pub(crate) async fn qubic_broadcast_transaction(&self, signed_tx: String) -> Result<Value> {
        self.call("qubic_broadcastTransaction", json!([signed_tx]))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    type Requests = Vec<(String, Value)>;

    fn server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Requests>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                requests.push((
                    first,
                    if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    },
                ));
                let _ = stream.write_all(response.as_bytes());
            }
            requests
        });
        (endpoint, task)
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn request_paths_preserve_endpoint_query_parameters() {
        let body = r#"{"tickInfo":{"tick":123,"duration":1,"epoch":2,"initialTick":3}}"#;
        let (endpoint, task) = server(vec![json_response(body), json_response("{}")]);
        let rpc = BoundedRpcClient::with_base_url(Cow::Owned(format!(
            "{endpoint}/live/v1?token=a%2Fb/#fragment"
        )));
        get_tick_info_with(&rpc).await.unwrap();
        let bob = BoundedBobClient::with_base_url(&format!("{endpoint}?token=a%2Fb/#fragment"));
        bob.qubic_status().await.unwrap();
        let requests = task.join().unwrap();
        assert_eq!(
            requests[0].0,
            "GET /live/v1/tick-info?token=a%2Fb/ HTTP/1.1\r\n"
        );
        assert_eq!(requests[1].0, "POST /qubic?token=a%2Fb/ HTTP/1.1\r\n");
    }

    #[tokio::test]
    async fn bob_response_ids_must_match_the_request() {
        for response in [
            json!({"jsonrpc":"2.0","id":2,"result":{}}),
            json!({"jsonrpc":"2.0","id":"1","result":{}}),
            json!({"jsonrpc":"2.0","id":null,"result":{}}),
            json!({"jsonrpc":"2.0","result":{}}),
            json!({"id":2,"result":{}}),
        ] {
            let (endpoint, task) = server(vec![json_response(&response.to_string())]);
            let result = BoundedBobClient::with_base_url(&endpoint)
                .qubic_status()
                .await;
            task.join().unwrap();
            assert_eq!(result, Err("Bob response ID does not match request"));
        }
        for response in [
            json!({"jsonrpc":"2.0","id":1,"result":{}}),
            json!({"id":1,"result":{}}),
            json!({"result":{}}),
            json!({"id":"legacy-transaction-id"}),
            json!([]),
        ] {
            let (endpoint, task) = server(vec![json_response(&response.to_string())]);
            let result = BoundedBobClient::with_base_url(&endpoint)
                .qubic_status()
                .await;
            task.join().unwrap();
            assert_eq!(result, Ok(response));
        }
    }

    #[tokio::test]
    async fn bounded_bob_requests_match_pinned_scapi() {
        let (endpoint, task) = server(vec![json_response("{}"); 8]);
        let old = scapi::bob::BobRpcClient::with_base_url(&endpoint);
        let new = BoundedBobClient::with_base_url(&endpoint);
        let filter = json!({"identity":"", "fromTick":123,"toTick":123});
        let params = json!([{"nonce":4,"scIndex":3,"funcNumber":2,"data":"0x00"}]);
        old.qubic_status().await.unwrap();
        old.qubic_get_transfers(filter.clone()).await.unwrap();
        old.call("qubic_querySmartContract", params.clone())
            .await
            .unwrap();
        old.qubic_broadcast_transaction("0001").await.unwrap();
        new.qubic_status().await.unwrap();
        new.qubic_get_transfers(filter).await.unwrap();
        new.call("qubic_querySmartContract", params).await.unwrap();
        new.qubic_broadcast_transaction("0001".into())
            .await
            .unwrap();
        let requests = task.join().unwrap();
        assert_eq!(&requests[..4], &requests[4..]);
    }

    #[tokio::test]
    async fn bounded_rpc_requests_match_pinned_scapi() {
        let bodies = [
            r#"{"tickInfo":{"tick":123,"duration":1,"epoch":2,"initialTick":3}}"#,
            r#"{"tickData":null}"#,
            r#"{"responseData":"AA=="}"#,
            r#"{"peersBroadcasted":1,"encodedTransaction":"AA==","transactionId":"id"}"#,
        ];
        let (endpoint, task) = server(
            bodies
                .iter()
                .cycle()
                .take(8)
                .map(|body| json_response(body))
                .collect(),
        );
        let old = scapi::rpc::RpcClient::with_base_url(Cow::Owned(endpoint.clone()));
        let new = BoundedRpcClient::with_base_url(Cow::Owned(endpoint));
        let payload = json!({"contractIndex":3,"inputType":2,"inputSize":1,"requestData":"AA=="});
        scapi::rpc::get_tick_info_with(&old).await.unwrap();
        scapi::rpc::get_tick_data_with(&old, 123).await.unwrap();
        old.post_json_value("querySmartContract", &payload)
            .await
            .unwrap();
        scapi::rpc::post::broadcast_transaction_with(&old, "AA==".into())
            .await
            .unwrap();
        get_tick_info_with(&new).await.unwrap();
        get_tick_data_with(&new, 123).await.unwrap();
        new.post_json_value("querySmartContract", &payload)
            .await
            .unwrap();
        broadcast_transaction_with(&new, "AA==".into())
            .await
            .unwrap();
        let requests = task.join().unwrap();
        assert_eq!(&requests[..4], &requests[4..]);
    }

    #[tokio::test]
    async fn body_limit_is_inclusive_for_fixed_and_chunked_responses() {
        for chunked in [false, true] {
            for size in [32, 33] {
                let body = format!("{{}}{}", " ".repeat(size - 2));
                let response = if chunked {
                    format!(
                        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{size:x}\r\n{body}\r\n0\r\n\r\n"
                    )
                } else {
                    json_response(&body)
                };
                let (endpoint, task) = server(vec![response]);
                let response = Client::new().get(endpoint).send().await.unwrap();
                let actual = read_json::<Value>(response, 32).await;
                assert_eq!(
                    actual,
                    if size == 32 {
                        Ok(json!({}))
                    } else {
                        Err("HTTP response exceeds byte limit")
                    }
                );
                task.join().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn valid_json_in_truncated_http_body_is_not_accepted() {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{}\r\n",
        ] {
            let (endpoint, task) = server(vec![response.to_string()]);
            let response = Client::new().get(endpoint).send().await.unwrap();
            assert_eq!(
                read_json::<Value>(response, 32).await,
                Err("HTTP body read failed")
            );
            task.join().unwrap();
        }
    }

    #[tokio::test]
    async fn incomplete_success_body_remains_cancellable_after_valid_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}")
                .unwrap();
            let _ = done_rx.recv_timeout(Duration::from_secs(3));
        });
        let response = Client::new().get(endpoint).send().await.unwrap();
        let result =
            tokio::time::timeout(Duration::from_millis(100), read_json::<Value>(response, 32))
                .await;
        let _ = done_tx.send(());
        task.join().unwrap();
        assert!(
            result.is_err(),
            "valid JSON must not bypass HTTP body completion"
        );
    }

    #[tokio::test]
    async fn http_errors_do_not_wait_for_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 999999999\r\n\r\n",
                )
                .unwrap();
            // Keep the body unavailable until the assertion has completed.
            let _ = done_rx.recv_timeout(Duration::from_secs(3));
        });
        let response = Client::new().get(endpoint).send().await.unwrap();
        let result =
            tokio::time::timeout(Duration::from_secs(1), read_json::<Value>(response, 32)).await;
        let _ = done_tx.send(());
        task.join().unwrap();
        assert_eq!(result.unwrap(), Err("HTTP request failed"));
    }
}
