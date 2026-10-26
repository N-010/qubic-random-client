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
| `src/http.rs` | Bound HTTP response bodies and preserve the required SCAPI RPC/Bob request formats. |
| `src/contract.rs` | Own Random constants and strict wire codecs. |
| `src/entropy.rs` | Generate 4096-bit preimages and KangarooTwelve commitments. |
| `src/engine.rs` | Own three stream states, tick scheduling, status-driven restart, retries, delivery counters, tick-data verification, epoch lifecycle, and drains. |
| `src/console.rs` | Format small, non-sensitive runtime logs and the current delivery-counter snapshot. |
| `proto/lightnode.proto` / `build.rs` | Define and generate the minimal QubicLightNode client. |

The project stays flat and small. `NetworkBackend` is the only operational
transport port selected by CLI; the engine contains no backend-specific
branches. SCAPI remains responsible for wallet/identity handling, transaction
construction and signing, and RPC wire models. The required HTTP calls use a
small bounded transport because the pinned SCAPI clients expose no response-size
limit. RPC paths, payloads and Bob JSON-RPC envelopes are checked against that
pinned implementation in local-server parity tests.

SCAPI is pinned to an immutable revision with default features disabled. The
client uses its Qubic APIs without the optional native WalletConnect module,
CLI, or WalletConnect/Ethereum dependency graph. This keeps unused HTTP stacks
out of the client while preserving SCAPI's signing and protocol behavior.

## Startup flow

1. Configuration accepts a 55-character lowercase seed, one backend, an
   endpoint, a collateral tier, two monitoring settings, and the two epoch
   window settings.
2. A seed omitted from CLI is read without echo or from redirected stdin. Seed
   bytes are locked, redacted from `Debug`, zeroized, and startup fails if
   memory locking fails. Temporary stdin buffers owned by the client are also
   zeroized on error, including partial input retained after a failed read.
   The vendored `rtoolbox` patch used by `rpassword` clears the whole current
   string allocation on drop, including characters removed by terminal editing.
   This does not cover earlier allocations released during string growth.
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

The application selects a 20 ms polling interval for gRPC's local tick cache
and 100 ms for RPC and Bob. It passes the interval to the transport-neutral
engine, which skips missed wakeups instead of accumulating catch-up cycles:

1. Fetch fresh tick metadata with a two-second timeout. Failure aborts only this
   cycle and is retried; an old tick is never substituted to schedule a send.
2. Update epoch/warmup/drain phase from epoch number, initial tick, and UTC time.
3. Harvest the previous provider-status query and independent broadcast tasks.
4. Apply a fresh status observation to each exact `(stream, tier)` state.
5. While active, start eligible absent streams and extend known chains.
6. During pre-epoch drain or shutdown, freeze normal planning and schedule safe
   terminal reveals.
7. Expire missed calls, dispatch eligible calls, finish drains, and prune old
   accepted calls.
8. Start at most one provider-status query, with at least 500 ms between starts.
9. Expire old monitoring checks, harvest a completed check, and start at most one
   eligible check unless draining or shutting down.

Status, transaction broadcasts, and the single delayed tick-data check are
separate bounded Tokio tasks. A slow or failed request does not serialize the
other streams. The engine aborts its status, broadcast, and tick-data tasks on
reset and all owned tasks on drop. A cancellation-safe shutdown observer
remains active while tick calls are awaited and is aborted when the run future
is dropped, including external cancellation. Broadcast dispatch precedes new
status and monitoring requests; faster tick polling does not increase the
provider-status request limit.

## Contract and scheduling invariants

- There are exactly three independent streams.
- A stream `s` targets only ticks where `tick % 3 == s`.
- Consecutive calls in one uninterrupted chain are exactly three ticks apart.
- A fresh first target is the first matching stream tick at or after
  `current_tick + 6` and, after a restart, at least three ticks after the old
  signed tail. Its broadcast may overlap delivery of that old tail.
- Planning stays six ticks ahead. A first commit (zero reveal) becomes
  broadcast-eligible six ticks before its immutable target. Normal and terminal
  reveals become eligible only four ticks before their target, including retries
  and retained old tails. The common dispatcher enforces this before passing
  signed bytes to any backend. If polling skips the boundary, a call is still
  sent while its target remains future.
