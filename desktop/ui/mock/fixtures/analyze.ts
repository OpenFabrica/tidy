// Mock data for the analyze area (see ../registry.ts for the API).
// A realistic home directory tree for /Users/mike plus the whole-disk root.
// Sizes are written in decimal GB (what humanKb prints) and converted to KB.
// Scenes:
//   default          → scans finish after ~1.6s with progress on the channel
//   ?scene=truncated → every listing comes back truncated (cancelled badge)
import type { DirEntryInfo, DirListing, PlanSummary } from "../../src/types";
import { delay, emit, on, scene } from "../registry";

const HOME = "/Users/mike";
/** Decimal GB → the KB unit the IPC uses (humanKb multiplies by 1024). */
const kb = (gb: number) => Math.round((gb * 1e9) / 1024);

/** Child rows per directory: [name, GB, isDir?]. Unlisted tails are padded
 * with small generated entries so the "其余 n 项" count looks real. */
type Row = [string, number, boolean?];
const TREE: Record<string, { rows: Row[]; tail?: { n: number; gb: number } }> = {
  "/": {
    rows: [
      ["Users", 452.6],
      ["Applications", 64.3],
      ["Library", 22.4],
      ["System", 18.9],
      ["private", 12.7],
      ["opt", 9.2],
      ["usr", 1.4],
      ["cores", 0.0, true],
      ["Volumes", 0.0, true],
    ],
  },
  "/Users": {
    rows: [
      ["mike", 412.3],
      ["Shared", 0.84],
    ],
  },
  [HOME]: {
    rows: [
      ["Library", 186.2],
      ["Projects", 98.4],
      ["Movies", 52.1],
      ["Downloads", 28.6],
      ["Documents", 14.8],
      [".docker", 10.1],
      ["Pictures", 6.2],
      [".npm", 4.8],
      [".cache", 3.9],
      ["Desktop", 2.1],
    ],
    tail: { n: 28, gb: 5.1 },
  },
  [`${HOME}/Library`]: {
    rows: [
      ["Containers", 88.1],
      ["Application Support", 42.6],
      ["Caches", 31.2],
      ["Developer", 14.9],
      ["Group Containers", 3.8],
      ["Mail", 2.1],
      ["Messages", 1.6],
      ["Logs", 0.74],
    ],
    tail: { n: 54, gb: 1.16 },
  },
  [`${HOME}/Library/Caches`]: {
    rows: [
      ["com.apple.Safari", 6.8],
      ["Google", 5.9],
      ["com.spotify.client", 4.2],
      ["Homebrew", 3.7],
      ["com.microsoft.VSCode.ShipIt", 2.4],
      ["pip", 1.9],
      ["com.tinyspeck.slackmacgap.ShipIt", 1.3],
      ["JetBrains", 1.1],
    ],
    tail: { n: 61, gb: 3.9 },
  },
  [`${HOME}/Library/Containers`]: {
    rows: [
      ["com.docker.docker", 52.3],
      ["com.apple.mail", 9.8],
      ["com.apple.photoanalysisd", 6.4],
      ["com.tencent.xinWeChat", 5.9],
      ["com.apple.Safari", 3.1],
    ],
    tail: { n: 212, gb: 10.6 },
  },
  [`${HOME}/Projects`]: {
    rows: [
      ["tidy", 21.4],
      ["ml-experiments", 19.7],
      ["website", 12.8],
      ["ios-reader", 11.2],
      ["rust-playground", 9.6],
      ["archive-2023", 8.3],
      ["dotfiles", 0.02],
    ],
    tail: { n: 17, gb: 15.38 },
  },
  [`${HOME}/Movies`]: {
    rows: [
      ["Final Cut Projects", 31.4],
      ["Screen Recordings", 12.6],
      ["family-2024.mov", 6.2, false],
      ["demo-take3.mp4", 1.9, false],
    ],
  },
  [`${HOME}/Downloads`]: {
    rows: [
      ["Xcode_16.1.xip", 11.8, false],
      ["ubuntu-24.04-desktop-arm64.iso", 3.1, false],
      ["Figma.dmg", 0.42, false],
      ["datasets", 9.6],
      ["receipts", 0.08],
    ],
    tail: { n: 143, gb: 3.6 },
  },
};

