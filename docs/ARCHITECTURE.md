# Random Client Architecture

> **Language policy:** this English document is authoritative. The
> [Russian version](ARCHITECTURE.ru.md) is a synchronized translation and must
> change with it. The translation must not add independent requirements.

## Purpose and authority

This document defines the intended responsibilities, runtime flows, state
transitions, protocol assumptions, and security invariants of RandomClient.
`docs/Random.h` defines the Random smart-contract interface and on-chain
behavior. The Rust code implements both documents. README is the canonical
user-facing usage guide.

Known disagreement between these sources must be resolved in the same change.

## System context

RandomClient is a long-running provider bot. One process controls one Qubic
identity and attempts to maintain one provider chain at the selected collateral
tier in each of Random's three streams.

A fresh chain starts with `zero reveal + non-zero commit`. Every normal call
reveals the previous 4096-bit preimage and commits to a new one. A proactive
terminal call uses the known outstanding preimage with a zero commitment; the
contract counts that reveal and removes the provider at end of tick.

The external boundaries are Qubic HTTP RPC, Bob JSON-RPC, or QubicLightNode
gRPC; the Random contract at index `3`; and the operating system for secure
randomness, protected seed memory, time, standard input, and shutdown signals.

## Component map

| Component | Responsibility |
| --- | --- |
| `src/main.rs` | Enter Tokio, parse configuration, and expose the process result. |
| `src/app.rs` | Construct wallet, backend, and engine; translate OS signals into graceful shutdown. |
| `src/config.rs` | Parse CLI input, normalize endpoints, validate collateral, and keep the seed locked and zeroized. |
| `src/backend.rs` | Define the transport-neutral port and adapt RPC, Bob, and gRPC. |
| `src/bob.rs` | Decode Bob's varying JSON response shapes. |
| `src/contract.rs` | Own Random constants and strict wire codecs. |
| `src/entropy.rs` | Generate 4096-bit preimages and KangarooTwelve commitments. |
| `src/engine.rs` | Own three stream states, tick scheduling, status-driven restart, retries, epoch lifecycle, and drains. |
| `src/console.rs` | Format small, non-sensitive runtime logs. |
| `proto/lightnode.proto` / `build.rs` | Define and generate the minimal QubicLightNode client. |

The project stays flat and small. `NetworkBackend` is the only operational
transport port selected by CLI; the engine contains no backend-specific
branches. SCAPI remains responsible for wallet/identity handling, transaction
construction and signing, and the existing RPC and Bob clients.

## Startup flow

1. Configuration accepts a 55-character lowercase seed, one backend, an
   endpoint, a collateral tier, and the two epoch window settings.
2. A seed omitted from CLI is read without echo or from redirected stdin. Seed
   bytes are locked, redacted from `Debug`, zeroized, and startup fails if
   memory locking fails.
3. The app derives a `QubicWallet`, constructs the chosen backend and engine,
   and drops the seed wrapper.
4. The engine creates exactly three slots for streams `0`, `1`, and `2`, all at
   `log10(collateral)`.
5. No stream starts until one successfully decoded provider-status response
   requested at an eligible tick reports that exact `(stream, tier)` absent,
   and epoch warmup has completed.

An occupied slot found after process startup is not overwritten. The process
does not persist preimages and therefore waits until that exact slot disappears.

## Main loop

The engine wakes every 500 ms:

1. Fetch tick metadata. Failure aborts only this cycle and is retried.
2. Update epoch/warmup/drain phase from epoch number, initial tick, and UTC time.
3. Harvest the previous provider-status query and start at most one next query.
4. Harvest independent broadcast tasks.
5. Apply a fresh status observation to each exact `(stream, tier)` state.
6. While active, start eligible absent streams and extend known chains.
7. During pre-epoch drain or shutdown, freeze normal planning and schedule safe
   terminal reveals.
8. Expire missed calls, dispatch eligible calls, finish drains, and prune old
   accepted calls.

Status and transaction broadcasts are separate bounded Tokio tasks. A slow or
failed request does not serialize the other streams. The engine aborts its
status and broadcast tasks on reset and all owned tasks on drop. A
cancellation-safe shutdown observer remains active while tick calls are
awaited.

