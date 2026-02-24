# SC Random Alignment Checklist

This file tracks required changes so the client behavior matches the current SC implementation in:

- `D:\Work\Qubic\core-lite\src\contracts\Random.h`

Status legend:

- `[x]` done
- `[~]` partially done
- `[ ]` not done

Update rule:

- Every new change related to SC compatibility must update this file.
- Completed items must be marked `[x]`.
- Partial work must be marked `[~]` with a short note of what was done and what remains.

## Current compliance status

1. `[x]` Stop-window reveal-only must send the same collateral amount, not `0`.
   - Done: stop-window reveal-only now sends `commit=0` with pending collateral amount.
   - References:
     - `src/pipeline.rs` (stop-window reveal-only branch in `emit_reveal_and_commit_job`)
     - `D:\Work\Qubic\core-lite\src\contracts\Random.h` (`switch (qpi.invocationReward())`)

2. `[x]` Shutdown reveal-only must send `commit=0` and the corresponding collateral amount.
   - Done: shutdown reveal now sends `commit=0` and uses pending collateral amount.
   - References:
     - `src/pipeline.rs` (`build_shutdown_job`)
     - `D:\Work\Qubic\core-lite\src\contracts\Random.h` (`if (input.commit == id::zero())`)

3. `[x]` Validate `--commit-amount` against SC collateral tiers.
   - Done: CLI config validation now accepts only `1, 10, 100, ..., 1_000_000_000`.
   - References:
     - `src/config.rs` (CLI parsing and config building)
     - `D:\Work\Qubic\core-lite\src\contracts\Random.h` (`switch (qpi.invocationReward())`)

4. `[x]` Validate `--reveal-delay-ticks` for stream consistency.
   - Done: CLI config validation now requires a positive multiple of 3.
   - References:
     - `src/config.rs` (`reveal_delay_ticks`)
      - `D:\Work\Qubic\core-lite\src\contracts\Random.h` (`locals.stream = mod<uint32>(qpi.tick(), 3)`)

5. `[x]` Synchronize local SC docs with actual contract.
   - Done: `docs/Random.h` synchronized with `core-lite` and usage docs updated (`README.md`, `docs/ClientUsage.md`, `docs/Architecture.md`, `docs/ShortDescriptio.txt`).
   - References:
     - `docs/Random.h`
     - `docs/ClientUsage.md`

6. `[x]` For `qln-grpc`, use QLN broadcast transport for SC sends (no RPC fallback in this backend).
   - Done: `Backend::QlnGrpc` now uses `QlnContractTransport` over `BroadcastTransaction`.
   - References:
     - `src/app.rs`
     - `src/qln/mod.rs`
     - `proto/lightnode.proto`

## Change log (this repository)

1. `[x]` Added this checklist file with mandatory status tracking rules.
2. `[x]` Added a link from `AGENTS.md` to this checklist.
3. `[x]` Updated pipeline reveal-only behavior to match SC NB comment (stop window + shutdown).
4. `[x]` Added CLI validation for collateral tiers and reveal-delay stream compatibility.
5. `[x]` Synchronized local SC docs and client usage docs with current `Random.h`.
6. `[x]` Switched `qln-grpc` send path from SCAPI RPC to QLN `BroadcastTransaction`.
