// Mock data for the apps area (see ../registry.ts for the API): the 卸载
// inventory + leftover previews, the 更新 catalog, and the 启动项 lists.
// Scenes:
//   ?scene=empty     → no updates, no login items
//   ?scene=checking  → the 更新 tab paints a stale cached catalog first,
//                      then swaps in the live scan
import { delay, on, scene } from "../registry";
import type {
  AppInfo,
  AppUpdate,
  EmbeddedLoginItem,
  LoginItem,
  PlanItem,
  UninstallNote,
  UninstallPlanPayload,
  UpdateCatalog,
  UpdateResult,
} from "../../src/types";

const HOME = "/Users/mike";

/** Decimal GB → the KB unit the plan API uses (humanKb multiplies by 1024). */
const gb = (n: number) => Math.round((n * 1e9) / 1024);
/** Decimal MB → KB. */
const mb = (n: number) => Math.round((n * 1e6) / 1024);
/** Decimal KB → KB. */
const kb = (n: number) => Math.round((n * 1e3) / 1024);

/** One inventory row: [name, bundle id, version, size KB, running, protected, official uninstaller]. */
type Row = [string, string, string, number, boolean, boolean, string | null];

const ROWS: Row[] = [
  ["Xcode", "com.apple.dt.Xcode", "16.2", gb(12.1), true, false, null],
  ["Final Cut Pro", "com.apple.FinalCut", "11.0.1", gb(6.42), false, false, null],
  ["Adobe Photoshop 2025", "com.adobe.Photoshop", "26.1.0", gb(4.86), false, false, "Adobe Uninstaller"],
  ["Adobe Illustrator 2025", "com.adobe.illustrator", "29.0.1", gb(3.12), false, false, "Adobe Uninstaller"],
  ["Android Studio", "com.google.android.studio", "2024.2.1", gb(2.94), false, false, null],
  ["Microsoft Word", "com.microsoft.Word", "16.90", gb(2.31), false, false, null],
  ["Logic Pro", "com.apple.logic10", "11.1", gb(2.27), false, false, null],
  ["Docker", "com.docker.docker", "4.30.0", gb(2.1), true, false, null],
  ["Microsoft Excel", "com.microsoft.Excel", "16.90", gb(2.08), false, false, null],
  ["Google Chrome", "com.google.Chrome", "124.0.6367.207", gb(1.82), true, false, null],
  ["Microsoft PowerPoint", "com.microsoft.Powerpoint", "16.90", gb(1.84), false, false, null],
  ["Parallels Desktop", "com.parallels.desktop.console", "20.1.1", gb(1.42), false, false, null],
  ["Microsoft Teams", "com.microsoft.teams2", "24277.3502", gb(1.21), false, false, null],
  ["WeChat", "com.tencent.xinWeChat", "4.0.0", gb(1.06), true, false, null],
  ["Arc", "company.thebrowser.Browser", "1.68.1", mb(980), false, false, null],
  ["Sketch", "com.bohemiancoding.sketch3", "101.2", mb(890), false, false, null],
  ["Cursor", "com.todesktop.230313mzl4w4u92", "0.42.3", mb(702), false, false, null],
  ["Visual Studio Code", "com.microsoft.VSCode", "1.95.1", mb(618), true, false, null],
  ["zoom.us", "us.zoom.xos", "6.2.11", mb(610), false, false, null],
  ["OrbStack", "dev.kdrag0n.MacVirt", "1.8.2", mb(520), false, false, null],
  ["Postman", "com.postmanlabs.mac", "11.18.0", mb(512), false, false, null],
  ["Notion", "notion.id", "3.16.0", mb(492), false, false, null],
  ["Slack", "com.tinyspeck.slackmacgap", "4.41.97", mb(486), true, false, null],
  ["Figma", "com.figma.Desktop", "125.5.6", mb(412), false, false, null],
  ["Firefox", "org.mozilla.firefox", "132.0", mb(408), false, false, null],
  ["1Password", "com.1password.1password", "8.10.48", mb(352), true, false, null],
  ["Obsidian", "md.obsidian", "1.7.5", mb(340), false, false, null],
  ["Spotify", "com.spotify.client", "1.2.48.405", mb(336), true, false, null],
  ["Keynote", "com.apple.iWork.Keynote", "14.2", mb(310), false, true, null],
  ["Telegram", "ru.keepcoder.Telegram", "11.3", mb(214), false, false, null],
  ["Raycast", "com.raycast.macos", "1.88.4", mb(162), true, false, null],
  ["Safari", "com.apple.Safari", "18.1", mb(58), true, true, null],
  ["TablePlus", "com.tinyapp.TablePlus", "6.1.8", mb(98), false, false, null],
  ["iTerm", "com.googlecode.iterm2", "3.5.10", mb(92), false, false, null],
  ["The Unarchiver", "com.macpaw.site.theunarchiver", "4.3.8", mb(52), false, false, null],
  ["Rectangle", "com.knollsoft.Rectangle", "0.85", mb(18), true, false, null],
];

