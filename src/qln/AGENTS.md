# QLN gRPC Architecture

## Goal
Add a third backend for `RandomCient` that works with `QubicLightNode` over gRPC, alongside existing `RPC` and `Bob`.

## Current extension points
- `src/transport.rs`
  - `ScapiClient` for read operations (`tick`, `balance`).
  - `ScTransport` for write operation (`send_reveal_and_commit`).
- `src/tick_data_watcher.rs`
  - `TickDataFetcher` for empty-tick verification.
- `src/app.rs`
  - Backend wiring and runtime composition.

## Target backend model
Replace boolean backend selection with explicit enum:

```rust
enum Backend {
    Rpc,
    Bob,
    QlnGrpc,
}
```

Runtime config fields:
- `backend: Backend`
- `grpc_endpoint: String` (example: `http://127.0.0.1:50051`)
- optional fallback fields for hybrid mode:
  - `endpoint: String` (RPC fallback for send path)

## QLN module layout (`src/qln`)
- `src/qln/mod.rs` - exports and integration entry points.
- `src/qln/client.rs` - thin typed gRPC client wrapper (tonic stub + retries/timeouts).
- `src/qln/mappers.rs` - maps protobuf responses into internal domain types.
- `src/qln/read_adapter.rs` - `ScapiClient` implementation over gRPC:
  - `GetStatus -> TickInfo`
  - `GetBalance -> Vec<BalanceEntry>`
- `src/qln/tick_data_fetcher.rs` - `TickDataFetcher` implementation:
  - `GetTickTransactions(tick)` and `Some(())` if any tx exists.
- `src/qln/transport.rs` - `ScTransport` implementation (if write RPC exists in proto).
- `src/qln/errors.rs` - typed error model + conversions into `TransportError`.

## LightNode proto coverage
Current `QubicLightNode/proto/lightnode.proto` methods:
- `GetStatus`
- `GetBalance`
- `GetTickTransactions`

These are enough for:
- tick polling,
- balance watcher,
- tick data validation.

They are not enough for:
- broadcast of signed transaction from `RandomCient`.

## Send path strategies
### Strategy A (minimal, no QLN changes)
- Read path via gRPC (`GetStatus`, `GetBalance`, `GetTickTransactions`).
- Send path remains existing RPC (`ScapiContractTransport`).
- Result: hybrid backend `QlnGrpc + RpcSend`.

### Strategy B (full gRPC backend)
Extend `lightnode.proto`:
- `BroadcastTransaction(BroadcastTransactionRequest) returns (BroadcastTransactionResponse)`

Proposed messages:
- `BroadcastTransactionRequest`
  - `bytes tx_bytes` or `string tx_hex`
- `BroadcastTransactionResponse`
  - `bool ok`
  - `string tx_id`
  - `string error`

Then implement `src/qln/transport.rs` and make `QlnGrpc` fully independent from classic RPC.

## Integration in app runtime
In `src/app.rs`, backend wiring should be:
- `Backend::Rpc` -> current SCAPI adapters.
- `Backend::Bob` -> current Bob adapters.
- `Backend::QlnGrpc` -> QLN read adapters + send adapter:
  - Strategy A: RPC send transport.
  - Strategy B: gRPC send transport.

All existing pipeline, balance watcher, tick watcher, and shutdown flow remain unchanged because abstractions already fit.

## Build and dependency plan
- Add gRPC deps:
  - `tonic`
  - `prost`
  - `tonic-build` (build dependency)
- Add `build.rs` in `RandomCient` to generate client from `QubicLightNode/proto/lightnode.proto`.
- Keep generated code behind `src/qln/client.rs` wrapper to isolate protobuf details.

## Error and resiliency rules
- Every gRPC call has timeout and contextual error.
- If `GetStatus` fails, keep current retry behavior in tick source.
- If `GetTickTransactions` fails, keep current requeue behavior in `TickDataWatcher`.
- Parse numeric fields with checked conversions (`u32`, `u64`) and explicit overflow errors.

## Migration sequence
1. Introduce backend enum and new CLI options.
2. Implement QLN read adapters and tick data fetcher.
3. Wire `Backend::QlnGrpc` in `app.rs` using Strategy A.
4. Validate behavior with existing pipeline tests + new mapper tests.
5. Add Strategy B only after LightNode proto supports broadcast.

## Test plan
- Unit tests:
  - proto -> `TickInfo` mapping,
  - proto -> `BalanceEntry` mapping,
  - tick-data empty/non-empty mapping.
- Integration tests:
  - run pipeline with mocked `ScapiClient`/`TickDataFetcher` from QLN adapter behavior.
- Regression:
  - existing `RPC` and `Bob` paths must pass unchanged.
