# How to work with the Random client

This document describes how to run and operate the Random SC client.

## Quick start

1) Build:

```bash
cargo build
```

2) Run (seed is required; default is stdin/TTY):

```bash
cargo run -- --seed <55-char seed of a-z>
```

Or just run and enter the seed (hidden input):

```bash
cargo run
```

## Seed requirements

- Length: exactly 55 characters.
- Allowed characters: only `a-z` (lowercase latin).
- The seed is kept in a locked memory buffer and zeroized on exit.

## Main CLI parameters

The client binary is `random-client` (see `Cargo.toml`). If `--seed` is not provided, the client reads from stdin/TTY by default.

```text
--seed <STRING>                Seed (55 chars a-z). If not provided, reads from stdin/TTY
--senders <N>                  Number of transaction senders (default 3; 0 = auto)
--reveal-delay-ticks <N>       Reveal delay relative to commit (default 3)
--commit-amount <N>            Deposit/stake amount (default 10000)
--commit-reveal-sleep-ms <N>   Sleep between commit/reveal iterations (default 200)
--commit-reveal-pipeline-count <N> Number of parallel commit/reveal pipelines (default 3)
--runtime-threads <N>          Tokio worker threads (default 0 = auto)
--tick-poll-interval-ms <N>    Tick poll interval (default 50)
--contract-id <ID>             Contract ID (default DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG)
--endpoint <URL>               RPC endpoint (default https://rpc.qubic.org/live/v1/)
--balance-interval-ms <N>      Balance print interval (default 300)
```

## Pipeline behavior

- The logic is built around `RANDOM::RevealAndCommit()`.
- First a commit is sent (publishing the digest), then after `+3` ticks a reveal + new commit.
- `revealedBits` reveal the entropy for the previous commit, `committedDigest` is the digest for the next reveal.
- Each transaction is sent with `commit_amount` (reveal-only is not used).
- If available balance is below `commit_amount`, the pipeline pauses.
- The transaction is scheduled for a future tick: `current_tick + reveal_delay_ticks`.
- To prevent non-reveals, a deposit is used: on commit, the deposit is held by the contract; if revealed on time it is returned; otherwise it is forfeited.

## RPC usage

- Transactions are sent via the RPC endpoint specified in `--endpoint`.
- Tick/balance queries use SCAPI v0.2 (see `docs/Architecture.md`).

## Shutdown behavior

- On shutdown, if there is a pending commit, the client synchronously sends a reveal before exit.
- The shutdown reveal uses amount=0 and does not create a new commit.

## Common errors

- `seed from stdin is empty`: stdin/TTY was empty.
- `seed must be 55 characters`: invalid length.
- `seed must contain only a-z characters`: invalid characters.
- `VirtualLock/mlock` errors: the system failed to lock the seed memory.

## Examples

Run with a custom endpoint and deposit:

```bash
cargo run -- \
  --seed <seed> \
  --endpoint https://rpc.qubic.org/live/v1/ \
  --commit-amount 25000
```

Run with auto senders:

```bash
cargo run -- --senders 0
```
