# Random Client Architecture

> **Language policy:** this English document is authoritative. The
> [Russian version](ARCHITECTURE.ru.md) is a synchronized translation and must
> be updated in the same change whenever this document changes. The translation
> must not introduce requirements or interpretations absent from this version.

## Purpose and authority

This document is the canonical description of the client's intended
architecture and runtime behavior. Read it before planning a code change and
compare the completed change against it before considering the work finished.

The sources of truth have separate scopes:

- `docs/ARCHITECTURE.md` defines client responsibilities, component boundaries,
  runtime flows, and invariants.
- `docs/ARCHITECTURE.ru.md` mirrors this document for Russian-speaking readers
  and has no independent normative authority.
- `docs/Random.h` defines the Random smart-contract interface and on-chain
  behavior.
- The Rust code is the executable implementation of the normative architecture
  and contract specifications.

If these sources disagree, do not preserve the disagreement. Establish which
behavior the task intends, then update the implementation and every affected
document in the same change. A change to component ownership, control flow,
protocol assumptions, scheduling, recovery, shutdown, security, or backend
semantics requires a corresponding update here.

## System context

`random-client` is a long-running provider bot for the Qubic Random smart
contract. One process controls one Qubic identity and attempts to maintain one
provider slot at the selected collateral tier in each of the contract's three
streams.

The bot repeatedly submits the contract's `RevealAndCommit` procedure. A new
slot starts with a zero reveal and a commitment. Every later normal call reveals
the preceding 4096-bit preimage and commits to a newly generated one. A terminal
call reveals the outstanding preimage and sends a zero commitment, which tells
the contract to remove the provider after counting the reveal.

The external systems are:

- a Qubic HTTP RPC, Bob JSON-RPC, or QubicLightNode gRPC endpoint;
- the Qubic Random smart contract at contract index `3`;
- the operating system, which supplies cryptographic randomness, protected
  memory, standard input, and shutdown signals.

## Component map

| Component | Responsibility | Must not own |
| --- | --- | --- |
| `src/main.rs` | Enter the Tokio runtime, obtain CLI configuration, invoke the library application, and expose its process result. | Scheduling or protocol logic. |
| `src/app.rs` | Initialize logging, construct the wallet/backend/engine, discard the seed wrapper, and translate OS signals into graceful shutdown. | Backend-specific behavior or slot state transitions. |
| `src/config.rs` | Parse and validate CLI input, normalize endpoints, validate collateral tiers, and protect the seed in locked, zeroized memory. | Network access or runtime scheduling. |
| `src/backend.rs` | Define `NetworkBackend`, select an implementation, and adapt RPC, Bob, and gRPC data to transport-neutral tick, balance, contract-query, and broadcast operations. | Random scheduling policy. |
| `src/bob.rs` | Decode the varying JSON shapes returned by Bob. | Engine state or contract rules. |
| `src/contract.rs` | Own Random constants, encode `RevealAndCommit` input, and strictly decode the QPI `GetProviderStatus` wire layout. | Network transport or transaction timing. |
| `src/entropy.rs` | Generate 4096-bit preimages with the OS RNG and calculate 32-byte KangarooTwelve commitments. | Preimage lifecycle or scheduling. |
| `src/engine.rs` | Own the three slot state machines, bounded predictive queues, transaction construction, status reconciliation, epoch lifecycle, retries, and graceful drain. | Backend wire formats. |
| `src/console.rs` | Maintain process-wide display context and reveal counters and format log output. | Decisions that affect contract calls. |
| `proto/lightnode.proto` / `build.rs` | Define and generate the QubicLightNode gRPC client API. | Domain policy. |

`scapi` remains the boundary for Qubic wallet/identity handling, transaction
construction and signing, and the existing RPC and Bob clients. Random-specific
wire decoding and scheduling remain in this repository.

## Startup flow

1. `main` obtains `AppConfig` from the CLI.
2. The seed must contain exactly 55 lowercase ASCII letters. When it is not an
   argument, it is read without echo from an interactive terminal or from the
   first redirected input line.