const APPS: AppInfo[] = ROWS.map(([name, bundle_id, version, size_kb, running, prot, official]) => ({
  name,
  bundle_id,
  version,
  path: `/Applications/${name}.app`,
  size_kb,
  protected: prot,
  official_uninstaller: official,
  running,
}));

on("list_apps", async () => {
  await delay(250);
  return APPS;
});

on("refresh_app_cache", async () => {
  await delay(500);
  return null;
});

/** Brand-ish tile colours for the apps that get a "real" icon in the mock;
 * the rest stay on the lettered fallback so both paths are exercised. */
const ICON_COLORS: Record<string, string> = {
  Xcode: "#3d7fd9",
  Docker: "#2496ed",
  "Google Chrome": "#e2a33a",
  Slack: "#611f69",
  Figma: "#1e1e1e",
  "Visual Studio Code": "#1f7ad6",
  Spotify: "#1db954",
  Raycast: "#e5484d",
  Notion: "#2b2b2b",
  "zoom.us": "#2d8cff",
};

on("app_icon", async (args) => {
  await delay(30);
  const path = String(args.appPath ?? "");
  const app = APPS.find((a) => a.path === path);
  const color = app ? ICON_COLORS[app.name] : undefined;
  if (!app || !color) return null;
  const letter = app.name.slice(0, 1).toUpperCase();
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" rx="7" fill="${color}"/><text x="16" y="22" font-family="Helvetica,Arial" font-size="17" font-weight="700" fill="#fff" text-anchor="middle">${letter}</text></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
});

/** Small deterministic hash so each app gets a stable leftover set. */
function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 16777619);
  return h >>> 0;
}

/** The leftover files an uninstall preview would find for one app. Docker
 * mirrors the design spread (9 items); others get a hashed mix. */
function leftovers(app: AppInfo): PlanItem[] {
  const L = `${HOME}/Library`;
  const id = app.bundle_id;
  let paths: [string, number | null][];
  if (app.name === "Docker") {
    paths = [
      [app.path, app.size_kb],
      [`${L}/Application Support/Docker Desktop`, gb(11.6)],
      [`${L}/Application Support/com.docker.docker`, mb(712)],
      [`${L}/Application Support/com.docker.helper`, mb(88)],
      [`${L}/Caches/com.docker.docker`, mb(340)],
      [`${L}/Logs/Docker Desktop`, mb(12)],
      [`${L}/Preferences/com.docker.docker.plist`, kb(41)],
      [`${L}/Preferences/com.electron.docker-frontend.plist`, kb(7)],
      [`${L}/LaunchAgents/com.docker.helper.plist`, kb(4)],
    ];
  } else {
    const h = hash(id);
    paths = [[app.path, app.size_kb]];
    paths.push([`${L}/Application Support/${app.name}`, mb(20 + (h % 900))]);
    if (h & 1) paths.push([`${L}/Caches/${id}`, mb(5 + ((h >>> 3) % 400))]);
    paths.push([`${L}/Preferences/${id}.plist`, kb(2 + ((h >>> 5) % 60))]);
    if (h & 2) paths.push([`${L}/HTTPStorages/${id}`, kb(40 + ((h >>> 7) % 900))]);
    if (h & 4) paths.push([`${L}/Saved Application State/${id}.savedState`, kb(12 + ((h >>> 9) % 200))]);
    if (h & 8) paths.push([`${L}/Logs/${app.name}`, mb(1 + ((h >>> 11) % 40))]);
    if (h & 16) paths.push([`${L}/WebKit/${id}`, mb(2 + ((h >>> 13) % 90))]);
    if (h & 32) paths.push([`${L}/Containers/${id}`, null]);
  }
  return paths.map(([path, size_kb], i) => ({
    id: `${id}#${i}`,
    path,
    size_kb,
    item_count: 1,
    scope: "user",
  }));
}

