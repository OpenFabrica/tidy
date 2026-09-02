# Repository Guidelines

## Where to look first

Before changing desktop code, read `desktop/AGENTS.md`; it is authoritative for
deletion safety and desktop-specific review requirements (it loads
automatically when you work under `desktop/`).

## Rules the code cannot tell you

- Keep Tauri commands thin and preserve the core-to-operations-to-IPC/UI
  dependency direction.
- `site/content/blog/` is the article source of truth; never hand-edit the
  generated `site/{zh,en}/blog/` pages. Regenerate them with the site tools
  (`cd site/tools && npm run build`) and validate with `npm run check`.
- `tests/fuzz_corpus/` feeds adversarial paths into the desktop safety tests;
  add cases there when you find a new dangerous path.

## Commits & Pull Requests

Follow Conventional Commits: `fix(clean): reject unsafe candidates` or
`feat(site): add safety article`. Keep commits scoped. PRs must summarize
user-visible behavior, safety implications, and validation run; link relevant
issues, include screenshots for UI changes, and call out IPC, permission,
log-format, or deletion-policy changes explicitly.
