# Tidy

English | [简体中文](README.zh-CN.md)

**An open-source macOS cleaner that shows you everything before it removes
anything.** Tidy scans caches, logs, app leftovers, and old installers, lists
every item with its size, and moves only what you select to the Trash. It is
the desktop surface of the Mole CLI, built with Tauri 2, Rust, and TypeScript.

[Website](https://tidy.talkwork.vip/en/) · [Download the latest DMG](https://github.com/OpenFabrica/tidy/releases/latest/download/Tidy.dmg) · [Releases](https://github.com/OpenFabrica/tidy/releases) · [Issues](https://github.com/OpenFabrica/tidy/issues)

![Tidy Clean view listing reclaimable space by category](site/assets/media/clean-scan@1x.png)

## Features

- **Clean** — reclaimable space grouped by origin: app caches, logs, developer
  tools, AI tools, browsers, design tools, messaging, and leftover
  `dmg` / `pkg` / `xip` installers.
- **Apps** — full uninstall with every leftover listed by category, updates
  grouped by source (Homebrew / App Store / Sparkle / Electron / Website), and
  login items.
- **Optimize** — one bounded maintenance pass: launch speed, system databases,
  network and search. Installs nothing and deletes no personal documents.
- **Analyze** — a treemap of the disk or a folder; drill down, cancel at any
  time, move a directory to Trash.
- **Status** — CPU, memory, disk, network, battery, GPU, fans, and per-process
  detail, also available from the menu bar.

## Safety model

- Every destructive feature builds a `DeletionPlan` and executes only through
  one sink in `mole-core`. Preview, confirmation, and execution use the same
  candidate set.
- User-scope deletions go to the Trash and can be restored.
- System-scope actions are refused with `requires_admin` until the privileged
  helper transport ships. There is no shell elevation fallback.
- Every run is logged to `~/Library/Logs/mole` in a format compatible with
  `mo history`.

## Install

Download [Tidy.dmg](https://github.com/OpenFabrica/tidy/releases/latest/download/Tidy.dmg).
It is a Universal build (Intel and Apple silicon), needs macOS 11 or later, and
is signed with a Developer ID and notarized by Apple. The app updates itself.

## Build from source

Requires Rust stable and Node 20+.

```bash
git clone https://github.com/OpenFabrica/tidy.git
cd tidy
make desktop-dev      # build the helper sidecar, install UI deps, start Tauri dev
make desktop-check    # fmt, Clippy, Rust tests, TypeScript check
make desktop-build    # unsigned app build, no bundle
```

## Repository layout

| Path | Contents |
| --- | --- |
| [`desktop/`](desktop/) | Tauri 2 app: Rust crates (`mole-core`, `mole-macos`, `mole-helper`, `mole-ops`) and the TypeScript UI. See [`desktop/README.md`](desktop/README.md). |
| [`site/`](site/) | Static product site and blog. See [`site/README.md`](site/README.md). |
| [`tests/fuzz_corpus/`](tests/fuzz_corpus/) | Adversarial paths that the safety tests must reject. |
| [`tools/`](tools/) | Supporting services, such as the telemetry proxy. |

## Contributing

Read [`AGENTS.md`](AGENTS.md) and [`desktop/AGENTS.md`](desktop/AGENTS.md)
first. Commits follow Conventional Commits. Run `make desktop-check` before
opening a pull request, and call out any change to IPC, permissions, logs, or
deletion policy.

When a refusal reason looks wrong, open an issue with the refusal code
(`probe_failed`, `requires_admin`, …) and the matching lines from
`~/Library/Logs/mole`.