/** Build an uninstall payload the way the backend does: protected and
 * vendor-uninstaller apps contribute a note and no items. */
function uninstallPayload(appPaths: string[], planId: string): UninstallPlanPayload {
  const items: PlanItem[] = [];
  const notes: UninstallNote[] = [];
  for (const path of appPaths) {
    const app = APPS.find((a) => a.path === path);
    if (!app) {
      notes.push({ bundle_id: "", note: `unreadable app bundle: ${path}` });
      continue;
    }
    if (app.protected) {
      notes.push({ bundle_id: app.bundle_id, note: "protected system component — not uninstallable" });
      continue;
    }
    if (app.official_uninstaller) {
      notes.push({
        bundle_id: app.bundle_id,
        note: `must be removed with the official ${app.official_uninstaller.replace(/ Uninstaller$/, "")} uninstaller`,
      });
      continue;
    }
    items.push(...leftovers(app));
  }
  const total = items.reduce((sum, i) => sum + (i.size_kb ?? 0), 0);
  return {
    summary: {
      plan_id: planId,
      sections: [{ title: "Applications", items, total_kb: total }],
      total_kb: total,
      count: items.length,
    },
    notes,
  };
}

let planCounter = 0;
on("preview_uninstall", async (args) => {
  await delay(350);
  return uninstallPayload((args.appPaths as string[]) ?? [], `preview-${++planCounter}`);
});
on("plan_uninstall", async (args) => {
  await delay(300);
  return uninstallPayload((args.appPaths as string[]) ?? [], `uninstall-${++planCounter}`);
});

// ── 更新 ────────────────────────────────────────────────────────────

const UPDATES: AppUpdate[] = [
  {
    id: "sparkle:com.figma.Desktop",
    kind: "app",
    name: "Figma",
    installed: "125.5.6",
    latest: "125.6.2",
    source: "sparkle",
    action: "open_app",
    release_notes: "修复了多人协作时图层面板偶发卡顿的问题；Dev Mode 新增变量导出。",
    command_hint: null,
    ignored: false,
  },
  {
    id: "app_store:com.apple.dt.Xcode",
    kind: "app",
    name: "Xcode",
    installed: "16.2",
    latest: "16.3",
    source: "app_store",
    action: "open_app_store",
    release_notes: "包含 iOS 18.4、macOS 15.4 SDK 及 Swift 6.1。",
    command_hint: null,
    ignored: false,
  },
  {
    id: "electron:com.tinyspeck.slackmacgap",
    kind: "app",
    name: "Slack",
    installed: "4.41.97",
    latest: "4.41.105",
    source: "electron",
    action: "open_app",
    release_notes: null,
    command_hint: null,
    ignored: false,
  },
  {
    id: "website:com.parallels.desktop.console",
    kind: "app",
    name: "Parallels Desktop",
    installed: "20.1.1",
    latest: "20.1.3",
    source: "website",
    action: "open_website",
    release_notes: null,
    command_hint: null,
    ignored: false,
  },
  {
    id: "sparkle:com.googlecode.iterm2",
    kind: "app",
    name: "iTerm",
    installed: "3.5.10",
    latest: "3.5.11",
    source: "sparkle",
    action: "open_app",
    release_notes: "修复 tmux 集成模式下窗口尺寸恢复错误。",
    command_hint: null,
    ignored: false,
  },
  {
    id: "homebrew:wget",
    kind: "package",
    name: "wget",
    installed: "1.24.5",
    latest: "1.25.0",
    source: "homebrew",
    action: "terminal",
    release_notes: null,
    command_hint: "brew upgrade wget",
    ignored: false,
  },
  {
    id: "homebrew:node",
    kind: "package",
    name: "node",
    installed: "22.11.0",
    latest: "23.1.0",
    source: "homebrew",
    action: "terminal",
    release_notes: null,
    command_hint: "brew upgrade node",
    ignored: false,
  },
  {
    id: "homebrew:ffmpeg",
    kind: "package",
    name: "ffmpeg",
    installed: "7.0.2",
    latest: "7.1",
    source: "homebrew",
    action: "terminal",
    release_notes: null,
    command_hint: "brew upgrade ffmpeg",
    ignored: false,
  },
  {
    id: "sparkle:com.bohemiancoding.sketch3",
    kind: "app",
    name: "Sketch",
    installed: "101.2",
    latest: "102.0",
    source: "sparkle",
    action: "open_app",
    release_notes: null,
    command_hint: null,
    ignored: true,
  },
];