- One successful backend acceptance completes delivery of a planned call.
  A failed normal or pre-epoch-drain delivery retries identical signed bytes
  until its target. Reveal, commitment, tick, signature, and transaction ID
  are never regenerated or retargeted. Nothing is broadcast at or after the
  target.
- While `Starting` or `Active`, an accepted first commit or normal call gets
  one additional identical-byte broadcast: first commits at four ticks before
  target (or three if polling skips that boundary), normal reveals at three
  ticks before target. Core constructs target `T`'s
  transaction set at `T - 2`; no additional attempt starts at or after that
  publication boundary. This best-effort dissemination attempt does
  not change acceptance, delivery counters, or monitoring deduplication; a
  failure is logged without another retry. No additional attempt starts for
  the old frozen tail or during drain, and terminal reveals are excluded.
  A prepared replacement follows the normal additional-broadcast policy.
  An outstanding attempt
  is cancelled at its target, chain removal, epoch reset, or process drop.
  A normal reveal first accepted at `T - 4` waits until `T - 3` for its
  additional attempt. Late first acceptance at `T - 3` can still be followed
  by the one additional attempt during that tick; none starts at `T - 2`.
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
- While `Starting` or `Active`, if any required target in the current chain
  arrives without backend acceptance, that local chain stops and waits for a
  status query requested after the discontinuity. Expiry of a retained old
  call cannot stop the replacement. `Restarting` follows its frozen-tail rule
  below.
- Backend acceptance proves only submission to the backend, not execution by
  the contract.
- Preimages use OS randomness, commitments use KangarooTwelve, and secrets stay
  only in process memory.

Procedure input is always 544 bytes: 512 reveal bytes followed by 32 commitment
bytes. `GetProviderStatus` input is the raw 32-byte public key and output is the
strict 680-byte aligned layout in `docs/Random.h`.
Reported `(stream, tier)` pairs must be unique, matching the contract's enrollment
rule. Any duplicate makes the entire status response malformed; no entry is
chosen from a contradictory pair.

The publication offset is traced to `TICK_TRANSACTIONS_PUBLICATION_OFFSET = 2`
and `processTick()` in Core `src/qubic.cpp`, revision
`f55b46126c99a1c3f3266164c744b3d0cd694d9c`. Arriving before execution alone does
not establish inclusion; dissemination must reach the leader before selection.
The primary identical-byte retry policy remains bounded by the immutable target.

The commitment consumed by a reveal at `T` is submitted in the same stream at
`T - 3`, whose transaction set is selected at `T - 5`. The four-tick reveal
gate puts first disclosure after that selection under correct backend tick
observations and the stated Core schedule. It is not a proof of consensus
finality or protection against a malicious leader or false tick reports.

Transport retries and the additional broadcast may present identical signed
bytes more than once. If that
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
requeues the check without changing counters. Each attempt has a two-second
timeout. Waiting targets are promoted only after their tick delay, and the
newest eligible target takes priority over older checks and retries. An error
delays that target by 1, 2, 4, 8, 16, then 30 seconds for subsequent retries
within its monitoring window. The scan interval still limits all starts.

The monitoring window ends 32 ticks after the configured eligibility delay
(42 ticks after the target with defaults); its last tick is inclusive. Older
waiting and ready checks are discarded and an older active check is cancelled
before its result can affect counters. The expiry watermark never moves
backwards within an epoch, preventing old checks from being queued again.
Deduplication covers waiting, ready, active, and completed checks within this
window; the watermark replaces expired deduplication entries. Expiry leaves
the existing outcome unverified and does not classify it as empty or failed.
Skipped checks are reported in aggregate at most once per minute. This is a
local monitoring budget, not the contract's transaction-acceptance window.
Epoch reset clears the window and all checks. A non-empty result preserves the
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

`GetTickTransactions` additionally returns all non-zero 32-byte transaction
digests in Core slot order, with `digests_complete=true` only after successful
authentication (including an empty set). The gRPC adapter rejects a different
tick, inconsistent presence flags, malformed/zero/duplicate digests, or more
than Core's 4096 slots. Legacy responses without the completeness marker retain
the presence result but provide no inclusion evidence. RPC and Bob currently
provide only their existing presence observations through this port.

