# Random Client

Small Rust provider client for the Qubic Random smart contract. One process
maintains one provider chain for the configured collateral tier in each of the
three Random streams and continuously sends `RevealAndCommit` transactions.

Supported backends:

- `rpc` — Qubic HTTP RPC;
- `bob` — Bob JSON-RPC through SCAPI;
- `grpc` — QubicLightNode gRPC.

## Build and run

Rust stable with edition 2024 support is required.

```bash
cargo build --release --locked
cargo test --all-targets
cargo run --release -- --seed <55-letter-seed>
```

If `--seed` is omitted, it is read without echo from an interactive terminal,
or from the first redirected input line.

```text
--backend <rpc|bob|grpc>
--endpoint <URL>
--collateral <AMOUNT>
--seed <SEED>
--empty-check-ms <MILLISECONDS>
--reveal-verify-after <TICKS>
--stop-before-epoch-end-secs <SECONDS>
--resume-after-epoch-start-ticks <TICKS>
```

Defaults:

- RPC: `https://rpc.qubic.org`
- Bob: `http://localhost:40420`
- gRPC: `http://127.0.0.1:50051`
- collateral: `10000`
- empty-tick check interval: 600 ms
- normal reveal verification delay: 10 ticks
- pre-epoch drain: 600 seconds before Wednesday 12:00 UTC
- new-epoch warmup: 50 ticks

Collateral must be a power of ten from `1` through `1000000000`. The client no
longer performs a balance precheck. An underfunded first commit is detected in
the same way as any other rejected enrollment: the exact `(stream, tier)`
remains absent from a later `GetProviderStatus` response, and the client starts
a fresh chain.

## Runtime behavior

The client continuously runs one `GetProviderStatus` request at a time. A
successfully decoded response is applied directly. It does not start a stream
while that exact `(stream, collateral tier)` is occupied, because commit
preimages are held only in process memory. One absent response requested at an
eligible tick permits a fresh first commit (`zero reveal + non-zero commit`).

Each stream uses only ticks where `tick % 3 == stream`; consecutive calls in a
chain are three ticks apart. Transactions are prepared for fixed target ticks
and become eligible for broadcast six ticks early. One successful backend
acceptance completes delivery. A failed delivery is retried with identical
signed bytes, independently from the other calls and streams. Nothing is
broadcast at or after its target tick.

Normal `reveal + commit` deliveries are summarized in every subsequent log as
`Sends: ok / failed / empty`. The counters exclude the first zero-reveal commit
and terminal drain/shutdown reveals. Retries belong to one target outcome: a
temporary broadcast error followed by backend acceptance counts only as `ok`,
while a target that expires without acceptance counts once as `failed`.

After an accepted normal reveal, the client waits 10 ticks by default and asks
the selected backend whether the target tick contains data or transactions. An
empty result reclassifies that target from `ok` to `empty`; a check error is
retried without changing the counters. This checks the tick as a whole and does
not prove that this client's transaction executed. The interval and delay are
configurable through `--empty-check-ms` and `--reveal-verify-after`; both must
be greater than zero. Counters are cumulative for the process lifetime.

The client keeps planning `reveal + commit` calls without waiting for every
`lastUpdateTick`. It treats the greatest local status tick as an acknowledgement
watermark. One response that lags behind a signed target starts a suspicion; a
second successfully decoded response requested at a later tick must leave that
same target unconfirmed before the chain is frozen. Advancement clears the
suspicion, and an older regressing response is ignored as stale.

After confirmed acknowledgement lag, status absence, or a foreign target, no
new calls are planned. The client still applies the normal delivery policy to
the already signed six-tick tail, then discards its untrusted preimages and
waits for the exact slot to be absent. No terminal reveal is created for that
untrusted chain. The replacement first target is both at least six ticks ahead
and later than the frozen signed tail. A target that expires without backend
acceptance outside this frozen recovery still stops the chain immediately; one
later absent response from a query requested after that break permits restart.