3. The seed bytes are locked with `VirtualLock` or `mlock`, redacted from
   `Debug`, zeroized before release, and startup fails if memory locking fails.
4. `app` derives a `QubicWallet`, creates the configured backend, and constructs
   `ProviderEngine` with the wallet and collateral. The configuration's seed is
   then dropped; the wallet necessarily remains available for signing.
5. The engine creates exactly three local slots, for streams `0`, `1`, and `2`.
   All use the tier `log10(collateral)`, where collateral is restricted to a
   power of ten from `1` through `1_000_000_000`.
6. Slots start as `Unknown`. No transaction is created until a successful
   `GetProviderStatus` observation establishes whether each slot is vacant or
   already occupied.

## Main control loop

The engine wakes every 500 ms. Each cycle performs these operations in order:

1. Fetch current tick information. This is the cycle's required input; a
   failure aborts that cycle, is logged by the outer loop, and is retried later.
2. Harvest a completed provider-status query and immediately ensure that at
   most one next query is running.
3. For each stream independently:
   - harvest completed broadcasts;
   - reconcile observed contract state;
   - reconcile a broken or discontinuous chain before sending more work;
   - extend its bounded predictive queue;
   - enter epoch drain or process shutdown when required;
   - dispatch eligible calls and prune obsolete calls.
4. When active and not shutting down, use a fresh balance snapshot to open
   vacant slots, account for in-flight enrollment reservations, and dispatch
   newly created first commits.

Status, balance, and broadcast work run in separate Tokio tasks. A slow request
for one stream must not block transaction planning or broadcasts for the other
streams. Network operations have bounded timeouts; broadcast attempts use the
reported tick duration to stay within their remaining target window. The engine
owns and aborts its outstanding tasks when dropped. Shutdown observation runs
in a separate cancellation-safe task, so an OS signal is not lost while a cycle
is awaiting tick information.

## Contract and scheduling invariants

These rules are architectural invariants and must remain explicit in any change
that touches the engine or contract integration:

- There are exactly three independently managed streams.
- A call for stream `s` targets only a tick where `tick % 3 == s`.
- Normal targets in one uninterrupted predictive segment are exactly three
  ticks apart. Reconciliation may resume a known outstanding preimage at a
  later same-stream target after a gap.
- The initial target is the first tick for that stream at or after
  `current_tick + 6`.
- The predictive queue is extended through `current_tick + 9` and contains at
  most four uncompleted future calls. Calls become eligible for broadcast at
  `current_tick + 6`.
- If polling skips the exact six-tick boundary, an eligible planned call is
  still attempted with the remaining lead. Retries also continue with less
  than six ticks remaining, but all attempts stop when the target tick is
  reached. A call is never retargeted.
- Every planned call owns immutable signed transaction bytes. An ambiguous
  broadcast failure retries those identical bytes so the reveal, commitment,
  target tick, signature, and transaction identity do not change.
- Every non-terminal call owns the next preimage that matches its commitment.
  Preimages are generated with `OsRng`, hashed with KangarooTwelve, and retained
  only in process memory.
- Backend acceptance means only that the backend accepted a broadcast request.
  It is not proof that the smart contract executed the call.
- `GetProviderStatus.lastUpdateTick` is the contract-execution confirmation.
  A locally signed target confirms that target and all earlier targets in the
  same generation.
- Status transport failures, delayed responses, and old successful observations
  do not pause predictive scheduling.
- Balance is consulted only before enrollment. A snapshot is fresh from the
  completion time of its network task, and pending first commits reserve one
  collateral amount until status confirms or rejects enrollment. Existing
  reveal chains continue even if balance queries fail.
- Opening a slot requires two unreserved collateral amounts to be available:
  one for enrollment and one reserved for its next reveal. The shared balance
  snapshot is reduced by one collateral amount after each slot is opened; thus
  opening all three slots from vacant state requires at least four collateral
  amounts.