## Contract and scheduling invariants

- There are exactly three independent streams.
- A stream `s` targets only ticks where `tick % 3 == s`.
- Consecutive calls in one uninterrupted chain are exactly three ticks apart.
- A fresh first target is the first matching stream tick at or after
  `current_tick + 6` and, after a restart, later than the discarded signed tail.
- A planned transaction becomes broadcast-eligible at most six ticks before
  its immutable target. If polling skips that boundary, it is still sent while
  the target remains future.
- One successful backend acceptance completes delivery of a planned call.
  A failed normal or pre-epoch-drain delivery retries identical signed bytes
  until its target. Reveal, commitment, tick, signature, and transaction ID
  are never regenerated or retargeted. Nothing is broadcast at or after the
  target.
- Planning continues before first-commit confirmation and while successful
  status observations contain an older locally signed `lastUpdateTick`.
- If any required target arrives without backend acceptance, that local chain
  stops and waits for a status query requested after the discontinuity.
- Backend acceptance proves only submission to the backend, not execution by
  the contract.
- Preimages use OS randomness, commitments use KangarooTwelve, and secrets stay
  only in process memory.

Procedure input is always 544 bytes: 512 reveal bytes followed by 32 commitment
bytes. `GetProviderStatus` input is the raw 32-byte public key and output is the
strict 680-byte aligned layout in `docs/Random.h`.

Transport retries may present identical signed bytes more than once. If that
reaches the contract twice in the target tick, `Random.h` rejects the second
normal reveal through its same-tick flag and rejects the second first commit
because the provider is already present; both paths refund and return without
another state mutation.

## Stream state machine

| State | Meaning | Principal exits |
| --- | --- | --- |
| `Waiting` | No usable local chain. It may be waiting for a fresh absence or for a restart target window. | `Starting` after one eligible absent status response; remains waiting while occupied. |
| `Starting` | A fresh first commit and its predictive continuation are locally known. | `Active` after one owned status response; `Waiting` after one eligible absent or foreign response; `Draining` when requested. |
| `Active` | The local reveal/commit chain is continuously extended. | `Waiting` after one absent or foreign response, or a missed target; `Draining` when requested. |
| `Draining` | Normal planning is frozen and a terminal reveal follows the frozen tail. | `Drained` after acceptance or failure/expiry. |
| `Drained` | No more transactions are created in this epoch or shutdown flow. | `Waiting` only after an observed epoch-number change. |

The chain owns its first target, signed tail, outstanding preimage, and planned
immutable transactions. Invalid combinations are represented by the enum and
`Option<Chain>` rather than independent flags.

## Status-driven restart

The engine continuously runs one `GetProviderStatus` request at a time. A
transport failure, timeout, or malformed response is discarded and current
chains remain unchanged. Each successfully decoded response directly
classifies an exact `(stream, tier)` as owned, absent, or foreign.

An owned response has a `lastUpdateTick` in the uninterrupted local target
sequence. Older or delayed owned ticks do not stop planning. An absent response
stops an `Active` chain immediately. For `Starting`, absence is ignored until
the response was requested after the first target could execute. A target
outside the local sequence means the local preimage chain is no longer
trustworthy. The engine discards it and waits for a later successfully decoded
response that reports the exact slot absent; it does not send a terminal reveal
for the discarded chain.

On observed disappearance the engine aborts old broadcasts, drops old
transactions and preimages, and records the old signed tail. The same absent
response permits a replacement immediately; its first target is the matching
stream tick at or after both `current + 6` and `old_tail + 3`. If the target is
farther away, broadcast waits until it enters the fixed six-tick window.

After a locally missed target, one absent response from a query requested after
the discontinuity permits a fresh chain. The client never sends a speculative
terminal reveal as failure recovery.

## Epoch lifecycle

The wall-clock boundary is Wednesday 12:00 UTC. By default, the engine enters
pre-epoch drain during the final 600 seconds. The lead is configured by
`--stop-before-epoch-end-secs`.

