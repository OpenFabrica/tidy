// Mock data for the clean area (see ../registry.ts for the API).
// Every plan_* command the clean view runs in parallel, plus the executors.
// Scenes:
//   ?scene=scanning  → plan_clean never finishes (progress card stays up)
//   ?scene=slowexec  → each executed item takes ~2.5s (execution card stays up)
//   ?scene=empty     → every plan comes back empty ("没有可处理的项目")
//   ?scene=trash     → Settings' delete mode = Trash (done page shows 清空废纸篓)
//   ?scene=fresh     → no reclaim history (front page shows "—")
import { delay, emit, on, scene } from "../registry";

const HOME = "/Users/mike";
const MB = 1024;
const GB = 1024 * 1024;

/** Plan item as the backend sends it. */
interface Item {
  id: string;
  path: string;
  size_kb: number | null;
  item_count: number;
  scope: "user" | "system";
}

/** Every item ever handed out, by plan id, so the executors can echo paths and sizes. */
const plans = new Map<string, Item[]>();

/** Build one item; `~` expands to the mock home. */
function item(id: string, path: string, kb: number | null, count = 1, scope: "user" | "system" = "user"): Item {
  return { id, path: path.replace(/^~/, HOME), size_kb: kb === null ? null : Math.round(kb), item_count: count, scope };
}

/** Build a PlanSummary from titled sections and remember its items. */
function summary(planId: string, sections: Array<[string, Item[]]>) {
  const empty = scene() === "empty";
  const out = sections
    .map(([title, items]) => (empty ? [title, []] : [title, items]) as [string, Item[]])
    .filter(([, items]) => items.length > 0)
    .map(([title, items]) => ({
      title,
      items,
      total_kb: items.reduce((n, i) => n + (i.size_kb ?? 0), 0),
    }));
  plans.set(planId, out.flatMap((s) => s.items));
  return {
    plan_id: planId,
    sections: out,
    total_kb: out.reduce((n, s) => n + s.total_kb, 0),
    count: out.reduce((n, s) => n + s.items.length, 0),
  };
}

// Delete mode is a localStorage pref; scenes pin it so both done pages can be shown.
if (scene() === "trash") localStorage.setItem("mole-delete-mode", "trash");
if (scene() === "permanent") localStorage.setItem("mole-delete-mode", "permanent");

// Seed a plausible reclaim history (46 past clean runs, about two weeks
// apart) so the front page and ledger side modules quote real-looking
// figures. Only when the ledger is empty, so real runs in the mock stick.
(function seedLedger() {
  const key = "tidy.ledger.reclaim.v1";
  if (scene() === "fresh") {
    localStorage.removeItem(key);
    return;
  }
  if (localStorage.getItem(key)) return;
  const day = 86_400_000;
  const rows = [];
  const sizesGb = [5.17, 11.03, 6.88, 4.2, 9.61, 7.35, 3.92, 8.8, 6.1, 12.4];
  for (let i = 0; i < 45; i++) {
    rows.push({ kind: "clean", at: Date.now() - (14 + (45 - i) * 14) * day, freedKb: sizesGb[i % sizesGb.length] * GB * 0.82 });
  }
  rows.push({ kind: "clean", at: Date.now() - 14 * day, freedKb: 8.42 * GB });
  localStorage.setItem(key, JSON.stringify(rows));
})();

/** Progress labels plan_clean reports while it walks each category. */
const CLEAN_STEPS: Array<[string, number]> = [
  ["App 缓存", 132],
  ["日志", 412],
  ["开发工具", 58],
  ["AI 工具", 9],
  ["浏览器", 3],
  ["设计工具", 4],
  ["通讯工具", 12],
  ["临时文件", 81],
  ["包管理器缓存", 6],
];