Backend acceptance is not proof of contract execution. For RPC and Bob,
provider status comes from their contract-query endpoint. For QubicLightNode,
it is peer-trusted as described below. A target from the local three-tick
sequence advances the local acknowledgement watermark. A foreign target makes
the preimage chain untrustworthy. `GetProviderStatus` cannot distinguish a
competing writer that targets the same stream tick; that conflict becomes
visible only after a later foreign update or disappearance.

## Epochs and shutdown

Wall-clock epoch calculation treats Wednesday 12:00 UTC as the boundary. By
default, 600 seconds before it the client freezes normal planning, finishes the
already planned tail, and sends one terminal `reveal + zero commit` per managed
stream. Normal prerequisites use the same one-acceptance policy. Failed
prerequisite deliveries and the terminal call are retried with identical bytes
until accepted by the backend or expired. No replacement chain is opened until
the backend reports a new epoch.

When the epoch number changes, all old tasks, transactions, and preimages are
discarded. Enrollment is paused until `initial_tick + 50` by default. With
QubicLightNode, which does not expose `initial_tick`, the first structurally
valid peer-reported tick observed in the epoch is used conservatively as the
local initial tick.

Windows console shutdown events and Unix `SIGINT`, `SIGTERM`, `SIGQUIT`, and
`SIGHUP` freeze normal planning too. The client first waits for backend
acceptance of every call in the frozen normal tail; one acceptance per call is
sufficient. When the terminal target enters the six-tick window, it makes one
terminal broadcast attempt per managed stream and exits after all attempts. A
failed/expired prerequisite, a failed terminal attempt, or the 90-second
shutdown deadline returns an error. The client does not wait for a later status
query after terminal backend acceptance.

If status recovery already made a chain untrustworthy, pre-epoch drain or
shutdown waits only for its frozen signed tail and does not append
`reveal + zero commit`.

## QubicLightNode

The companion source is expected at:

```text
D:\Work\MySelf\Qubic\QubicLightNode
```

The protocol tracks the current QubicLightNode `HEAD` in that checkout. Tick
status advances after one exact-size, structurally valid `BroadcastTick` or
`RespondCurrentTickInfo` message. It is deliberately not signature- or
quorum-authenticated. Missing `initial_tick` and `tick_duration_ms` are
normalized to the first observed tick of the epoch and 1,000 ms respectively.

`QueryContractFunction` is an ordinary unary RPC returning one accepted peer
response. RandomClient applies that response directly. A transport error,
malformed output, or timeout discards the observation while current chains
continue. Contract output and tick status are peer-trusted data, not
cryptographic authentication or a Qubic consensus proof. QubicLightNode still
verifies transaction signatures locally before forwarding transactions.
`GetTickTransactions` is used only for delayed empty-tick monitoring and does
not affect provider state or scheduling. QubicLightNode derives its boolean
result from an exact-size Core `TickData` after checking the requested tick,
leader, digest set, current-epoch computor key, K12 digest, and FourQ signature.

`compose.yaml` uses the QubicLightNode checkout as its build context:

```bash
docker compose up --build
```

At the current QubicLightNode `HEAD`, this build is blocked because its
`Dockerfile` does not copy the path dependency `crates/qubic-fourq-verifier`
into the builder. RandomClient intentionally carries no duplicate Dockerfile
workaround. The compose client connects to `http://light-node:50051`.

## Protocol and security notes

[`docs/Random.h`](docs/Random.h) is the smart-contract source of truth. The
client uses contract index `3`, `RevealAndCommit` procedure `1`, and
`GetProviderStatus` function `2`. Procedure input is 544 bytes: a 512-byte
reveal followed by a 32-byte commitment. Status input is the raw 32-byte public
key; output is the aligned 680-byte structure from the header.

Run exactly one writer for an identity/collateral tier. A competing process can
invalidate the local preimage chain before its foreign target appears in the
selected backend's status response. Seeds, preimages, and signed bytes are
never logged or persisted. Broadcasting six ticks early
reveals preimage material to the selected backend before the target tick, so
the backend must be trusted with that availability/confidentiality tradeoff.

Developer-level states and invariants are specified in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