const UP_TO_DATE: [string, string, string][] = [
  ["Google Chrome", "124.0.6367.207", "electron"],
  ["Visual Studio Code", "1.95.1", "electron"],
  ["Docker", "4.30.0", "sparkle"],
  ["Raycast", "1.88.4", "sparkle"],
  ["Rectangle", "0.85", "sparkle"],
  ["The Unarchiver", "4.3.8", "app_store"],
  ["Keynote", "14.2", "app_store"],
  ["Logic Pro", "11.1", "app_store"],
  ["Final Cut Pro", "11.0.1", "app_store"],
  ["Telegram", "11.3", "app_store"],
  ["1Password", "8.10.48", "electron"],
  ["Notion", "3.16.0", "electron"],
  ["Obsidian", "1.7.5", "electron"],
  ["Postman", "11.18.0", "electron"],
  ["git", "2.47.0", "homebrew"],
  ["gh", "2.61.0", "homebrew"],
  ["ripgrep", "14.1.1", "homebrew"],
  ["jq", "1.7.1", "homebrew"],
  ["python@3.12", "3.12.7", "homebrew"],
  ["rustup", "1.27.1", "homebrew"],
  ["TablePlus", "6.1.8", "website"],
  ["Arc", "1.68.1", "website"],
];

const WARNINGS = [
  "app_store:WeChat:not_found",
  "electron:Cursor:missing_owner",
];

/** Ignore flags flipped through set_app_update_ignored this session. */
const ignored = new Map(UPDATES.map((u) => [u.id, u.ignored]));
/** Ids that run_app_updates already "updated" (they drop off the list). */
const done = new Set<string>();

/** Current catalog, honouring the ignore flags and finished updates. */
function catalog(checkedAt: number): UpdateCatalog {
  if (scene() === "empty") {
    return { updates: [], up_to_date: [], warnings: [], checked_at: checkedAt };
  }
  return {
    updates: UPDATES.filter((u) => !done.has(u.id)).map((u) => ({ ...u, ignored: ignored.get(u.id) ?? false })),
    up_to_date: UP_TO_DATE.map(([name, version, source]) => ({ name, version, source })),
    warnings: WARNINGS,
    checked_at: checkedAt,
  };
}

const now = () => Math.floor(Date.now() / 1000);

on("cached_app_updates", () =>
  scene() === "checking"
    ? { catalog: catalog(now() - 3600 * 5), live: false }
    : { catalog: catalog(now() - 90), live: true },
);

on("list_app_updates", async () => {
  await delay(scene() === "checking" ? 2500 : 900);
  return catalog(now());
});

on("set_app_update_ignored", (args) => {
  ignored.set(String(args.updateId), Boolean(args.ignored));
  return null;
});

on("run_app_updates", async (args) => {
  await delay(1200);
  const ids = (args.updateIds as string[]) ?? [];
  return ids.map((id): UpdateResult => {
    const update = UPDATES.find((u) => u.id === id);
    if (update?.action === "open_app_store") {
      return { id, outcome: "external", cause: "app_store", message: `${update.name}：已在 App Store 打开更新页` };
    }
    if (update?.action === "open_website") {
      return { id, outcome: "external", cause: "website", message: `${update.name}：已打开官网下载页` };
    }
    done.add(id);
    return { id, outcome: "updated", cause: "ok", message: `${update?.name ?? id}：已更新到 ${update?.latest ?? "最新版本"}` };
  });
});

