# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

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
