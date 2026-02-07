# Random Client

Client for the Random smart contract (SC) on the Qubic network. Implements the
commit -> reveal cycle for the `RANDOM::RevealAndCommit()` procedure and sends
transactions via RPC.

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
- Access to an RPC endpoint (default: `https://rpc.qubic.org/live/v1/`).
- Optional Bob endpoint for JSON-RPC (default: `http://localhost:40420/qubic`).
- Optional QubicLightNode gRPC endpoint (default: `http://127.0.0.1:50051`).

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
--seed <seed>                      Seed (55 chars, a-z)
--max-inflight-sends <n>           Number of concurrent senders (default: 3; 0 = auto)
--reveal-delay-ticks <n>           Reveal delay in ticks (default: 3)
--reveal-window-ticks <n>          Guard ticks before reveal send (default: 6)
--commit-amount <n>                Commit amount (default: 10000)
--pipeline-count <n>               Pipeline size (default: 3)
--worker-threads <n>               Runtime threads (default: 0 = auto)
--tick-poll <ms>                   Tick polling interval (default: 1000)
--balance-interval-ms <ms>         Balance print interval (default: 600)
--empty-tick-check-interval-ms <ms> Reveal empty-tick check interval (default: 600)
--reveal-check-delay-ticks <n>     Minimum tick distance before reveal checks (default: 10)
--epoch-stop-lead-time-secs <sec>  Pause before epoch end (default: 600)
--epoch-resume-delay-ticks <n>     Resume delay after new epoch starts (default: 50)
--rpc [url]                        Use RPC backend (optional URL after flag)
--bob [url]                        Use Bob JSON-RPC backend (optional URL after flag)
--grpc [url]                       Use QubicLightNode gRPC backend (optional URL after flag)
```

## Parameter details
### --seed
Required secret used to derive commits. Must be exactly 55 lowercase letters.
Changing the seed changes all generated commits and reveals. Keep it private.

### --max-inflight-sends
Maximum number of concurrent RPC sends. Higher values increase throughput but
also increase pressure on the endpoint. Set to 1 for strictly sequential
sending. If set to 0, the value is replaced with available CPU parallelism.

### --reveal-delay-ticks
Base delay (in ticks) between a commit and its reveal. Larger values spread out
reveal traffic and reduce overlap but increase the time until a reveal is sent.
Smaller values make the cycle faster but can be less tolerant to network delays.

### --reveal-window-ticks
Guard window (in ticks) before the scheduled reveal tick. The pipeline waits
until `now_tick >= reveal_send_at_tick - guard` before sending a reveal+commit.
Larger values send reveals earlier; smaller values wait closer to the reveal
tick.

### --commit-amount
Amount sent with each commit/reveal transaction. If zero, balance checks are
skipped and the pipeline still emits jobs. Larger values require more balance
and can cause the pipeline to pause when funds are insufficient.

### --pipeline-count
Number of parallel pipelines. Higher values increase throughput and staggering,
but also increase concurrent in-flight commitments.

### --worker-threads
Tokio runtime worker threads. Use 0 for auto (based on CPU count). Higher values
can improve concurrency on busy systems.

### --tick-poll
Polling interval for fetching tick info from the RPC endpoint. Smaller values
reduce latency but increase load on the endpoint.

### --rpc
Use RPC endpoint for transaction broadcast and RPC-based reads.
If URL is provided right after the flag, that URL is used.
If URL is omitted, default is `https://rpc.qubic.org/live/v1/`.

### --bob
Use Bob JSON-RPC for tick, balance, empty-tick checks, and transaction broadcast.
If URL is provided right after the flag, that URL is used.
If URL is omitted, default is `http://localhost:40420/qubic`.

### --grpc
Use QubicLightNode gRPC for tick, balance, and empty-tick checks.
If URL is provided right after the flag, that URL is used.
If URL is omitted, default is `http://127.0.0.1:50051`.

### --balance-interval-ms
How often the balance is printed/logged. Smaller values produce more frequent
logging.

### --empty-tick-check-interval-ms
How often (in ms) the client checks whether reveal was sent into an empty tick.
Smaller values can reduce reaction latency, but increase load on the selected backend.

### --reveal-check-delay-ticks
Minimum number of ticks between current tick and reveal target tick before the
client starts active reveal checks.

### --epoch-stop-lead-time-secs
How many seconds before expected epoch boundary the pipeline is paused to avoid
sending too close to epoch switch.

### --epoch-resume-delay-ticks
How many ticks the client waits after epoch switch before resuming pipeline work.

## Important details
- Each transaction contains the reveal for the previous commit plus a new commit.
- `--bob` and `--grpc` are mutually exclusive; using both returns an error.
- Backend selection priority: `--bob` -> `--grpc` -> default `RPC`.
- The seed is kept in locked memory and zeroized on shutdown.
- On shutdown, a pending reveal is sent synchronously.