on("plan_clean", async (args) => {
  const forever = scene() === "scanning";
  for (const [label, count] of CLEAN_STEPS) {
    emit(args.onProgress, { label, count });
    await delay(forever ? 4_000 : 260);
  }
  if (forever) await delay(3_600_000);
  return {
    summary: summary("plan-clean", [
      [
        "app_cache",
        [
          item("ac1", "~/Library/Caches/com.spotify.client", 1.02 * GB, 214),
          item("ac2", "~/Library/Caches/notion.id", 412 * MB, 96),
          item("ac3", "~/Library/Caches/us.zoom.xos", 290 * MB, 58),
          item("ac4", "~/Library/Application Support/Steam/appcache", 350 * MB, 1_208),
          item("ac5", "~/Library/Caches/com.raycast.macos", 186 * MB, 41),
          item("ac6", "~/Library/Caches/com.tinyspeck.slackmacgap", 238 * MB, 77),
          item("ac7", "~/Library/Caches/com.microsoft.VSCode.ShipIt", 164 * MB, 3),
          item("ac8", "~/Library/Caches/com.apple.Music", 96 * MB, 18),
        ],
      ],
      [
        "logs",
        [
          item("lg1", "~/Library/Logs/DiagnosticReports", 412 * MB, 318),
          item("lg2", "~/Library/Logs/JetBrains", 386 * MB, 52),
          item("lg3", "~/Library/Logs/Homebrew", 148 * MB, 27),
          item("lg4", "~/Library/Logs/CoreSimulator", 172 * MB, 15),
        ],
      ],
      [
        "dev",
        [
          item("dv1", "~/Library/Developer/Xcode/DerivedData", 4.18 * GB, 23_402),
          item("dv2", "~/.npm/_cacache", 1.31 * GB, 18_877),
          item("dv3", "~/Library/Caches/com.apple.dt.Xcode", 640 * MB, 1_150),
          item("dv4", "~/Library/Caches/Yarn/v6", 296 * MB, 4_031),
        ],
      ],
      [
        "ai",
        [
          item("ai1", "~/Library/Caches/com.anthropic.claudefordesktop", 312 * MB, 64),
          item("ai2", "~/Library/Application Support/Cursor/Cache", 418 * MB, 911),
          item("ai3", "~/Library/Caches/ai.perplexity.mac", 130 * MB, 22),
        ],
      ],
      [
        "browser",
        [
          item("br1", "~/Library/Caches/Google/Chrome/Default/Cache", 1.46 * GB, 3_812),
          item("br2", "~/Library/Caches/com.apple.Safari", 520 * MB, 940),
          item("br3", "~/Library/Caches/Firefox/Profiles/q3k8s1lv.default-release/cache2", 330 * MB, 2_116),
        ],
      ],
      [
        "design",
        [
          item("ds1", "~/Library/Application Support/Figma/DesktopProfile/v31/Cache", 520 * MB, 1_402),
          item("ds2", "~/Library/Caches/com.bohemiancoding.sketch3", 220 * MB, 88),
        ],
      ],
      [
        "im",
        [
          item("im1", "~/Library/Containers/com.tencent.xinWeChat/Data/Library/Caches", 860 * MB, 5_220),
          item("im2", "~/Library/Application Support/Telegram Desktop/tdata/user_data/cache", 340 * MB, 1_904),
          item("im3", "~/Library/Application Support/discord/Cache", 182 * MB, 640),
        ],
      ],
      [
        "system",
        [
          item("sy1", "/Library/Caches/com.apple.iconservices.store", 6 * MB, 1, "system"),
          item("sy2", "/private/var/log/asl", 214 * MB, 380, "system"),
        ],
      ],
      [
        "temp",
        [
          item("tp1", "/private/var/folders/7k/2w9_xq5d4bn0h8c1r6t3m0000gn/T/com.apple.Safari", 268 * MB, 412),
          item("tp2", "/private/var/folders/7k/2w9_xq5d4bn0h8c1r6t3m0000gn/T/node-compile-cache", 212 * MB, 1_930),
        ],
      ],
      [
        "pkg_cache",
        [
          item("pk1", "~/.cargo/registry/cache", 512 * MB, 1_604),
          item("pk2", "~/.gradle/caches/modules-2", 386 * MB, 2_877),
          item("pk3", "~/Library/Caches/pip", 140 * MB, 512),
          item("pk4", "~/Library/pnpm/store/v3", 108 * MB, 9_411),
        ],
      ],
      [
        "xcode_support",
        [
          item("xs1", "~/Library/Developer/Xcode/iOS DeviceSupport/iPhone15,2 17.2 (21C62)", 3.62 * GB, 1),
          item("xs2", "~/Library/Developer/Xcode/iOS DeviceSupport/iPhone15,2 17.5.1 (21F90)", 3.71 * GB, 1),
        ],
      ],
      [
        "ios_backups",
        [item("ib1", "~/Library/Application Support/MobileSync/Backup/00008110-001A2C3E0E91801E", 21.4 * GB, 1)],
      ],
    ]),
    blocked: { owners: ["Chrome", "Slack"], count: 34, total_kb: 1.2 * GB },
    backups: [
      {
        path: `${HOME}/Library/Application Support/MobileSync/Backup/00008110-001A2C3E0E91801E`,
        device: "Mike's iPhone",
        product: "iPhone15,2",
        last_backup_days: 30,
      },
    ],
  };
});

