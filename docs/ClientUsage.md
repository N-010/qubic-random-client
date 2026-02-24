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
--max-inflight-sends <N>                  Number of transaction senders (default 3; 0 = auto)
--reveal-delay-ticks <N>       Reveal delay relative to commit (default 3)
--reveal-window-ticks <N>  Guard ticks before reveal send (default 5)
--commit-amount <N>            Deposit/stake amount (default 10000)
--pipeline-count <N> Number of parallel commit/reveal pipelines (default 3)
--worker-threads <N>          Tokio worker threads (default 0 = auto)
--tick-poll <N>    Tick poll interval (default 300)
--empty-tick-check-interval-ms <N> Interval for checking whether reveal was sent into an empty tick (ms)
--reveal-check-delay-ticks <N> Minimum tick delay before checking reveal tick data (default 10)
--epoch-stop-lead-time-secs <N> Seconds before Wednesday 12:00 UTC when reveal/commit sending is stopped (default 600)
--epoch-resume-delay-ticks <N> Minimum ticks from epoch initial tick before reveal/commit sending resumes (default 50)
--rpc [URL]                    Use RPC endpoint (default https://rpc.qubic.org/live/v1/)
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
- Sending is blocked during epoch stop window (Wednesday before `12:00 UTC`, controlled by `--epoch-stop-lead-time-secs`).
- On entering stop window, if a reveal+commit pair is pending, the client sends only reveal with empty commit (`committed_digest = 0`, amount `0`), then blocks new reveal/commit sends.
- Resume condition is `current_tick - initial_tick >= --epoch-resume-delay-ticks` and not being in the stop window.
- After resume condition is met, pipeline continues with normal start (`empty reveal + commit`), then normal reveal+commit cycle.
- `empty tick` checks and balance polling continue regardless of reveal/commit send pause.

## RPC usage

- Transactions are sent via the RPC endpoint specified in `--rpc`.
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

Run with a custom RPC endpoint and deposit:

```bash
cargo run -- \
  --seed <seed> \
  --rpc https://rpc.qubic.org/live/v1/ \
  --commit-amount 25000
```

Run with auto senders:

```bash
cargo run -- --max-inflight-sends 0
```

