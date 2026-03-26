# Random Client

`Random Client` is a program for the Qubic Random smart contract.
It works automatically in a loop:

1. it sends a hidden value,
2. waits a few ticks,
3. reveals that value,
4. prepares the next round.

In simple words, this tool keeps your participation in the Random contract
running without manual action every few ticks.

## What This Program Is For

This README is written for normal users: people who want to build and run the
program, even if they are not deeply technical.

If you only need the short version:

1. build the program,
2. prepare your seed,
3. run it,
4. leave it working.

## What The Program Does

While it is running, the program:

- watches Qubic ticks,
- sends transactions at the right moment,
- checks your balance,
- pauses itself if balance is too low,
- avoids sending too close to epoch end,
- tries to send the last pending reveal before shutdown.

You do not need to manage each commit and reveal manually.

## What You Need Before Starting

- Rust installed on your computer.
- A valid seed.
  The seed must be exactly `55` lowercase English letters: `a-z`.
- Access to one backend.
  Default choices are:
  - `rpc`: `https://rpc.qubic.org`
  - `bob`: `http://localhost:40420/qubic`
  - `grpc`: `http://127.0.0.1:50051`
- Enough Qubic balance for the collateral amount you want to use.

## Build

Run this command in the project folder:

```bash
cargo build --release
```

After the build finishes, the executable file will be here:

- `target/release/RandomCient` on Linux/macOS
- `target/release/RandomCient.exe` on Windows

## First Start

The easiest launch command is:

```bash
cargo run --release -- --seed <your-55-char-seed>
```

If you do not want to put the seed in the command line, run:

```bash
cargo run --release
```

The program will ask you to enter the seed.

## Simple Launch Examples

Use the default public RPC:

```bash
cargo run --release -- --seed <your-seed>
```

Use Bob:

```bash
cargo run --release -- \
  --seed <your-seed> \
  --backend bob \
  --endpoint http://localhost:40420/qubic
```

Use gRPC / QubicLightNode:

```bash
cargo run --release -- \
  --seed <your-seed> \
  --backend grpc \
  --endpoint http://127.0.0.1:50051
```

Run the already built executable directly:

```bash
target/release/RandomCient --seed <your-seed>
```

On Windows:

```powershell
target\release\RandomCient.exe --seed <your-seed>
```

## What You Will See In The Logs

The program prints simple status messages.
Usually they tell you:

- which backend is being used,
- current epoch and tick,
- current balance,
- whether a commit was prepared,
- whether a reveal was sent,
- whether a transaction was accepted,
- whether the program is waiting because of timing or low balance.

There is also a reveal summary with:

- successful,
- failed,
- empty,
- percentages.

## The Most Important Options

Most users only need these settings:

- `--seed <SEED>`
  Your private seed. Keep it secret.
- `--backend <rpc|bob|grpc>`
  Which connection type to use.
- `--endpoint <URL>`
  Custom server address for the selected backend.
- `--collateral <AMOUNT>`
  The amount attached to each send.
- `--pipelines <N>`
  How many parallel work chains to run.
- `--senders <N>`
  How many transactions may be sent at the same time.

## What The Options Mean In Simple Words

- `--collateral`
  This is the amount used in commit and reveal transactions.
  Allowed values are only:
  `1, 10, 100, 1000, 10000, 100000, 1000000, 10000000, 100000000, 1000000000`.
- `--pipelines`
  More pipelines means more parallel commit/reveal chains.
- `--senders`
  More senders means more transactions can be sent at the same time.
  `0` means automatic choice.
- `--reveal-after`
  How many ticks to wait before reveal.
  Must be a positive multiple of `3`.
- `--reveal-guard`
  Lets the program send a little earlier so it does not miss the target tick.
- `--tick-poll-ms`
  How often the program checks for a new tick.
- `--balance-ms`
  How often balance is refreshed.
- `--empty-check-ms`
  How often old reveal ticks are checked later.
- `--reveal-verify-after`
  How many ticks to wait before checking whether reveal data appeared.
- `--stop-before-epoch-end-secs`
  How early the program should stop creating new work before epoch end.
- `--resume-after-epoch-start-ticks`
  How long to wait after a new epoch starts before working again.
- `--workers`
  Number of runtime worker threads.
  `0` means automatic choice.

## Recommended Starting Point

If you are not sure what to choose, start with defaults and only pass:

- `--seed`
- optionally `--backend`
- optionally `--endpoint`

For example:

```bash
cargo run --release -- --seed <your-seed> --backend rpc
```

## When It Is Normal For The Program To Wait

Sometimes the program waits on purpose. This is normal.

Typical reasons:

- the balance is lower than the selected collateral amount,
- it is too early to send reveal,
- the epoch is close to ending,
- the program is waiting for the safe start period of a new epoch.

## Common Problems

- `seed from stdin is empty`
  No seed was provided.
- `seed must be 55 characters`
  The seed length is wrong.
- `seed must contain only a-z characters`
  The seed contains invalid characters.
- `--reveal-after must be a positive multiple of 3`
  The reveal delay value is not allowed.
- `--collateral must be one of ...`
  The collateral value is invalid.
- `VirtualLock failed` or `mlock failed`
  The operating system refused to lock seed memory.

## Useful Commands

Show help:

```bash
cargo run --release -- --help
```

Use automatic sender count:

```bash
cargo run --release -- --seed <your-seed> --senders 0
```

Use a larger collateral amount:

```bash
cargo run --release -- --seed <your-seed> --collateral 100000
```

Use more parallel pipelines:

```bash
cargo run --release -- --seed <your-seed> --pipelines 5
```

## Full List Of Main Options

```text
--seed <SEED>
--backend <BACKEND>
--endpoint <URL>
--collateral <AMOUNT>
--senders <N>
--pipelines <N>
--reveal-after <TICKS>
--reveal-guard <TICKS>
--tick-poll-ms <MS>
--balance-ms <MS>
--empty-check-ms <MS>
--reveal-verify-after <TICKS>
--stop-before-epoch-end-secs <SECS>
--resume-after-epoch-start-ticks <TICKS>
--workers <N>
```

## Technical Notes

- The seed is stored in locked memory and cleared on drop.
- For `rpc`, pass only the base URL.
  The program adds the SCAPI path parts itself.
- If you do not pass an option, the program uses built-in defaults.
- Developer-facing contract details are in `docs/Random.h`.
