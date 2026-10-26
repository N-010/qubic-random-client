# Changelog

All notable changes to this project will be documented in this file.

## v2.1.0 - 2026-10-26

### Security

- Delay normal and terminal reveal broadcasts until T-4, including retries,
  while retaining six-tick planning and early first-commit submission.
- Bound HTTP response bodies and redact backend errors, returned identifiers,
  credentials, and transaction material from diagnostics.
- Zeroize temporary seed-input buffers, including failure paths; patch vendored
  rtoolbox to clear the current allocation, including terminal-deleted bytes.
- Update dependencies to address advisory and dependency-policy findings.

### Added

- Report transaction inclusion by comparing the full locally computed hash
  with authenticated TickData digests supplied by QubicLightNode. Inclusion
  is distinct from contract execution and does not alter financial counters.
- Poll the local gRPC tick cache every 20 ms without increasing the
  provider-status request rate.

### Fixed

- Harden immutable-byte retries, target expiry, overlapping restart handling,
  status-observation freshness, tick jumps, epoch cleanup, and shutdown.
- Reject duplicate provider stream/tier entries and inconsistent TickData
  digest responses instead of treating them as usable observations.
- Require positive RPC peer acceptance; validate Bob response IDs and errors,
  reject malformed hexadecimal payloads, and preserve endpoint query strings.

### Changed

- Gate release publication on successful Linux and Windows builds, matching
  package/tag versions, binary version/help checks, and SHA256 verification.
- Publish SHA256SUMS; add manual CI, weekly dependency checks, job timeouts,
  and write permission restricted to the publication job.
- Expand regression coverage and architecture documentation; organize
  repository ignore rules while retaining vendored sources and Cargo.lock.

## v2.0.3 - 2026-09-09

### Fixed

- Prioritize transaction delivery with 100 ms tick polling and two-second
  tick-query timeouts, while keeping provider-status queries rate limited.
- Add one identical-byte broadcast before Core's transaction publication
  boundary to improve dissemination without changing delivery counters.
- Prepare replacement registrations while the old signed tail finishes,
  preserving the six-tick send lead, three-tick spacing, and fresh-absence
  requirement. Old retries cannot stop the replacement or block its drain.
- Prioritize recent eligible tick checks and expire checks outside their
  bounded monitoring window, preventing an accumulating historical backlog.

### Changed

- Expand regression coverage for broadcast timing, overlapping recovery,
  monitoring expiry, and task cancellation; synchronize architecture and usage
  documentation.

## v2.0.2 - 2026-09-08

### Security

- Update h2 to 0.4.16 to fix RUSTSEC-2026-0258.
- Disable SCAPI's optional WalletConnect integration, removing the unused
  legacy h2 0.3 and WalletConnect/Ethereum dependency graph.
- Remove obsolete RustSec exceptions; audit and dependency policy checks now
  run without advisory exclusions.
- Replace the yanked chacha20 0.10.1 with 0.10.2.

## v2.0.1 - 2026-09-08

### Fixed

- Prevent unavailable tick checks from blocking other ready targets, with
  deduplicated retries and exponential backoff capped at 30 seconds.
- Document matching QubicLightNode epoch-key refresh and peer freshness
  checks, preserving scheduling and shutdown invariants.

## v2.0.0

### Changed

- Refactored the client around one transport-neutral engine for the RPC, Bob,
  and gRPC backends.
- Added status-confirmed recovery, immutable-target retries, epoch drain and
  warmup handling, delayed empty-tick monitoring, and graceful shutdown.
- Removed balance-gated enrollment and kept provider status as the source of
  truth for starting and restarting chains.

### Security

- Locked and zeroized seed handling and kept sensitive transaction material
  out of logs and persistent state.
- Added dependency advisory, license, and source-policy checks for release
  preparation.
