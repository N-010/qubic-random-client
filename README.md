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
--senders <n>                      Number of senders (default: 3)
--reveal-delay-ticks <n>           Reveal delay in ticks (default: 3)
--reveal-send-guard-ticks <n>      Guard ticks before reveal send (default: 5)
--commit-amount <n>                Commit amount (default: 10000)
--commit-reveal-pipeline-count <n> Pipeline size (default: 3)
--runtime-threads <n>              Runtime threads (0 = auto)
--heap-dump                        Trigger a jemalloc heap profile dump at startup
--heap-stats                       Print allocator stats on shutdown (Ctrl+C)
--heap-dump-interval-secs <n>      Periodic heap dump interval in seconds (0 = disabled)
--tick-poll-interval-ms <ms>       Tick polling interval (default: 300)
--endpoint <url>                   RPC endpoint
--balance-interval-ms <ms>         Balance print interval (default: 300)
```

## Parameter details
### --seed
Required secret used to derive commits. Must be exactly 55 lowercase letters.
Changing the seed changes all generated commits and reveals. Keep it private.

### --senders
Maximum number of concurrent RPC sends. Higher values increase throughput but
also increase pressure on the RPC endpoint. Set to 1 for strictly sequential
sending.

### --reveal-delay-ticks
Base delay (in ticks) between a commit and its reveal. Larger values spread out
reveal traffic and reduce overlap but increase the time until a reveal is sent.
Smaller values make the cycle faster but can be less tolerant to network delays.

### --reveal-send-guard-ticks
Guard window (in ticks) before the scheduled reveal tick. The pipeline waits
until `now_tick >= reveal_send_at_tick - guard` before sending a reveal+commit.
Larger values send reveals earlier; smaller values wait closer to the reveal
tick.

### --commit-amount
Amount sent with each commit/reveal transaction. If zero, balance checks are
skipped and the pipeline still emits jobs. Larger values require more balance
and can cause the pipeline to pause when funds are insufficient.

### --commit-reveal-pipeline-count
Number of parallel pipelines. Higher values increase throughput and staggering,
but also increase concurrent in-flight commitments.

### --runtime-threads
Tokio runtime worker threads. Use 0 for auto (based on CPU count). Higher values
can improve concurrency on busy systems.

### --heap-dump
Triggers a jemalloc heap profile dump at startup (jemalloc builds only).

### --heap-stats
Prints allocator stats on shutdown (Ctrl+C). Works with jemalloc and mimalloc.

### --heap-dump-interval-secs
Periodic heap dump interval in seconds (jemalloc builds only). Set to 0 to
disable periodic dumps.

### --tick-poll-interval-ms
Polling interval for fetching tick info from the RPC endpoint. Smaller values
reduce latency but increase load on the endpoint.

### --endpoint
RPC endpoint URL.

### --balance-interval-ms
How often the balance is printed/logged. Smaller values produce more frequent
logging.

## Important details
- Each transaction contains the reveal for the previous commit plus a new commit.
- The seed is kept in locked memory and zeroized on shutdown.
- On shutdown, a pending reveal is sent synchronously.

## Heap profiling (jemalloc)
- Build with jemalloc enabled: `cargo build --features jemalloc`.
- Enable profiling before start: `JEMALLOC_CONF=prof:true,prof_active:true,lg_prof_interval:30`.
- Trigger a dump at startup with `--heap-dump`, or periodically with `--heap-dump-interval-secs`.
- Print allocator stats on shutdown with `--heap-stats`.
- Use `JEMALLOC_CONF=prof_prefix:/path/prefix` to control where dumps are written.
- Jemalloc profiling is not supported on MSVC targets.

## Windows allocator stats (mimalloc)
- Build with mimalloc enabled: `cargo build --features mimalloc`.
- Use `--heap-stats` to print allocator stats on shutdown (e.g., Ctrl+C).
- `MIMALLOC_SHOW_STATS=1` also prints stats on shutdown if set.
- `--heap-dump` flags require jemalloc and are not available on MSVC.
