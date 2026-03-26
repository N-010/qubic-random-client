# Random Client

Client for the Qubic Random smart contract. It runs the `commit -> reveal`
cycle for `RANDOM::RevealAndCommit()` and sends transactions through the
selected backend.

## What It Does

- Generates 4096 random bits.
- Sends a commit first: only the digest of those bits.
- After a delay, sends the reveal for the previous commit and a new commit for
  the next round.
- Monitors balance and pauses when there is not enough collateral.
- Supports multiple parallel pipelines and multiple backends.

Related technical docs:

- `docs/Random.h` for the SC input structure.

## How It Works

1. The client generates random bits from the OS.
2. It computes a digest of those bits.
3. It sends a commit-only transaction with an empty reveal and that digest.
4. It waits a few ticks. Default is `3`.
5. It sends the previous reveal plus a new digest for the next cycle.
6. The cycle repeats continuously.

This means every reveal proves the data promised by the previous commit.

The client also does the following while running:

- Watches ticks to know when to send.
- Watches balance to avoid sending without enough collateral.
- Verifies reveal ticks later to detect empty-tick cases.
- On shutdown, tries to send the pending reveal before exit.

## Requirements

- Rust edition 2024.
- A valid seed: exactly 55 lowercase `a-z` characters.
- Access to one backend endpoint:
  - `rpc`: `https://rpc.qubic.org`
  - `bob`: `http://localhost:40420/qubic`
  - `grpc`: `http://127.0.0.1:50051`
- Enough balance for the selected collateral tier.

## Build

```bash
cargo build --release
```

## Quick Start

Run with the seed on the command line:

```bash
cargo run --release -- --seed <55-char-seed>
```

Or run without `--seed` and enter it interactively:

```bash
cargo run --release
```

Example with explicit backend, endpoint, and collateral:

```bash
cargo run --release -- \
  --seed <seed> \
  --backend rpc \
  --endpoint https://rpc.qubic.org \
  --collateral 100000
```

Example with Bob:

```bash
cargo run --release -- \
  --seed <seed> \
  --backend bob \
  --endpoint http://localhost:40420/qubic
```

Example with automatic sender count:

```bash
cargo run --release -- --seed <seed> --senders 0
```

## Main Options

```text
--seed <SEED>                             Seed. If omitted, reads from stdin/TTY
--backend <BACKEND>                       Backend: rpc, bob, grpc
--endpoint <URL>                          Endpoint for the selected backend
--collateral <AMOUNT>                     Collateral tier for commit/reveal sends
--senders <N>                             Number of concurrent senders
--pipelines <N>                           Number of parallel commit/reveal pipelines
--reveal-after <TICKS>                    Delay between commit and reveal
--reveal-guard <TICKS>                    Early-send guard window before reveal tick
--tick-poll-ms <MS>                       Tick polling interval
--balance-ms <MS>                         Balance print interval
--empty-check-ms <MS>                     Empty-tick check interval
--reveal-verify-after <TICKS>             Delay before reveal tick verification
--stop-before-epoch-end-secs <SECS>       Stop sending before epoch end
--resume-after-epoch-start-ticks <TICKS>  Resume sending after epoch start
--workers <N>                             Tokio worker threads
```

## Option Meaning

- `--seed`: secret used to derive identity and signatures. Keep it private.
- `--backend`: chooses transport stack:
  - `rpc` uses SCAPI RPC.
  - `bob` uses Bob JSON-RPC.
  - `grpc` uses QubicLightNode gRPC.
- `--endpoint`: overrides the default endpoint for the selected backend.
- `--collateral`: amount sent with commit and reveal+commit transactions. Must
  be one of the contract tiers:
  `1, 10, 100, 1000, 10000, 100000, 1000000, 10000000, 100000000, 1000000000`.
- `--senders`: max number of concurrent transaction sends. `0` means auto.
- `--pipelines`: number of parallel commit/reveal chains.
- `--reveal-after`: must be a positive multiple of `3`.
- `--reveal-guard`: allows sending a reveal slightly before its exact target
  tick to tolerate slower polling.
- `--tick-poll-ms`: how often the client polls for new ticks.
- `--balance-ms`: how often current balance is printed.
- `--empty-check-ms`: how often reveal ticks are checked for empty-tick cases.
- `--reveal-verify-after`: minimum tick delay before checking reveal tick data.
- `--stop-before-epoch-end-secs`: prevents sending too close to epoch end.
- `--resume-after-epoch-start-ticks`: warmup period after new epoch start.
- `--workers`: Tokio worker threads. `0` means auto.

## Runtime Behavior

- Normal cycle:
  - commit-only first
  - then reveal + new commit
- If balance is below the selected collateral amount, the pipeline pauses.
- In the stop window, the client stops creating new commit/reveal work.
- On stop-window entry or shutdown, pending work is sent as reveal-only with
  `commit = 0`.
- If reveal broadcast fails due to external transport/backend errors, that
  reveal is not retried by design.

## Common Errors

- `seed from stdin is empty`: no seed was provided.
- `seed must be 55 characters`: wrong seed length.
- `seed must contain only a-z characters`: invalid seed characters.
- `--reveal-after must be a positive multiple of 3`: invalid reveal delay.
- `--collateral must be one of ...`: invalid collateral tier.
- `VirtualLock` / `mlock` errors: the OS failed to lock seed memory.

## Notes

- The seed is stored in locked memory and zeroized on drop.
- For `rpc`, pass the base URL only; the client appends the SCAPI path parts it
  needs.
- The client binary name comes from `Cargo.toml`; when running through Cargo,
  use `cargo run --release -- ...`.
