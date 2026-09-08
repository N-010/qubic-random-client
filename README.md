# Random Client

Random Client is a Rust provider client for the Qubic Random smart contract.
One process maintains a provider chain for the configured collateral tier in
each of the three Random streams and continuously submits `RevealAndCommit`
transactions.

The client supports three network backends:

- `rpc` — Qubic HTTP RPC;
- `bob` — Bob JSON-RPC through SCAPI;
- `grpc` — QubicLightNode gRPC.

## Release status

RandomClient v2.0.2 can be built from the tagged checkout. Successful tag
workflows also publish Linux and Windows binaries to GitHub Releases.

For the `grpc` backend, use QubicLightNode v2.0.2 with the matching
`proto/lightnode.proto` schema.

The pinned SCAPI dependency is built without its optional WalletConnect
integration. Qubic signing, RPC, and Bob remain available; the unused
WalletConnect/Ethereum dependencies are excluded from this client.

## Requirements

The repository pins Rust 1.93.0, including `rustfmt` and `clippy`, in
`rust-toolchain.toml`. A compatible Rust installation automatically selects
that toolchain when commands are run from the repository.

## Build and run

Build and verify the client:

```bash
cargo build --release --locked
cargo test --all-targets --all-features --locked
```

Run it and enter the seed at the hidden prompt:

```bash
cargo run --release --locked
```

For non-interactive use, the seed can be supplied as an argument or as the
first redirected input line:

```bash
cargo run --release --locked -- --seed YOUR_55_LETTER_SEED
```

Avoid placing the seed in shell history or process arguments when an
interactive prompt or protected input stream is available.

## Configuration

Use `cargo run --release --locked -- --help` to see the current CLI. The main
options are:

| Option | Default | Purpose |
| --- | --- | --- |
| `--backend <rpc\|bob\|grpc>` | `rpc` | Select the network backend. |
| `--endpoint <URL>` | Backend-specific | Override the selected backend endpoint. |
| `--collateral <AMOUNT>` | `10000` | Select the Random collateral tier. |
| `--seed <SEED>` | Hidden input | Supply the 55-letter Qubic seed. |
| `--empty-check-ms <MILLISECONDS>` | `600` | Set the retry interval for delayed empty-tick checks. |
| `--reveal-verify-after <TICKS>` | `10` | Delay verification of a financial target. |
| `--stop-before-epoch-end-secs <SECONDS>` | `600` | Begin proactive drain before the epoch boundary. |
| `--resume-after-epoch-start-ticks <TICKS>` | `50` | Delay enrollment after a new epoch begins. |

Default endpoints are:

- RPC: `https://rpc.qubic.org`
- Bob: `http://localhost:40420`
- gRPC: `http://127.0.0.1:50051`

Collateral must be a power of ten from `1` through `1000000000`. The client
does not perform a balance precheck; an underfunded enrollment is detected
through later provider status observations like any other rejected enrollment.
Both empty-tick timing options must be greater than zero.

## Operating model

- The process manages one chain per Random stream for the selected identity and
  collateral tier. Run exactly one writer for that identity/tier combination.
- Each stream uses its assigned three-tick sequence. Transactions become
  eligible for broadcast six ticks before their immutable target tick.
- A temporary delivery failure retries identical signed bytes until the target
  tick. A backend acceptance is not proof of contract execution.
- A chain restarts only after a fresh `GetProviderStatus` observation reports
  its exact `(stream, tier)` absent. Confirmed foreign or stale status freezes
  planning and discards untrusted preimages after the signed tail is handled.
- Before the Wednesday 12:00 UTC epoch boundary, the client drains planned work
  and attempts a terminal `reveal + zero commit`. It waits through the new-epoch
  warmup before enrolling again.
- On supported shutdown signals, the client freezes planning, handles its
  already signed tail, attempts the terminal reveal when safe, and reports an
  error if the bounded shutdown cannot complete.

Logs report cumulative financial outcomes as `Sends: ok / failed / empty`.
These counters distinguish backend acceptance, failed financial targets, and
later observations that the whole target tick was empty; they are not a proof
that a particular transaction executed.

Unavailable tick checks retry after 1, 2, 4, 8, 16, then 30 seconds without
blocking other eligible checks. They remain pending until success or an epoch
change/process exit. Backend errors preserve the counters, so `empty = 0` does
not establish that all ticks were checked successfully.

The complete scheduling, recovery, epoch, and shutdown state machines are
specified in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). A non-normative
Russian translation is available in
[`docs/ARCHITECTURE.ru.md`](docs/ARCHITECTURE.ru.md).

## Backend trust and protocol

RPC and Bob obtain provider status through their contract-query endpoints.
QubicLightNode returns one structurally valid peer response for contract
queries after checking that peer's current epoch/tick against the query's
cached reference (same epoch, at most one tick behind). If no eligible peer
answers, the query fails temporarily. QubicLightNode refreshes authenticated
computor keys across epochs and derives empty-tick status from validated Core
tick data. Its tick
and contract-query observations are peer-trusted data, not a Qubic consensus
proof. QubicLightNode still verifies transaction signatures locally before
forwarding transactions.

Broadcasting six ticks early reveals preimage material to the selected backend
before the target tick. Choose and operate that backend according to this
availability and confidentiality tradeoff. Seeds, preimages, and signed bytes
are not logged or persisted by the client.

[`docs/Random.h`](docs/Random.h) is the smart-contract source of truth. Random
Client uses contract index `3`, `RevealAndCommit` procedure `1`, and
`GetProviderStatus` function `2`.

## Development and security

Run the same primary quality checks used by CI:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

CI also enforces the dependency policies in [`deny.toml`](deny.toml) with
`cargo-audit` and `cargo-deny`. See [`SECURITY.md`](SECURITY.md) for private
vulnerability reporting, [`CHANGELOG.md`](CHANGELOG.md) for release changes,
and [`LICENSE`](LICENSE) for the MIT license.