The procedure input is always 544 bytes: a 512-byte reveal followed by a
32-byte commitment. Contract-facing constants and the 680-byte aligned
`GetProviderStatus` output layout must stay synchronized with `docs/Random.h`.

## Slot state machine

Each `(stream, collateral tier)` slot has one of these states:

| State | Meaning | Principal exits |
| --- | --- | --- |
| `Unknown` | No successful status observation has established ownership yet. | `Vacant` if absent; `Unmanaged` if occupied. |
| `Unmanaged` | The contract has a slot for this identity, but this process does not know its outstanding preimage. | `Vacant` only after status reports the slot absent. |
| `Vacant` | Status reports no slot and enrollment is allowed. | `Predicting` after balance-gated first-commit creation; `Unmanaged` if an external owner appears. |
| `Predicting` | A locally owned generation is being extended and reconciled. | `Reconciling`, `Vacant`, `Unmanaged`, or `Stopping`. |
| `Reconciling` | A scheduling discontinuity was detected; all broadcasts are paused until a status query started after the discontinuity resolves ownership and the last accepted local target. | `Predicting` when a known outstanding preimage can safely resume, `Vacant` if the slot is absent, or `Unmanaged` otherwise. |
| `Stopping` | A proactive terminal reveal is scheduled; no replacement generation may be opened. | `ShutdownComplete` after every prerequisite call and the terminal call are accepted, or after any required call expires. |
| `ShutdownComplete` | This process has finished or exhausted its proactive drain for the slot. | `Unknown` at the next epoch. |

An occupied slot found after process startup is deliberately `Unmanaged`.
Preimages are not persisted, so the bot cannot safely reveal or overwrite that
slot. It waits until the contract reports the exact slot absent before starting
a new generation.

## Prediction reconciliation and restart

For a current generation, the first unconfirmed target `T` remains
inconclusive until a status response that was requested at or after `T + 3`.
If that response still does not report an expected local target, the predictive
chain is considered broken.

Once the chain is considered broken, or once polling has advanced beyond a
target that was never planned, all pending broadcasts stop. The client never
uses `reveal + zero commit` as post-failure recovery: after several rejected or
missed reveals, the locally predicted tail may no longer match the contract's
outstanding commitment.

A status query started after the discontinuity resolves the slot as follows:

1. If it reports a locally signed last target, discard every later prediction,
   recover the preimage committed by that accepted call, and resume with a
   normal `reveal + commit` at a new future target with six ticks of lead.
2. If it reports the exact slot absent, discard the old generation and return
   to `Vacant`. Balance-gated enrollment then starts a new generation with the
   contract's first-commit form: zero reveal and a new non-zero commitment.
3. If it reports an occupied slot whose outstanding preimage cannot be derived
   from a locally accepted call, cancel local work, enter `Unmanaged`, and wait
   until a later status reports the slot absent.

A delayed status response requested before the discontinuity cannot resolve
`Reconciling`. A target at or after the local generation's first target that
was never signed locally also makes the slot `Unmanaged`.

## Epoch lifecycle

The client treats Wednesday 12:00 UTC as the epoch boundary. By default it
proactively drains managed chains during the final 600 seconds before that
boundary and does not enroll new slots. At an observed epoch-number change it
cancels all old-epoch tasks and signed work, resets slots to `Unknown`, and
requires fresh status observations. Enrollment remains paused for the first 50
ticks of the new epoch. Both windows are configurable through
`--stop-before-epoch-end-secs` and `--resume-after-epoch-start-ticks`.

QubicLightNode 0.2.0 does not expose the epoch's initial tick. For that backend,
the first verified-quorum tick observed for an epoch becomes a conservative
local initial tick, including on process startup. Enrollment therefore remains
paused for the configured warmup interval after that first observation.

## Graceful shutdown

Windows console shutdown events and Unix `SIGINT`, `SIGTERM`, `SIGQUIT`, and
`SIGHUP` initiate the same drain sequence:

- stop opening vacant slots;
- retain enough predictive work to preserve the outstanding reveal chain;
- schedule one terminal `reveal + zero commit` three ticks after the frozen
  normal tail;
