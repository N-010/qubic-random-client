# Changelog

All notable changes to this project will be documented in this file.

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