Before an attempted normal or terminal broadcast, the engine retains K12 of
the complete immutable signed bytes for that target. During its existing
delayed check it compares all 32 bytes against the complete digest set and logs
`TickData inclusion for target T: included`, `missing`, or `unknown`. This is
the binary transaction ID, independent of the backend's returned display ID.
Digests are released after a successful check, window expiry, or epoch reset;
errors retain them for the bounded retry window. Only the existing financial
targets are checked; this does not add checks for first commits or successful
terminal reveals. Inclusion observations do not change financial counters,
restart decisions, or sending. The trusted local LightNode authenticates the
leader's TickData, not consensus finality or successful contract execution.

QubicLightNode maintains computor retrieval throughout each peer session. It
rechecks the authenticated epoch at the reconnect interval and requests keys
again when they do not match the observed epoch, with retries capped at 30
seconds. Receipt alone does not establish readiness; the verified set must
have been applied.

## Stream state machine

| State | Meaning | Principal exits |
| --- | --- | --- |
| `Waiting` | No usable local chain. It may be waiting for a fresh absence or for a restart target window. | `Starting` after one eligible absent status response; remains waiting while occupied. |
| `Starting` | A fresh first commit and its predictive continuation are locally known; older frozen calls may still be in delivery. | `Active` after one owned status response; `Restarting` after three consistent eligible absent or foreign responses; `Draining` when requested. |
| `Active` | The local reveal/commit chain is continuously extended. | `Restarting` after three consistent observations of status absence, a foreign target, or acknowledgement lag; `Waiting` after a missed target; `Draining` when requested. |
| `Restarting` | Status made the local chain untrustworthy. Planning of that chain is frozen, but its already signed six-tick tail keeps its normal delivery policy. | `Starting` after eligible exact absence, even before the tail finishes; otherwise `Waiting` after the tail finishes, or `Drained` if epoch drain or shutdown was requested. |
| `Draining` | Normal planning is frozen and a terminal reveal follows the frozen tail. | `Drained` after acceptance or failure/expiry. |
| `Drained` | No more transactions are created in this epoch or shutdown flow. | `Waiting` only after an observed epoch-number change. |

The chain owns its first target, signed tail, outstanding preimage, and planned
immutable transactions. During an overlapping restart its call queue also
retains the older signed calls and their in-flight tasks. Targets below the
new first target are transport-only remnants: they cannot acknowledge the new
chain, trigger its failure, or gate its terminal reveal. No extra state flag
is needed; the first-target boundary distinguishes them. Invalid combinations
are represented by the enum and `Option<Chain>` rather than independent flags.

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

A repeated acknowledgement with no watermark advancement cannot clear a
status suspicion if its request tick is at or below that suspicion's latest
request tick. Actual acknowledgement advancement still clears the suspicion.

The first two consistent observations are logged as non-terminal suspicions and
normal planning continues. The third status absence, non-regressing foreign
target, or unchanged acknowledgement lag freezes the current signed tail
instead of aborting it. No new calls extend that old chain, while all already signed
calls through the old six-tick horizon retain identical-byte retry until
backend acceptance or target expiry. Expiry of one call during this frozen
disposal does not prevent later signed calls from being attempted. Once the
tail target is reached, its remaining transactions are discarded. Its
outstanding preimage is discarded when a replacement is prepared or when the
old chain finishes without one. No terminal reveal is created for the old
chain because its outstanding preimage is no longer trusted.

A replacement still requires one successfully decoded observation that reports
the exact slot absent. An eligible absence observed while the tail is frozen is
retained unless a later status reports the slot occupied again before a
replacement is prepared. During the active epoch, that absence permits
preparing the replacement immediately, without waiting for the old tail's
target. Its first target is the matching stream tick at or after both
`current + 6` and `old_tail + 3`; if farther away, broadcast waits for the fixed
six-tick window. The slot enters `Starting`, keeps the old calls' immutable
bytes and retry tasks, and schedules the new chain normally. Once prepared,
the replacement follows the usual `Starting` status rules and is never
retargeted. Old calls expire independently without another financial failure;
their backend acceptances retain the existing counter and monitoring policy.
Expired old calls are pruned outside drain; during drain they are released
with the completed chain.

After a locally missed target, one absent response from a query requested after
the discontinuity permits a fresh chain. The client never sends a speculative
terminal reveal as failure recovery.