When the backend reports a new epoch number, all previous-epoch queries,
broadcast tasks, signed transactions, and preimages are discarded. Every
stream returns to `Waiting` and requires a new status observation. Enrollment
is paused until `initial_tick + 50` by default, configurable with
`--resume-after-epoch-start-ticks`.

QubicLightNode `HEAD` does not expose the epoch's initial tick. Its adapter uses
the first verified-quorum tick observed in that epoch as a conservative local
initial tick, including at process startup.

## Pre-epoch drain

Pre-epoch drain is intentionally retained:

- stop enrollment and normal chain extension;
- keep the already signed normal tail and outstanding preimage;
- append one `reveal + zero commit` three ticks after that tail;
- wait for backend acceptance of every normal prerequisite before sending the
  terminal transaction;
- retry failed prerequisite deliveries and the terminal with identical bytes
  until accepted or their target expires;
- remain idle after completion or expiry until a new epoch is observed.

Slots without a known chain require no terminal call. Contract status is not
used to start a replacement while the epoch is draining.

## Graceful shutdown

Windows console shutdown events and Unix `SIGINT`, `SIGTERM`, `SIGQUIT`, and
`SIGHUP` freeze normal planning. Each known chain keeps its already planned
normal tail and appends a terminal reveal three ticks later.

Normal prerequisites retain identical-byte retry while their targets remain
future. A terminal call waits until every prerequisite has backend acceptance
and its target is within six ticks. Shutdown makes one terminal broadcast
attempt per managed stream, then exits after all streams finish. It does not
wait for subsequent contract status.

A failed/expired prerequisite, a failed terminal attempt, or the 90-second
deadline makes shutdown fail.

If shutdown arrives during an existing pre-epoch drain, it waits on that
already frozen drain under the same overall deadline; it does not create a
second terminal transaction.

## Backend boundary

Every backend implements only three operations:

- obtain epoch/current/initial tick and normalized tick duration;
- query a contract function with raw input/output bytes;
- broadcast exact signed transaction bytes and return a non-empty transaction
  identifier.

There is no balance operation or balance-gated enrollment in this client.

- RPC uses `/live/v1` and base64 contract payloads. A zero tick duration is
  normalized to 1,000 ms.
- Bob uses JSON-RPC, tolerant result extraction, monotonic query nonces, and
  hexadecimal contract payloads.
- gRPC uses the generated minimal QubicLightNode client and raw bytes. Missing
  initial tick and duration use the conservative epoch fallback and 1,000 ms.

QubicLightNode verifies tick quorum and transaction signatures locally, but an
ordinary contract-function RPC returns one accepted peer response. The client
uses that single response directly, as it does for RPC and Bob. For
QubicLightNode this is peer-trusted data, not cryptographic authentication or a
Qubic consensus proof.

## Failure and security model

- Cycle, status, and broadcast failures are logged and retried according to the
  state rules; unrelated streams continue.
- Empty transaction identifiers and malformed contract output are rejected.
- Target arithmetic is checked or saturating where clamping is intentional;
  overflow never creates a wrapped target.
- Insufficient balance is not preflighted. A rejected first commit eventually
  appears as fresh status absence and follows the normal fresh-chain restart.
- Seeds, preimages, signed bytes, credentials, query parameters, and fragments
  must never appear in logs, errors, `Debug`, or persisted runtime state.
- Exactly one process may write a given identity/tier. A competing writer can
  only be detected after its target appears in the selected backend's status
  response; QubicLightNode status is one peer-trusted response.
- Six-tick early broadcast exposes reveal material to the selected backend.
  This is an explicit availability/confidentiality tradeoff.

The client cannot guarantee a reveal while the process, host, network, or
backend is unavailable through the target tick. Collateral loss from such an
outage is an inherent commit/reveal risk.

## Change consistency checklist

Before implementation, read this file fully, identify affected invariants, and
check `docs/Random.h` for contract-facing work. Before completion, compare the
final diff and behavior with this file, synchronize `ARCHITECTURE.ru.md`, check
README and `Random.h`, then run formatting and tests. No known discrepancy may
remain between either language version, documentation, contract header, and
code.