// ── 启动项 ──────────────────────────────────────────────────────────

/** launchd services; one points at a binary that no longer exists. */
const SERVICES: LoginItem[] = [
  {
    label: "com.docker.helper",
    path: `${HOME}/Library/LaunchAgents/com.docker.helper.plist`,
    program: "/Applications/Docker.app/Contents/Library/LaunchServices/com.docker.helper",
    program_exists: true,
    scope: "user",
    enabled: true,
  },
  {
    label: "com.google.keystone.agent",
    path: `${HOME}/Library/LaunchAgents/com.google.keystone.agent.plist`,
    program: `${HOME}/Library/Application Support/Google/GoogleUpdater/Current/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`,
    program_exists: true,
    scope: "user",
    enabled: true,
  },
  {
    label: "com.spotify.webhelper",
    path: `${HOME}/Library/LaunchAgents/com.spotify.webhelper.plist`,
    program: `${HOME}/Applications/Spotify.app/Contents/MacOS/SpotifyWebHelper`,
    program_exists: false,
    scope: "user",
    enabled: true,
  },
  {
    label: "homebrew.mxcl.postgresql@16",
    path: `${HOME}/Library/LaunchAgents/homebrew.mxcl.postgresql@16.plist`,
    program: "/opt/homebrew/opt/postgresql@16/bin/postgres",
    program_exists: true,
    scope: "user",
    enabled: false,
  },
  {
    label: "com.adobe.ccxprocess",
    path: `${HOME}/Library/LaunchAgents/com.adobe.ccxprocess.plist`,
    program: "/Applications/Utilities/Adobe Creative Cloud Experience/CCXProcess/CCXProcess.app/Contents/MacOS/Creative Cloud Content Manager.node",
    program_exists: true,
    scope: "user",
    enabled: false,
  },
  {
    label: "com.docker.vmnetd",
    path: "/Library/LaunchDaemons/com.docker.vmnetd.plist",
    program: "/Library/PrivilegedHelperTools/com.docker.vmnetd",
    program_exists: true,
    scope: "system",
    enabled: true,
  },
  {
    label: "com.parallels.desktop.launchdaemon",
    path: "/Library/LaunchDaemons/com.parallels.desktop.launchdaemon.plist",
    program: "/Applications/Parallels Desktop.app/Contents/MacOS/Parallels Service",
    program_exists: true,
    scope: "system",
    enabled: true,
  },
  {
    label: "com.microsoft.autoupdate.helper",
    path: "/Library/LaunchDaemons/com.microsoft.autoupdate.helper.plist",
    program: null,
    program_exists: false,
    scope: "system",
    enabled: true,
  },
];

const EMBEDDED: EmbeddedLoginItem[] = [
  { app_name: "Raycast", app_path: "/Applications/Raycast.app", item_name: "RaycastLauncher", kind: "login" },
  { app_name: "1Password", app_path: "/Applications/1Password.app", item_name: "1Password Launcher", kind: "login" },
  { app_name: "Rectangle", app_path: "/Applications/Rectangle.app", item_name: "RectangleLauncher", kind: "login" },
  { app_name: "Docker", app_path: "/Applications/Docker.app", item_name: "com.docker.helper", kind: "helper" },
  { app_name: "Spotify", app_path: "/Applications/Spotify.app", item_name: "SpotifyLaunchHelper", kind: "login" },
  { app_name: "Microsoft Teams", app_path: "/Applications/Microsoft Teams.app", item_name: "com.microsoft.teams2.launcher", kind: "helper" },
];

on("list_login_items", async () => {
  await delay(120);
  return scene() === "empty" ? [] : SERVICES;
});

on("list_embedded_login_items", async () => {
  await delay(120);
  return scene() === "empty" ? [] : EMBEDDED;
});

on("set_login_item_enabled", async (args) => {
  await delay(200);
  const item = SERVICES.find((s) => s.label === args.label);
  if (!item || item.scope === "system") throw new Error("system scope needs the helper");
  item.enabled = Boolean(args.enable);
  return null;
});