/** Plausible small folders and files used to pad a directory's long tail. */
const TAIL_NAMES = [
  "Preferences", "Saved Application State", "WebKit", "HTTPStorages", "Cookies",
  "Keychains", "Fonts", "LaunchAgents", "Calendars", "Safari", "Music", "Public",
  ".vscode", ".zsh_history", ".gitconfig", "Sites", "Scripts", "Notes.txt",
  "Accounts", "Shortcuts", "Spelling", "Sounds", "Autosave Information", "Passes",
];

/** Build a listing for a root: known rows, a generated tail, sorted by size. */
function listing(root: string): DirListing {
  const known = TREE[root];
  const prefix = root === "/" ? "" : root;
  const entries: DirEntryInfo[] = [];
  if (known) {
    for (const [name, gb, isDir] of known.rows) {
      entries.push({
        name,
        path: `${prefix}/${name}`,
        size_kb: kb(gb),
        is_dir: isDir ?? true,
        item_count: Math.max(1, Math.round(gb * 1800)),
      });
    }
    if (known.tail) {
      // Spread the tail over n entries with a gently falling size.
      const { n, gb } = known.tail;
      const weights = Array.from({ length: n }, (_, i) => 1 / (i + 2));
      const sum = weights.reduce((a, b) => a + b, 0);
      weights.forEach((w, i) => {
        const name = TAIL_NAMES[i % TAIL_NAMES.length] + (i >= TAIL_NAMES.length ? ` ${Math.floor(i / TAIL_NAMES.length) + 1}` : "");
        entries.push({
          name,
          path: `${prefix}/${name}`,
          size_kb: kb((gb * w) / sum),
          is_dir: !/\.(txt|gitconfig|zsh_history)/.test(name),
          item_count: 12 + i,
        });
      });
    }
  } else {
    // Any folder we have no story for: a plausible handful of children.
    const names = ["build", "node_modules", "assets", "cache", "src", "docs", "README.md"];
    names.forEach((name, i) => {
      entries.push({
        name,
        path: `${prefix}/${name}`,
        size_kb: kb(2.4 / (i + 1)),
        is_dir: !name.includes("."),
        item_count: 40 * (7 - i),
      });
    });
  }
  entries.sort((a, b) => b.size_kb - a.size_kb);
  return {
    root,
    entries,
    total_kb: entries.reduce((s, e) => s + e.size_kb, 0),
    truncated: scene() === "truncated",
  };
}

on("analyze_scan", async (args) => {
  const root = String(args.root ?? HOME);
  const result = listing(root);
  // Walk progress: the directory being read plus a growing visit count.
  const steps = 8;
  for (let i = 1; i <= steps; i++) {
    const child = result.entries[(i - 1) % Math.max(1, result.entries.length)];
    emit(args.onProgress, {
      label: child ? `${child.path}/${["Caches", "Data", "objects", "blobs"][i % 4]}` : root,
      count: i * 1_437,
    });
    await delay(200);
  }
  return result;
});

// Analyze only reads `disks` from the snapshot (front-page ledger and the
// disk strip). The status fixture registers the full snapshot later and wins;
// this is the fallback so analyze never depends on it.
on("status_snapshot", () => ({
  disks: [
    {
      name: "Macintosh HD",
      mount_point: "/",
      total_bytes: 994_662_584_320,
      available_bytes: 282_620_000_000,
    },
  ],
}));

on("plan_delete_paths", async (args) => {
  const paths = (args.paths as string[]) ?? [];
  await delay(500);
  // Find each picked path's size in whatever listing holds it.
  const sizeOf = (path: string) => {
    const parent = path.slice(0, path.lastIndexOf("/")) || "/";
    return listing(parent).entries.find((e) => e.path === path)?.size_kb ?? kb(1);
  };
  const items = paths.map((path, i) => ({
    id: `analyze-${i}`,
    path,
    size_kb: sizeOf(path),
    item_count: 120 + i * 37,
    scope: "user" as const,
  }));
  const total = items.reduce((s, it) => s + it.size_kb, 0);
  const plan: PlanSummary = {
    plan_id: `plan-analyze-${Date.now()}`,
    sections: [{ title: "analyze", items, total_kb: total }],
    total_kb: total,
    count: items.length,
  };
  return plan;
});
