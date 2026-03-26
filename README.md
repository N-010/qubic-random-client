# Random Client

Client for the Random smart contract (SC) on the Qubic network. Implements the
commit -> reveal cycle for the `RANDOM::RevealAndCommit()` procedure and sends
transactions via the selected backend.

## What it does
- Generates 4096 bits of entropy and submits a commit (digest).
- After a configured number of ticks, sends the reveal for the previous commit
  plus a new commit.
- Monitors balance and pauses the pipeline when funds are insufficient.
- Supports multiple parallel senders.

A short protocol overview is in `docs/ShortDescriptio.txt`, the client
architecture is in `docs/Architecture.md`, and the input structure is in
`docs/Random.h`.

## Requirements
- Rust (edition 2024).
- Access to one supported backend endpoint:
  - RPC (default: `https://rpc.qubic.org`)
  - Bob JSON-RPC (default: `http://localhost:40420/qubic`)
  - QubicLightNode gRPC (default: `http://127.0.0.1:50051`)

## Build
```bash
cargo build
```

## Run
```bash
cargo run -- --seed <your_seed>
```

If `--seed` is not provided, the seed is read from stdin/TTY.

### Seed format
- Exactly 55 characters.
- Lowercase `a-z` only.

## CLI options
```text
--seed <seed>                             Seed (55 chars, a-z)
--senders <n>                             Number of senders (default: 3; 0 = auto)
--reveal-after <ticks>                    Reveal delay in ticks (default: 3)
--reveal-guard <ticks>                    Guard ticks before reveal send (default: 6)
--collateral <amount>                     Collateral amount (must be 1/10/.../1_000_000_000, default: 10000)
--pipelines <n>                           Number of parallel pipelines (default: 3)
--workers <n>                             Runtime threads (0 = auto)
--tick-poll-ms <ms>                       Tick polling interval (default: 1000)
--balance-ms <ms>                         Balance print interval (default: 600)
--empty-check-ms <ms>                     Reveal empty-tick check interval (default: 600)
--reveal-verify-after <ticks>             Minimum tick distance before reveal checks (default: 10)
--stop-before-epoch-end-secs <secs>       Pause before epoch end (default: 600)
--resume-after-epoch-start-ticks <ticks>  Resume delay after new epoch starts (default: 50)
--backend <backend>                       Backend: rpc, bob, grpc (default: rpc)
--endpoint <url>                          Endpoint for the selected backend
```

## Parameter details
### --seed
Required secret used to derive commits. Must be exactly 55 lowercase letters.
Changing the seed changes all generated commits and reveals. Keep it private.

### --senders
Maximum number of concurrent transaction sends. Higher values increase throughput but
also increase pressure on the endpoint. Set to 1 for strictly sequential
sending. If set to 0, the value is replaced with available CPU parallelism.

### --reveal-after
Base delay (in ticks) between a commit and its reveal. Larger values spread out
reveal traffic and reduce overlap but increase the time until a reveal is sent.
Smaller values make the cycle faster but can be less tolerant to network delays.
Value must be a positive multiple of 3 to keep SC stream alignment.

### --reveal-guard
Guard window (in ticks) before the scheduled reveal tick. The pipeline waits
until `now_tick >= reveal_send_at_tick - guard` before sending a reveal+commit.
Larger values send reveals earlier; smaller values wait closer to the reveal
tick.

### --collateral
Amount sent with each commit/reveal transaction. Must be one of SC collateral
tiers: `1, 10, 100, ..., 1_000_000_000`. Larger values require more balance
and can cause the pipeline to pause when funds are insufficient.

### --pipelines
Number of parallel pipelines. Higher values increase throughput and staggering,
but also increase concurrent in-flight commitments.

### --workers
Tokio runtime worker threads. Use 0 for auto (based on CPU count). Higher values
can improve concurrency on busy systems.

### --tick-poll-ms
Polling interval for fetching tick info from the selected backend. Smaller values
reduce latency but increase load on the endpoint.

### --backend
Selects which transport stack the client uses:
- `rpc` for classic SCAPI RPC
- `bob` for Bob JSON-RPC
- `grpc` for QubicLightNode gRPC

### --endpoint
Overrides the endpoint for the selected backend.
For `rpc`, pass only the base endpoint (`ip:port` or `scheme://host:port`), without `/live/v1` or `/query/v1`.
If omitted, the client uses the default endpoint for the selected backend.

### --balance-ms
How often the balance is printed/logged. Smaller values produce more frequent
logging.

### --empty-check-ms
How often (in ms) the client checks whether reveal was sent into an empty tick.
Smaller values can reduce reaction latency, but increase load on the selected backend.

### --reveal-verify-after
Minimum number of ticks between current tick and reveal target tick before the
client starts active reveal checks.

### --stop-before-epoch-end-secs
How many seconds before expected epoch boundary the pipeline is paused to avoid
sending too close to epoch switch.

### --resume-after-epoch-start-ticks
How many ticks the client waits after epoch switch before resuming pipeline work.

## Important details
- Each transaction contains the reveal for the previous commit plus a new commit.
- Backend selection is explicit via `--backend`.
- `--endpoint` applies to whichever backend is selected.
- In stop window and on shutdown, reveal-only is sent with `commit=0` and the
  same collateral amount as the pending commit.
- The seed is kept in locked memory and zeroized on shutdown.
- On shutdown, a pending reveal is sent synchronously.