on("plan_purge", async (args) => {
  for (const p of ["code/my-app/node_modules", "code/tidy/target", "code/old-site/.next"]) {
    emit(args.onProgress, { label: `${HOME}/${p}`, count: 1 });
    await delay(320);
  }
  return {
    summary: summary("plan-purge", [
      [
        "projects",
        [
          item("pj1", "~/code/my-app/node_modules", 820 * MB, 41_220),
          item("pj2", "~/code/tidy/target", 1.94 * GB, 12_904),
          item("pj3", "~/code/old-site/.next", 380 * MB, 2_301),
        ],
      ],
    ]),
    projects: [
      { root: `${HOME}/code/my-app`, blockers: ["uncommitted changes"], idle_days: 3 },
      { root: `${HOME}/code/tidy`, blockers: [], idle_days: 1 },
      { root: `${HOME}/code/old-site`, blockers: [], idle_days: 92 },
    ],
  };
});

on("plan_installer", async () => {
  await delay(400);
  return summary("plan-installer", [
    [
      "Installer artifacts",
      [
        item("in1", "~/Downloads/Docker.dmg", 612 * MB),
        item("in2", "~/Downloads/Figma-124.5.dmg", 284 * MB),
        item("in3", "~/Downloads/node-v22.11.0.pkg", 96 * MB),
      ],
    ],
  ]);
});

on("plan_docker", async () => {
  await delay(500);
  const images = [
    { id: "sha256:9b1e", repository: "postgres", tag: "16", size_kb: 432 * MB, containers: 2, idle_days: 0, dangling: false },
    { id: "sha256:4c7a", repository: "<none>", tag: "<none>", size_kb: 270 * MB, containers: 0, idle_days: 61, dangling: true },
    { id: "sha256:e21f", repository: "node", tag: "18-alpine", size_kb: 258 * MB, containers: 0, idle_days: 120, dangling: false },
  ];
  return {
    summary: summary("plan-docker", [
      [
        "docker",
        [
          ...images.map((img) => item(img.id, `${img.repository}:${img.tag}`, img.size_kb)),
          item("build-cache", "Docker build cache", 1.1 * GB, 214),
        ],
      ],
    ]),
    images,
  };
});

on("plan_simulators", async () => {
  await delay(450);
  const devices = [
    { udid: "6F1C2A90-0B4E-4D7A-9E1B-3C5D7F2A1B08", name: "iPhone 15 Pro", runtime: "iOS 17.2", state: "Shutdown", is_available: true, size_kb: 1.12 * GB, age_days: 45 },
    { udid: "A3D94E12-77C0-4B5F-8E2A-61F0C9B3D4E7", name: "iPad Air (5th generation)", runtime: "iOS 16.4", state: "Shutdown", is_available: false, size_kb: 680 * MB, age_days: 210 },
  ];
  return {
    summary: summary("plan-sims", [
      [
        "simulators",
        devices.map((d) => item(d.udid, `~/Library/Developer/CoreSimulator/Devices/${d.udid}`, d.size_kb)),
      ],
    ]),
    devices,
  };
});

