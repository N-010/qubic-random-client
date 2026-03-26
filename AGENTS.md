# Repository Guidelines

## Overview

This repository contains a Rust client for the Qubic Random smart contract.
The client runs the `commit -> reveal` flow for `RANDOM::RevealAndCommit()`
and supports multiple backends (`rpc`, `bob`, `grpc`).

## Project Structure

- `src/` contains the application code.
- `src/main.rs` is the binary entry point.
- `src/config.rs` defines CLI parsing and runtime configuration.
- `src/app.rs` wires runtime components together.
- `src/pipeline.rs` contains the commit/reveal scheduling logic.
- `src/transport.rs` contains transport adapters.
- `src/qln/` contains QubicLightNode gRPC integration.
- `docs/Random.h` is the SC interface header.
- `README.md` is the main user-facing documentation.
- `Cargo.toml` defines package metadata and dependencies.
- `Cargo.lock` is committed for deterministic builds.
- `target/` contains build artifacts and must not be edited manually.

## Build And Run

Run commands from the repository root.

- `cargo build --release` builds the client.
- `cargo run --release -- --help` shows CLI help.
- `cargo run --release -- --seed <seed>` runs the client.
- `cargo test` runs the test suite.
- `cargo fmt` formats the Rust code.

## Coding Guidelines

Use standard Rust conventions unless the codebase already establishes a more
specific local pattern.

- Use 4-space indentation.
- Use `snake_case` for functions, modules, and variables.
- Use `PascalCase` for types and enums.
- Use `SCREAMING_SNAKE_CASE` for constants.
- Keep comments short and only where they add real value.
- Prefer exhaustive `match` statements over wildcard arms when practical.
- Collapse nested `if` statements where that improves clarity.
- Inline `format!` arguments when possible, for example `format!("{value}")`.

## Testing

The repository contains unit and integration-style tests in `src/` and `tests/`.

- Add tests for behavior changes when practical.
- Prefer asserting full objects or full outputs instead of field-by-field checks.
- Run `cargo test` after code changes.
- Run `cargo fmt` after Rust edits.

## Documentation

- Keep `README.md` aligned with the actual CLI and runtime behavior.
- If SC-facing structures or protocol assumptions change, verify whether
  `docs/Random.h` or `README.md` also need updates.
- Do not add duplicate user-facing usage docs; `README.md` is the canonical
  usage document for this repository.

## Dependencies And Integration

- This crate uses Rust edition 2024.
- If you add dependencies, update `Cargo.toml` and commit the resulting
  `Cargo.lock`.
- For SC integration, use the `scapi` library already referenced by the project
  unless there is a clear reason to change that.

## Commits And Pull Requests

- Use short, imperative commit messages.
- Keep pull request descriptions concise and include verification steps.
- If behavior changes are user-visible, mention the expected CLI usage or output.
