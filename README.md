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
--commit-amount <n>                Commit amount (default: 10000)
--commit-reveal-sleep-ms <ms>      Sleep between ticks in pipeline (default: 200)
--commit-reveal-pipeline-count <n> Pipeline size (default: 3)
--runtime-threads <n>              Runtime threads (0 = auto)
--tick-poll-interval-ms <ms>       Tick polling interval (default: 50)
--contract-id <id>                 Contract ID (default: Random SC)
--endpoint <url>                   RPC endpoint
--balance-interval-ms <ms>         Balance print interval (default: 300)
```

## Important details
- Each transaction contains the reveal for the previous commit plus a new commit.
- The seed is kept in locked memory and zeroized on shutdown.
- On shutdown, a pending reveal is sent synchronously.
