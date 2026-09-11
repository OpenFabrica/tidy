# Tidy

Tidy is an open-source Tauri 2 desktop app: Rust backend + HTML/TypeScript
frontend. Product, app-shell crate, and process name are all Tidy; only the
core safety crates (`mole-core`, `mole-macos`, `mole-helper`, `mole-ops`) and
the byte-compatible log formats keep the Mole name, for compatibility with the
Mole CLI they were ported from. The core logic is a Rust port of the CLI's
safety layer — see "Desktop App Scope" in the repository's `AGENTS.md` for the
contract this workspace inherits.

Brand identity, UI tokens, component behavior, accessibility, and copy rules
are documented in [`docs/BRAND_GUIDELINES.md`](docs/BRAND_GUIDELINES.md).

## Layout

- `crates/mole-core` — safety layer: the ONLY crate allowed to delete.
  `build.rs` code-generates protection lists from
  `../lib/core/app_protection_data.sh` (fail-closed on drift).
- `crates/mole-macos` — macOS adapters (NSFileManager Trash, privileged helper
  transport) behind mockable traits.
- `crates/mole-helper` — the privileged helper: in-helper re-validation and
  mutable-ancestor refusal logic ship now; the SMAppService/XPC transport and
  signing land with release tooling (until then system-scope actions refuse
  with `requires_admin`). Its `fan-daemon` mode is live: bundled as a Tauri
  sidecar (`scripts/build-helper.sh` → `src-tauri/binaries/`), launched as
  root through the admin prompt, it drives fan RPM over a user-only Unix
  socket and hands fans back to macOS whenever the GUI is gone.
- `crates/mole-ops` — feature logic: status, analyze, clean, uninstall, purge,
  installer, optimize, touchid, history, whitelist. Destructive features build
  a `DeletionPlan` and execute only through `engine::execute` → the sink.
- `src-tauri` — thin IPC layer: two-phase PlanStore (`plan_*` → stored preview,
  `execute_plan` accepts only plan_id + selection ⊆ plan), progress channels,
  cooperative cancellation.
- `ui/` — vanilla TypeScript + Vite, no framework. One shared
  preview→confirm→execute→results flow component backs every destructive view.

Feature contracts:

- [`docs/ANALYZE.md`](docs/ANALYZE.md) — Analyze 扫描生命周期、取消语义与全进程并发预算。
- [`docs/OPTIMIZE_TASKS.md`](docs/OPTIMIZE_TASKS.md) — optimization domain,
  Mole.app parity inventory, conditions, and execution boundaries.
- [`docs/OPTIMIZE_HELP.md`](docs/OPTIMIZE_HELP.md) — user-facing explanation of
  what each optimization changes and why a task may be skipped.

## Develop

Requires Rust (stable) and Node ≥ 20.

```bash
make desktop-dev      # from the repo root: npm install + tauri dev
make desktop-check    # fmt + clippy + cargo test + tsc --noEmit
make desktop-build    # unsigned app build (no bundle)
make desktop-bundle   # Tidy.app + DMG, icons from src-tauri/icons/icon.icns
```

### Packaging assets

Both are generated files — edit the source, re-run the script, commit the output.

| Asset | Source | Regenerate with |
| --- | --- | --- |
| App icon (`src-tauri/icons/icon.icns` + PNGs) | `src-tauri/icons/icon.png` (1024²) | `src-tauri/icons/generate-app-icon.sh` |
| DMG window background (`src-tauri/dmg/background.tiff`) | `src-tauri/dmg/background.svg` | `src-tauri/dmg/generate-background.sh` |

The background is a multi-resolution TIFF (1x + 2x); a plain PNG would be
scaled up on Retina and read as blurry. Its layout is pinned to
`bundle.macOS.dmg` in `tauri.conf.json` — change the icon positions or window
size there and the artwork has to move with them.

Or directly:

```bash
cd desktop && cargo test --workspace
cd desktop/ui && npm install && npx tauri dev
```

## Safety gates

- `clippy.toml` bans `std::fs::remove_*` outside `mole-core::sink` — the Rust
  equivalent of the shell `# SAFE:` whitelist.
- `mole-core` tests run the shared adversarial corpus
  (`../tests/fuzz_corpus/dangerous_paths.txt`): every entry must be rejected.
- Log writers are byte-compatible with the CLI (`operations.log`,
  `deletions.log`), pinned by golden tests, so `mo history` reads both surfaces.
