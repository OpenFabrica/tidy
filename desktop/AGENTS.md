# Repository Guidelines

## Project Structure & Architecture

`desktop/` is the Tauri 2 desktop surface for Tidy. Keep UI orchestration and
destructive policy separate:

- `crates/mole-core/` is the safety boundary: validation, policy, deletion
  planning, the sole deletion sink, protection-list generation, and logs.
- `crates/mole-macos/` provides mockable macOS adapters; `mole-helper/` is the
  privileged-helper boundary; `mole-ops/` implements product operations.
- `src-tauri/` is a thin IPC router. It stores plans and accepts execution only
  by `plan_id` with a selection contained by that plan.
- `ui/src/` is the framework-free TypeScript/Vite frontend; shared styles live
  in `ui/src/styles/`. Tauri configuration and capabilities live in
  `src-tauri/`.

## Build, Test, and Development Commands

Run commands from the repository root unless noted otherwise:

```bash
make desktop-dev      # build the mole-helper sidecar, install UI deps, start Tauri dev
make desktop-check    # format, Clippy, Rust tests, and TypeScript checking
make desktop-build    # compile an unsigned app without bundling
cd desktop && scripts/build-helper.sh   # sidecar needed before any raw `tauri dev|build`
cd desktop && cargo test --workspace
cd desktop && cargo clippy --workspace -- -D warnings
cd desktop/ui && npm run build
```

Rust stable and Node 20+ are required. Use `npm ci` for CI-like frontend
installs. Do not commit generated `ui/dist/` output or local dependency trees.

## Coding Style & Safety

Use Rust 2021 idioms, `cargo fmt`, and warning-free Clippy. Use four spaces in
Rust and the existing TypeScript style in nearby files; name Rust modules in
`snake_case` and TypeScript files by their view or responsibility.

All destructive behavior must construct a `DeletionPlan` and reach deletion
only through `mole_core::sink`. Do not call `std::fs::remove_*` elsewhere:
`clippy.toml` enforces this. Keep preview, confirmation, and execution on the
same candidate set. System-scope work belongs behind the helper and must
re-validate inputs; never add root-shell shortcuts.

## Testing Guidelines

Place focused Rust unit tests beside the module they protect. Cover safety
boundaries, refusal causes, and plan-to-execution invariants; `mole-core` tests
also exercise the shared dangerous-path corpus. Run the narrow crate tests
while iterating, then `make desktop-check` before opening a PR. Frontend changes
must pass `cd ui && npm run build`, which includes `tsc --noEmit`.

## Commits and Pull Requests

Follow the existing Conventional Commit style, for example
`fix(clean): bind candidates to scanned paths` or `feat(desktop): add plan UI`.
Keep commits focused. PRs should explain user-visible behavior, safety impact,
and validation performed; link the issue when available. Include screenshots
for visual changes and call out any change to IPC, permissions, logs, or
deletion policy explicitly.
