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
| `src/config.rs` | Parse CLI input, normalize endpoints, validate collateral and monitoring intervals, and keep the seed locked and zeroized. |
| `src/backend.rs` | Define the transport-neutral port and adapt RPC, Bob, and gRPC, including delayed tick-data checks. |
| `src/bob.rs` | Decode Bob's varying JSON response shapes. |
| `src/contract.rs` | Own Random constants and strict wire codecs. |
| `src/entropy.rs` | Generate 4096-bit preimages and KangarooTwelve commitments. |
| `src/engine.rs` | Own three stream states, tick scheduling, status-driven restart, retries, delivery counters, tick-data verification, epoch lifecycle, and drains. |
| `src/console.rs` | Format small, non-sensitive runtime logs and the current delivery-counter snapshot. |
| `proto/lightnode.proto` / `build.rs` | Define and generate the minimal QubicLightNode client. |

The project stays flat and small. `NetworkBackend` is the only operational
transport port selected by CLI; the engine contains no backend-specific
branches. SCAPI remains responsible for wallet/identity handling, transaction
construction and signing, and the existing RPC and Bob clients.

## Startup flow

1. Configuration accepts a 55-character lowercase seed, one backend, an
   endpoint, a collateral tier, two monitoring settings, and the two epoch
   window settings.
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
3. Harvest the previous delayed tick-data check and start at most one due check.
4. Harvest the previous provider-status query and start at most one next query.
5. Harvest independent broadcast tasks.
6. Apply a fresh status observation to each exact `(stream, tier)` state.
7. While active, start eligible absent streams and extend known chains.
8. During pre-epoch drain or shutdown, freeze normal planning and schedule safe
   terminal reveals.
9. Expire missed calls, dispatch eligible calls, finish drains, and prune old
   accepted calls.

Status, transaction broadcasts, and the single delayed tick-data check are
separate bounded Tokio tasks. A slow or failed request does not serialize the
other streams. The engine aborts its status, broadcast, and tick-data tasks on
reset and all owned tasks on drop. A cancellation-safe shutdown observer
remains active while tick calls are awaited.

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
- RPC acceptance requires both a non-empty transaction identifier and a
  positive `peersBroadcasted` count. A zero or invalid peer count is a
  temporary delivery failure and retains the identical-byte retry policy.
- Planning continues without waiting for each target confirmation. Absence, a
  foreign target, or an older local `lastUpdateTick` starts a suspicion, but
  planning continues through the first two confirmations. Only a third
  consistent successfully decoded observation requested at a later tick
  freezes the chain. Status advancement clears the suspicion; duplicate or
  regressing request ticks do not advance it, and a regressing local tick is
  treated as stale.
- While `Starting` or `Active`, if any required target arrives without backend
  acceptance, that local chain stops and waits for a status query requested
  after the discontinuity. `Restarting` follows its frozen-tail rule below.
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

## Delivery counters and empty-tick monitoring

The engine keeps cumulative process-lifetime financial outcome counters. A
normal `reveal + commit` accepted by the backend starts as `ok`. Temporary
broadcast errors do not count. Reaching a normal immutable target without
acceptance records one `failed`. Three consistent absence or acknowledgement-
lag observations that freeze an active local chain reclassify its first
unconfirmed normal target from `ok` to `failed`; foreign-writer evidence does
not. The same target is never counted twice, and later signed calls in the
untrusted tail do not add failures because `Random.h` removes the provider
after the first missed reveal and refunds those rejected calls.

First commits are excluded. Successful terminal reveals are also excluded from
`ok`, but a failed terminal attempt or an attempted terminal reveal that
expires records one `failed` because it can leave the outstanding collateral
exposed to the contract's no-show path. A terminal that was never attempted
after a prerequisite failed does not count the same collateral twice.

An accepted or failed financial target is queued once for a delayed check. By
default the check becomes eligible after 10 ticks and at most one check is
active; the scan interval defaults to 600 ms. `--reveal-verify-after` and
`--empty-check-ms` configure positive values. A backend error or timeout
requeues the check without changing counters. A non-empty result preserves the
current outcome; an empty result moves one outcome from `ok` or `failed` to
`empty`.

Empty means that the selected backend reports no data or transactions for the
target tick as a whole. It does not prove whether this client's transaction
executed. RPC uses tick data, Bob uses the single-tick transfers response, and
QubicLightNode uses `GetTickTransactions`. Pending checks are discarded on an
epoch reset or process drop, their already counted outcome remains unchanged,
and monitoring never delays drain or graceful shutdown. A shutdown terminal
failure can therefore remain `failed` when the process exits before its delayed
check. The three counters remain cumulative across epoch changes and are
appended to logs after the first outcome.

The QubicLightNode response is derived from an exact-size Core `TickData` only
after validation of the requested tick, designated leader, unique non-zero
digest set, active arbitrator-authenticated computor key, K12 digest, and FourQ
signature. Missing authentication state is an unavailable observation, not an
empty tick.

## Stream state machine