In `Waiting` and `Restarting`, the minimum request tick for a fresh absence
never decreases. An occupied response clears absence evidence, but an older
request cannot lower the boundary established by a discontinuity or newer status.

If a tick jump passes the next unplanned continuation, extension stops before
constructing any backlog of expired transactions. Already planned expirations
take precedence; otherwise the first unplanned missed reveal records one failure.
The chain is discarded and restart still requires a fresh absence observation.
This bounds signing work after an outage without skipping links in a live chain.

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

If status recovery has not yet prepared a replacement, pre-epoch drain lets
the old frozen signed tail finish but never appends a terminal reveal or
starts a replacement. Once a replacement is prepared, drain terminates that
new chain normally; retained old calls are not prerequisites for its terminal
reveal. The slot then remains drained until the new epoch.

## Graceful shutdown

Windows console shutdown events and Unix `SIGINT`, `SIGTERM`, `SIGQUIT`, and
`SIGHUP` freeze normal planning. Each known chain keeps its already planned
normal tail and appends a terminal reveal three ticks later.

Once the shutdown observer completes, the engine also enters drain directly
from local state, even if the tick request failed. Slots without a chain finish
without network recovery. Preparing a terminal reveal does not send it: dispatch
still requires fresh tick metadata and the existing four-tick disclosure gate.

The cycle rechecks the shutdown token after awaiting tick metadata and harvesting
completed tasks, immediately before choosing planning or drain. A signal during
those waits cannot start or extend normal chains using a stale shutdown snapshot.

Normal prerequisites retain identical-byte retry while their targets remain
future. A terminal call waits until every prerequisite has backend acceptance
and its target is within four ticks. Shutdown makes one terminal broadcast
attempt per managed stream, then exits after all streams finish. It does not
wait for subsequent contract status.

A failed/expired prerequisite, a failed terminal attempt, or the 90-second
deadline makes shutdown fail.

If shutdown arrives during an existing pre-epoch drain, it waits on that
already frozen drain under the same overall deadline; it does not create a
second terminal transaction.

If shutdown arrives during status recovery before a replacement is prepared,
it waits for the already frozen signed tail under the same deadline and does
not create a terminal reveal for the untrustworthy chain. A prepared
replacement is drained normally, without making its terminal reveal depend on
the retained old calls.

## Backend boundary

Every backend implements only four operations:

- obtain epoch/current/initial tick and normalized tick duration;
- report whether one historical tick contains data or transactions, with an
  optional complete authenticated transaction-digest set for inclusion checks;
- query a contract function with raw input/output bytes;
- broadcast exact signed transaction bytes and return a non-empty transaction
  identifier.

There is no balance operation or balance-gated enrollment in this client.

Read-operation failures identify the backend and operation without including
transport error text, server error strings, or raw response bodies. gRPC status
codes and numeric JSON-RPC error codes may be reported. This prevents remote
responses and transport URLs from echoing credentials into runtime logs.

Acceptance logs derive the short transaction ID locally from the K12 digest of
the immutable signed bytes. Backend-returned identifiers are never logged,
because even a successful response can echo sensitive request content. This
does not change backend acceptance validation or the identical-byte retry policy.

RPC and Bob successful HTTP bodies are limited to 16 MiB before JSON parsing.
Both declared Content-Length and streamed chunks are checked; the exact limit
is accepted. Oversized bodies are unavailable observations or failed broadcast
attempts, never empty data or acceptance. HTTP error bodies are dropped without
reading them. The limit bounds buffered wire input, not all JSON allocations.
Existing engine timeouts and identical-byte retry rules still apply. The transport
uses the existing reqwest 0.12 dependency and TLS implementation; the SCAPI revision
and transaction construction/signing remain unchanged.

Endpoint suffix normalization and operation-path construction modify only the
part before the first query or fragment delimiter. Query parameters and their
percent encoding are preserved, including trailing slashes in values. Fragments
remain local URL metadata and are not transmitted by the HTTP client. Neither
query parameters nor fragments are included in endpoint diagnostics.

- RPC uses `/live/v1`, `/query/v1` for tick data, and base64 contract payloads.
  A non-null tick-data response must identify the requested tick, including when
  its transaction list is empty. A different tick is unavailable and retried;
  it cannot change monitoring counters. A null tick-data result retains the
  existing presence-only empty interpretation.
  Broadcast responses are accepted only when at least one peer was reached.
  A zero tick duration is normalized to 1,000 ms.