on("plan_trash", async () => {
  await delay(300);
  return summary("plan-trash", [
    [
      "trash",
      [
        item("tr1", "~/.Trash/Screen Recording 2026-10-12 at 14.03.11.mov", 820 * MB),
        item("tr2", "~/.Trash/old-exports", 270 * MB, 64),
      ],
    ],
  ]);
});

on("plan_orphans", async () => {
  await delay(380);
  const orphans = [
    { path: `${HOME}/Library/Application Support/com.evernote.Evernote`, bundle_id: "com.evernote.Evernote", idle_days: 412 },
    { path: `${HOME}/Library/Containers/com.readdle.smartemail-Mac`, bundle_id: "com.readdle.smartemail-Mac", idle_days: 188 },
    { path: `${HOME}/Library/Preferences/com.charliemonroe.Downie-4.plist`, bundle_id: "com.charliemonroe.Downie-4", idle_days: 60 },
  ];
  return {
    summary: summary("plan-orphans", [
      [
        "orphans",
        [
          item(orphans[0].path, orphans[0].path, 1.31 * GB, 2_810),
          item(orphans[1].path, orphans[1].path, 342 * MB, 902),
          item(orphans[2].path, orphans[2].path, 8),
        ],
      ],
    ]),
    orphans,
  };
});

on("plan_tools", async () => {
  await delay(520);
  const items = [
    {
      id: "snapshot:2026-11-02-213015",
      label: "com.apple.TimeMachine.2026-11-02-213015.local",
      detail: "2026-11-02 21:30",
      size_kb: null,
      requires_admin: true,
      target: { kind: "snapshot", stamp: "2026-11-02-213015" },
    },
    {
      id: "brew-cleanup",
      label: "Homebrew",
      detail: "41 个旧版本",
      size_kb: 1.34 * GB,
      requires_admin: false,
      target: { kind: "brew_cleanup" },
    },
  ];
  return {
    summary: summary("plan-tools", [
      ["tools", items.map((t) => item(t.id, t.label, t.size_kb))],
    ]),
    items,
  };
});

/** The two items that never go through, with the backend's stable error codes. */
const PROBLEMS: Record<string, { outcome: string; error: string }> = {
  sy1: { outcome: "failed", error: "io:Permission denied (os error 13)" },
  ac5: { outcome: "skipped", error: "whitelisted" },
};

/** Stream one ExecItem per selected id over the channel, then return the report. */
async function execute(args: Record<string, unknown>, trashed: boolean) {
  const items = plans.get(String(args.planId)) ?? [];
  const selection = new Set((args.selection as string[]) ?? []);
  const report = { items: [] as unknown[], total_freed_kb: 0, skipped: 0, failed: 0 };
  const step = scene() === "slowexec" ? 2_500 : 70;
  for (const it of items) {
    if (!selection.has(it.id)) continue;
    await delay(step);
    const problem = PROBLEMS[it.id];
    const outcome = problem?.outcome ?? (trashed ? "trashed" : "removed");
    const row = { id: it.id, path: it.path, outcome, size_kb: it.size_kb, error: problem?.error ?? null };
    if (outcome === "skipped") report.skipped += 1;
    else if (outcome === "failed") report.failed += 1;
    else report.total_freed_kb += it.size_kb ?? 0;
    report.items.push(row);
    emit(args.onProgress, row);
  }
  return report;
}

// The Trash plan is permanent-only on the backend; everything else honours
// the Settings delete mode passed as trashOverride.
on("execute_plan", (args) => execute(args, args.trashOverride === true && args.planId !== "plan-trash"));
on("execute_docker", (args) => execute(args, false));
on("execute_simulators", (args) => execute(args, false));
on("execute_tools", (args) => execute(args, false));
