# Random Client

Random Client is a Rust provider client for the Qubic Random smart contract.
One process maintains a provider chain for the configured collateral tier in
each of the three Random streams and continuously submits `RevealAndCommit`
transactions.

The client supports three network backends:

- `rpc` — Qubic HTTP RPC;
- `bob` — Bob JSON-RPC;
- `grpc` — QubicLightNode gRPC.

## Release status

The current source version is RandomClient v2.1.0. Successful tag
workflows publish Linux and Windows binaries together with `SHA256SUMS` to
GitHub Releases. A release tag must equal `v` followed by the package version
in `Cargo.toml`. Both platform builds must pass `--version` and `--help` checks;
one publication job verifies both artifacts and their checksums before upload.

For the `grpc` backend, use QubicLightNode v2.0.2 with the matching
`proto/lightnode.proto` schema.

The pinned SCAPI dependency is built without its optional WalletConnect
integration. Qubic signing, RPC, and Bob remain available; the unused
WalletConnect/Ethereum dependencies are excluded from this client.

RPC and Bob HTTP responses are limited to 16 MiB before JSON parsing, including
chunked responses. Larger responses fail the attempt and follow the normal retry
policy. HTTP error bodies are discarded without reading them. The bounded
transport preserves SCAPI's request formats and RPC models; signing stays in SCAPI.

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
| `--empty-check-ms <MILLISECONDS>` | `600` | Set the minimum interval between starts of delayed tick checks. |
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
- The engine polls ticks every 20 ms for gRPC and 100 ms for RPC/Bob, and
  dispatches transactions before new
  background queries. Provider-status queries remain limited to two per second.
- Each stream uses its assigned three-tick sequence and plans six ticks ahead.
  First commits become eligible for broadcast six ticks before their immutable
  target. Normal and terminal reveals wait until four ticks before target,
  including retries and retained old tails.
- A temporary delivery failure retries identical signed bytes until the target
  tick. A backend acceptance is not proof of contract execution.
- An accepted first commit gets one additional identical-byte send four ticks
  before its target (three if polling skips that boundary). An accepted normal
  reveal gets its additional send at three ticks before target, separating it
  from the first send at four ticks before target.
  No extra send starts at or after Core's two-tick publication boundary.
  Its result does not change delivery counters. No extra sends start for the
  old frozen tail or during drain, and terminal reveals are excluded.
- A chain restarts only after a fresh `GetProviderStatus` observation reports
  its exact `(stream, tier)` absent. Three consistent absence, foreign-target,
  or acknowledgement-lag observations freeze the old chain. A replacement can
  be sent while that old signed tail finishes:
  its first target is at least six ticks ahead and three ticks after the old
  tail. Old retries keep identical bytes and cannot stop the replacement or
  block its terminal reveal. The new chain retains normal broadcast timing.
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

Tick checks prioritize the newest eligible targets, with one request at a time
and a two-second timeout. Unavailable checks retry after 1, 2, 4, 8, 16, then
30 seconds, only through 32 ticks after `--reveal-verify-after` (age 42 with
defaults). Older checks are discarded and reported as unverified, preserving
the counters. No new checks start during drain or shutdown. Monitoring can
skip targets under load; `empty = 0` does not establish that all ticks were
checked successfully. This monitoring window does not change the SC schedule.

With the updated LightNode, delayed checks also log `TickData inclusion for
target T: included / missing / unknown`. The client compares the full 32-byte
K12 hash of its signed transaction against the authenticated TickData digest
list. An old LightNode, RPC, or Bob provides `unknown` for this check. These
observations do not change `Sends` counters or restart decisions and do not
prove successful contract execution or consensus finality. Upgrade both
LightNode and this client to enable inclusion checking; no new CLI flag is needed.

The complete scheduling, recovery, epoch, and shutdown state machines are
specified in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). A non-normative
Russian translation is available in
[`docs/ARCHITECTURE.ru.md`](docs/ARCHITECTURE.ru.md).

## Backend trust and protocol

The gRPC adapter logs the target and full locally computed transaction digest
for offline inclusion checks, without signed payloads or preimages.

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

Broadcast reveals preimages to both the selected backend and Core's P2P peers.
The client holds reveal-bearing transactions until observed `T - 4`, after the
preceding round's commitment set is selected at `T - 5` under Core's two-tick
publication schedule. This protection requires correct backend tick reports;
it does not authenticate consensus or eliminate selective withholding.
Bob uses `currentVerifyLoggingTick`: verification lag can delay delivery, and
switching to a local Bob or QubicLightNode alone does not ensure security or
timely inclusion. Seeds, preimages, and signed bytes are not logged or persisted.

Entropy is separate per tier. The default `--collateral 10000` supports tier 4;
use `--collateral 1` to support tier 0. A higher collateral tier does not supply
entropy to lower tiers. Contract fees are on-chain state and cannot be repaired
by changing this provider client.

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
`cargo-audit` and `cargo-deny`. Security checks also run every Monday at 06:23
UTC on the default branch. CI can be started manually; only a pushed `v*` tag
can trigger release publication. All Rust jobs use `rust-toolchain.toml`.
See [`SECURITY.md`](SECURITY.md) for private
vulnerability reporting, [`CHANGELOG.md`](CHANGELOG.md) for release changes,
and [`LICENSE`](LICENSE) for the MIT license.
