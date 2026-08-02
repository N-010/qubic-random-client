# Random Client

`random-client` is a Rust provider client for the Qubic Random smart contract.
It maintains one provider slot in each Random stream and runs the
`commit -> reveal + commit` cycle continuously.

The client supports three independent backends:

- `rpc` — Qubic HTTP RPC;
- `bob` — Bob JSON-RPC;
- `grpc` — the local `QubicLightNode` gRPC API.

Every backend supplies tick, balance, transaction broadcast, and generic
smart-contract function queries. The client uses Random's
`GetProviderStatus` function to verify contract execution; accepting a
broadcast is not treated as proof that the contract accepted the call.

## Architecture

- `config` owns CLI parsing, validation, and protected seed storage.
- `backend` exposes one transport-neutral interface implemented by RPC, Bob,
  and QubicLightNode.
- `contract` owns the exact Random wire layout and its validation.
- `engine` owns three independent predictive commit/reveal schedulers.
- `app` only constructs these components and handles process signals.

SCAPI remains pinned to the existing revision. It is used for wallet/identity
handling, transaction construction and signing, and the existing RPC and Bob
clients. Random-specific encoding, status decoding, and scheduling are kept
outside SCAPI.

## Build

Rust stable with edition 2024 support is required.

```bash
cargo build --release --locked
cargo test --all-targets
```

The executable is `target/release/random-client`.

## Run

```bash
cargo run --release -- --seed <55-letter-seed>
```

If `--seed` is omitted, it is read from the terminal without echo or from the
first line of redirected standard input.

Main options:

```text
--backend <rpc|bob|grpc>
--endpoint <URL>
--collateral <AMOUNT>
--seed <SEED>
--stop-before-epoch-end-secs <SECONDS>
--resume-after-epoch-start-ticks <TICKS>
```

Default endpoints:

- RPC: `https://rpc.qubic.org`
- Bob: `http://localhost:40420`
- gRPC: `http://127.0.0.1:50051`

Allowed collateral values are `1`, `10`, `100`, `1000`, `10000`, `100000`,
`1000000`, `10000000`, `100000000`, and `1000000000`.

The client uses the same collateral tier in streams 0, 1, and 2. A fresh
three-stream enrollment needs three locked collateral amounts plus one liquid
collateral amount for subsequent reveal calls.

## Reconciliation, epochs, and shutdown

Commit preimages exist only in process memory. After restart, any provider
slot already reported by the contract is treated as unmanaged because its
preimage is unavailable. The client waits for that exact `(stream, tier)` slot
to disappear before opening a new chain.

The scheduler does not wait for `GetProviderStatus` before building the next
call. It keeps at most four unfinished future calls through nine ticks ahead;
calls become eligible for broadcast six ticks before their target. If polling
skips that exact boundary, the call is still attempted before its target. Each
queued call owns its exact signed transaction bytes and its own broadcast task,
so a slow stream or backend request cannot block the other streams. Ambiguous
transport failures are retried with identical bytes until the original target
tick.

`GetProviderStatus` is only a consistency check. A `lastUpdateTick` matching
any target signed by the current local generation confirms that target and
every earlier call in the generation. Missing, older, delayed, or failed status
requests do not by themselves pause predictive broadcasts. An empty or lagging
successful status remains inconclusive through the next complete stream cycle
(`T + 3`). A response requested at or after that deadline which still contains
none of the expected targets confirms a predictive-chain break. A newer target
which was never signed locally makes the slot unmanaged because its current
preimage is unknown.

After a confirmed or locally detected break, the client stops sending that
slot's queued work and reconciles with a status request started after the
break. If status reports a locally accepted target, the client uses the
preimage committed at that exact target and resumes with a future normal
`reveal + commit`. If status reports the slot absent, the old work is discarded
and the client may enroll again using `zero reveal + new nonzero commit`. If
the slot remains occupied but its outstanding preimage is not known, the slot
becomes unmanaged and waits for contract eviction.

The client never sends `reveal + zero commit` as post-failure recovery. That
terminal form is safe only while the local outstanding preimage is still known,
so it is reserved for proactive epoch drain and graceful process shutdown.

The client starts epoch drain 600 seconds before Wednesday 12:00 UTC by default
and pauses enrollment for the first 50 ticks after an observed epoch change.
QubicLightNode does not report the epoch's initial tick, so with that backend
the first verified tick observed after startup or an epoch change starts the
same conservative 50-tick pause. The two CLI options above override those
windows. Old-epoch signed work is discarded when the epoch number changes, and
status ownership is established again from `Unknown`.

Balance queries are only used when opening vacant slots. A balance failure
does not delay pending broadcasts or reveals for already managed slots. The
old tick-data heuristic has been removed because unrelated transactions could
produce false confirmations.

On Windows, Ctrl+C, Ctrl+Break, console close, logoff, and system shutdown
start graceful shutdown. On Unix, SIGINT, SIGTERM, SIGQUIT, and SIGHUP do the
same. No new slots are opened. Each safely managed scheduler freezes its current
predictive tail and submits a terminal `reveal + zero commit` three ticks later;
it does not create a replacement first commit. Shutdown succeeds only after all
prerequisite calls and the terminal call have backend acceptance. An expired
required call or the 90-second deadline returns an error. A chain already in
reconciliation is not speculatively drained.

Run only one writer for a given identity and collateral tier. A competing
client cannot be detected before it changes contract state. Also note that the
six-tick availability lead exposes reveal material to the selected backend
before the target tick; use a backend you trust with that early reveal.

## QubicLightNode

The companion source is expected at:

```text
D:\Work\MySelf\Qubic\QubicLightNode
```

The client protocol matches QubicLightNode 0.2.0 at commit
`99d17ddc008e05d094a16a09c93f7f779d012116`. Tick status is unavailable until
the node has authenticated its computor list and collected a FourQ-verified
quorum of 451 matching votes. The quorum format does not provide
`initial_tick` or `tick_duration_ms`; the client conservatively uses the first
verified tick seen in each epoch and a 1,000 ms duration. Its generic
`QueryContractFunction` method continues to accept a contract index, function
input type, and raw input bytes and returns raw output bytes.

`compose.yaml` uses `QubicLightNode` itself as the light-node build context.
Its `Dockerfile` and `.dockerignore` live in that repository, so unrelated
sibling projects and local build artifacts are not sent to the Docker daemon:

```bash
docker compose up --build
```

The compose client uses `grpc` and connects to `http://light-node:50051`.

## Protocol

[`docs/Random.h`](docs/Random.h) is the source of truth for the smart-contract
interface. The client currently uses:

- contract index `3`;
- `RevealAndCommit` procedure `1`;
- `GetProviderStatus` function `2`.

`GetProviderStatus` input is the provider's raw 32-byte public key. Its output
is the 680-byte QPI structure described by the header, including alignment
padding before `lockedCollateral`.