| State | Meaning | Principal exits |
| --- | --- | --- |
| `Waiting` | No usable local chain. It may be waiting for a fresh absence or for a restart target window. | `Starting` after one eligible absent status response; remains waiting while occupied. |
| `Starting` | A fresh first commit and its predictive continuation are locally known. | `Active` after one owned status response; `Restarting` after three consistent eligible absent or foreign responses; `Draining` when requested. |
| `Active` | The local reveal/commit chain is continuously extended. | `Restarting` after three consistent observations of status absence, a foreign target, or acknowledgement lag; `Waiting` after a missed target; `Draining` when requested. |
| `Restarting` | Status made the local chain untrustworthy. Planning is frozen, but its already signed six-tick tail keeps its normal delivery policy. | `Waiting` after the frozen tail finishes; `Drained` if epoch drain or shutdown was requested. |
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

An owned response has a `lastUpdateTick` in the uninterrupted signed local
target sequence. The chain keeps the greatest such tick as an acknowledgement
watermark. For a status query requested at tick `R`, only signed targets below
`R` are due. The chain records one status suspicion at a time: repeated absence,
non-regressing foreign targets, or the same first due target left unconfirmed.
The evidence must agree across three successfully decoded observations with
strictly increasing request ticks. Switching evidence starts again at one;
duplicate or regressing request ticks do not count. Advancement resets the
suspicion, and a response below the acknowledgement watermark is stale and
cannot confirm a lag. For `Starting`, absence and foreign targets are ignored
until the response was requested after the first target could execute.

The first two consistent observations are logged as non-terminal suspicions and
normal planning continues. The third status absence, non-regressing foreign
target, or unchanged acknowledgement lag freezes the current signed tail
instead of aborting it. No new calls are then planned, while all already signed
calls through the old six-tick horizon retain identical-byte retry until
backend acceptance or target expiry. Expiry of one call during this frozen
disposal does not prevent later signed calls from being attempted. Once the
tail target is reached, its transactions and preimages are discarded. No
terminal reveal is created because the outstanding preimage is no longer
trusted.

A replacement still requires one successfully decoded observation that reports
the exact slot absent. An eligible absence observed while the tail is frozen is
retained unless a later status reports the slot occupied again. The replacement
first target is the matching stream tick at or after both `current + 6` and
`old_tail + 3`; if farther away, broadcast waits for the fixed six-tick window.

After a locally missed target, one absent response from a query requested after
the discontinuity permits a fresh chain. The client never sends a speculative
terminal reveal as failure recovery.

## Epoch lifecycle

The wall-clock boundary is Wednesday 12:00 UTC. By default, the engine enters
pre-epoch drain during the final 600 seconds. The lead is configured by
`--stop-before-epoch-end-secs`.

When the backend reports a new epoch number, all previous-epoch status and
tick-data queries, broadcast tasks, signed transactions, and preimages are
discarded. Every stream returns to `Waiting` and requires a new status
observation. Enrollment is paused until `initial_tick + 50` by default,
configurable with `--resume-after-epoch-start-ticks`.

The current compatible QubicLightNode schema does not expose the epoch's
initial tick. Its adapter uses the first structurally valid peer-reported tick
observed in that epoch as a conservative local initial tick, including at
process startup.

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

If status recovery already marked a chain untrustworthy, pre-epoch drain lets
its frozen signed tail finish but never appends a terminal reveal. The slot then
remains drained until the new epoch.

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

If shutdown arrives during status recovery, it waits for the already frozen
signed tail under the same deadline and does not create a terminal reveal for
the untrustworthy chain.

## Backend boundary

Every backend implements only four operations:

- obtain epoch/current/initial tick and normalized tick duration;
- report whether one historical tick contains data or transactions;
- query a contract function with raw input/output bytes;
- broadcast exact signed transaction bytes and return a non-empty transaction
  identifier.

There is no balance operation or balance-gated enrollment in this client.

- RPC uses `/live/v1`, `/query/v1` for tick data, and base64 contract payloads.
  Broadcast responses are accepted only when at least one peer was reached.
  A zero tick duration is normalized to 1,000 ms.
- Bob uses JSON-RPC, tolerant result extraction, single-tick transfer queries,
  monotonic query nonces, and hexadecimal contract payloads.
- gRPC uses the generated minimal QubicLightNode client, raw bytes, and
  `GetTickTransactions`. Missing initial tick and duration use the conservative
  epoch fallback and 1,000 ms.

QubicLightNode verifies transaction signatures locally, but tick status comes
from one structurally valid `BroadcastTick` or `RespondCurrentTickInfo` message
without signature or quorum authentication. An ordinary contract-function RPC
likewise returns one accepted peer response. The client uses these responses
directly, as it does for RPC and Bob. For QubicLightNode they are peer-trusted
data, not cryptographic authentication or a Qubic consensus proof.

## Failure and security model

- Cycle, status, broadcast, and tick-data-check failures are logged and retried
  according to their state rules; unrelated streams continue.
- Empty transaction identifiers and malformed contract output are rejected.
- Target arithmetic is checked or saturating where clamping is intentional;
  overflow never creates a wrapped target.
- Insufficient balance is not preflighted. A rejected first commit eventually
  appears as fresh status absence and follows the normal fresh-chain restart.
- Seeds, preimages, signed bytes, credentials, query parameters, and fragments
  must never appear in logs, errors, `Debug`, or persisted runtime state.
- Exactly one process may write a given identity/tier. A foreign target from a
  competing writer is detected in the selected backend's provider-status
  response. `GetProviderStatus` cannot distinguish two writers that target the
  same stream tick; that conflict is detected only after a later foreign update
  or disappearance. QubicLightNode obtains the contract output from one peer-
  trusted response.
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