- Bob uses JSON-RPC, tolerant result extraction, single-tick transfer queries,
  monotonic query nonces, and hexadecimal contract payloads. Decoded hex output
  must contain complete ASCII hex-digit pairs; signs, whitespace, and Unicode
  characters are rejected without echoing the response.
  JSON-RPC responses with a `jsonrpc` member must echo the numeric request ID.
  Legacy `result`/`error` envelopes with an explicit ID must also match it.
  Raw legacy results and legacy envelopes without an ID retain their supported
  interpretation; a raw transaction object's `id` alias is not an envelope ID.
  ID matching detects mismatched responses, not dishonest server data.
  A null `error` field is treated as absent; a non-null error takes precedence
  over any result. Required result fields are still validated by each operation.
  JSON-RPC errors expose only an integer error code when present; server-provided
  messages and data are omitted because they may echo sensitive request content.
  Transaction-ID aliases are checked in order for a string value; null or
  non-string fields do not hide a later supported alias. Empty IDs remain errors.
  Scheduling uses `currentVerifyLoggingTick`, not `lastSeenNetworkTick`.
  Verification lag can delay delivery; a local Bob does not itself guarantee
  timely inclusion or authenticate network progress.
- gRPC uses the generated minimal QubicLightNode client, raw bytes, and
  `GetTickTransactions`. Missing initial tick and duration use the conservative
  epoch fallback and 1,000 ms.

The gRPC adapter logs the target and full locally computed transaction digest
before submission for offline inclusion checks. It never logs signed bytes or
preimages.

QubicLightNode offers each locally submitted transaction to six established
peers by default. The user-approved `--broadcast-fanout` availability
adaptation permits 1–32 direct recipients, limited by available connections.
It preserves signed bytes, queue budgets, rate limits, and queue-acceptance
semantics. A wider fanout increases traffic and does not prove inclusion.

QubicLightNode also evaluates peer RTT and tick freshness in a separate bounded
background task. It qualifies one spare connection before replacing an idle
old peer, requires repeated evidence and a material RTT improvement or sustained
poor observations, and applies cooldowns. A temporary candidate adds at most one
socket beyond the working outbound target. Broadcasts never await selection;
the client's T-4 gate, retries and signed bytes are unchanged. This approved
availability policy is not authentication or a guarantee of timely inclusion.

QubicLightNode verifies transaction signatures locally, but tick status comes
from one structurally valid `BroadcastTick` or `RespondCurrentTickInfo` message
without signature or quorum authentication. An ordinary contract-function RPC
returns the first accepted peer response after a freshness preflight on that
same session. Each query snapshots the cached epoch/tick, then requests Core
`RespondCurrentTickInfo` from up to three peers. Only peers in that epoch and
at most one tick behind the snapshot may receive the contract query. A zero
status or missing reference is unavailable. Both stages share the existing
query deadline; no eligible result is an error, never provider absence. Ordinary
lag does not penalize a peer. This filter is a user-approved availability
adaptation, not authentication, and still depends on the unauthenticated cache.
The client uses these responses
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
  CLI diagnostics omit raw seed and endpoint fields; RPC/Bob adapter diagnostics
  omit their underlying transport clients, which can retain endpoint credentials.
  `RevealAndCommitInput` diagnostics redact the complete input, including preimages.
- Exactly one process may write a given identity/tier. A foreign target from a
  competing writer is detected in the selected backend's provider-status
  response. `GetProviderStatus` cannot distinguish two writers that target the
  same stream tick; that conflict is detected only after a later foreign update
  or disappearance. QubicLightNode obtains the contract output from one peer-
  trusted response.
- Broadcast exposes reveal material to the backend and Core's P2P peers.
  Reveal-bearing calls must not leave before observed `T - 4`. This closes the
  client's previous `T - 6` disclosure window, but assumes the backend does not
  report a tick ahead of the relevant network progress. All adapters remain
  trusted for tick observations; switching transport is not a security fix.
- Entropy is separate per collateral tier: the default 10,000 collateral is
  tier 4 and does not contribute to tier 0. Operators must explicitly select
  the tier they intend to support. The client cannot change on-chain fees or
  guarantee unbiased entropy against selective withholding by other providers.

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