- never start a replacement generation after the terminal reveal;
- keep retrying every prerequisite and the terminal transaction with identical
  bytes until all are accepted or one reaches its target tick.

`reveal + zero commit` is restricted to this proactive drain and the equivalent
pre-epoch drain. A slot in `Reconciling` cannot be safely drained because its
outstanding preimage is uncertain; it becomes `Unmanaged` instead of sending a
speculative terminal reveal.

Shutdown completes when every slot is unknown, vacant, unmanaged, or drained.
It does not wait for a later contract-status observation after backend
acceptance. Expiry of any required drain call is reported as failure, as is the
overall 90-second deadline.

## Backend boundary

All transports implement the same four operations:

- obtain epoch/current/initial tick information and a normalized tick duration;
- obtain an identity balance;
- query a smart-contract function with raw input and output bytes;
- broadcast exact signed transaction bytes and return a transaction id.

Backend-specific routing and codecs stay behind `NetworkBackend`:

- RPC uses the `/live/v1` API and base64 contract payloads;
- Bob uses JSON-RPC, tolerant response extraction, monotonically increasing
  query nonces, and hexadecimal contract payloads;
- gRPC uses the generated QubicLightNode client and raw bytes. QubicLightNode
  0.2.0 reports epoch and tick only after a FourQ-verified quorum of 451
  computors. Its unavailable zero `initial_tick` is replaced by the first
  verified tick observed for that epoch, and its unavailable zero
  `tick_duration_ms` is normalized to 1,000 ms at the backend boundary.

Adding or modifying a backend must not introduce backend-specific branches in
the engine. In particular, all backends must preserve exact transaction bytes
and expose generic Random status queries with equivalent meaning.

## Failure and security model

- A failed cycle is logged and retried; it does not terminate the process.
- QubicLightNode status remains unavailable until a verified tick quorum is
  established; this is handled as a tick-information failure and retried.
- Provider-status failures preserve the current predictive state.
- Balance failures block only new enrollment.
- Broadcast failures enter identical-byte retry without blocking other calls.
- Empty transaction identifiers are rejected as failed broadcasts.
- Invalid contract output is rejected rather than partially decoded.
- Arithmetic that establishes generation or target ordering is checked or
  saturating so wraparound cannot create an invalid call.
- Seeds, signed transaction bytes, and preimages must never appear in logs,
  errors, `Debug` output, or persisted application state. Explicit startup and
  configuration `Debug` output must redact endpoint credentials, query
  parameters, and fragments.
- Exactly one running process may write a given identity/tier. This is a hard
  operational precondition: a competing writer cannot be detected before it
  acts. An unknown contract update is detected only afterward, at which point
  the local slot becomes unmanaged.
- Broadcasting reveals before their target tick exposes them to the selected
  backend. The six-tick lead is an accepted availability-versus-confidentiality
  tradeoff and assumes that backend will not exploit early reveal material.

The bot cannot guarantee a reveal when the process, host, network, or backend
is unavailable through its target tick. Collateral loss caused by such an
outage is an inherent operational risk of the commit/reveal protocol.

## Change consistency checklist

Before implementation:

1. Identify the affected components and invariants in this document.
2. Check `docs/Random.h` for any contract-facing assumption in scope.
3. Plan how the change preserves the model or explicitly changes it.

Before completion:

1. Compare the final diff and observed behavior with this entire document.
2. Update this document in the same change if responsibilities, dependencies,
   flows, states, timing, protocol layouts, failure handling, or security
   properties changed.
3. Update `docs/ARCHITECTURE.ru.md` in the same change whenever this English
   document changes, and verify that its structure and meaning still match.
4. Check README for user-visible CLI or runtime changes and `docs/Random.h` for
   smart-contract-facing changes.
5. Run formatting and the relevant test suite. Treat tests as verification,
   not as a replacement for the architecture comparison.
6. Do not finish with a known mismatch between either language version,
   documentation, and code.
