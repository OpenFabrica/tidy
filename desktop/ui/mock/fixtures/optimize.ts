// Mock data for the optimize area (see ../registry.ts for the API).
// The catalog mirrors desktop/crates/mole-ops/src/optimize/catalog.rs: same
// ids, English titles/descriptions and admin flags (the backend only speaks
// English here, so the UI shows these strings as data).
// Scenes:
//   default        → a seeded maintenance log: Flush DNS ran today, so it
//                    starts unchosen as "今天已运行"; older runs fill the log
//   ?scene=fresh   → empty maintenance log ("尚未运行过维护", log shows —)
//   ?scene=nohelper→ helper unavailable: admin tasks are hidden
import type { OptimizeTask, TaskResult } from "../../src/types";
import { delay, on, scene } from "../registry";

/** Build one catalog row; commands are only shown, never run, in the mock. */
function task(
  id: string,
  title: string,
  description: string,
  requiresAdmin = false,
  guard: string[] = [],
): OptimizeTask {
  return {
    id,
    title,
    description,
    commands: [["/usr/bin/true"]],
    requires_admin: requiresAdmin,
    guard_processes: guard,
  };
}

const CATALOG: OptimizeTask[] = [
  task("rebuildQuickLook", "Rebuild QuickLook cache", "Fixes broken file previews in Finder."),
  task("rebuildQuickLookThumbnails", "Rebuild QuickLook thumbnails", "Reloads QuickLook generators when file thumbnails are stale."),
  task("flushDNS", "Flush DNS cache", "Refreshes stale DNS answers after a network change.", true),
  task("reindexSpotlight", "Re-index Spotlight", "Rebuilds the search index only when indexing is enabled and search is slow.", true),
  task("memoryPurge", "Purge inactive memory", "Releases inactive pages only when memory pressure is elevated.", true),
  task("cleanSavedState", "Clear stale saved window state", "Moves saved window state older than 30 days to Trash."),
  task("cleanQuarantineEvents", "Clear quarantine history", "Clears macOS download history without changing file quarantine flags."),
  task("cleanCoreDuet", "Trim oversized usage database", "Compacts the on-device usage database only after it exceeds 100 MB."),
  task("preventNetworkDSStore", "Prevent .DS_Store on network and USB drives", "Stops Finder writing .DS_Store files to shares and removable drives."),
  task("auditLegacyOverrides", "Remove legacy App Nap and disk-image overrides", "Removes only hidden overrides left by old tuning tools."),
  task("rebuildFontCache", "Rebuild font cache", "Rebuilds the user font database after browsers are closed.", false, ["Safari", "Google Chrome", "Firefox"]),
  task("rebuildLaunchServices", "Rebuild Launch Services database", "Rebuilds Open With registrations and removes stale app entries."),
  task("repairDiskPermissions", "Repair disk permissions", "Resets current-user home permissions only when ownership or writability is wrong.", true),
  task("runPeriodicMaintenance", "Run periodic maintenance", "Runs daily, weekly, and monthly scripts only when the daily log is stale.", true),
  task("vacuumSQLiteDatabases", "Compact Mail and Messages databases", "Vacuums small Mail, Messages, and Safari databases only when fragmentation exceeds 5%.", false, ["Mail", "Messages", "Safari"]),
  task("repairSharedFileLists", "Clean broken shared file list entries", "Moves invalid .sfl2/.sfl3 Finder lists to Trash while keeping recent-document data."),
  task("cleanBrokenLaunchAgents", "Remove broken Launch Agents", "Moves user LaunchAgent plists to Trash only when their absolute executable is gone."),
  task("repairBrokenPlists", "Recover corrupted preferences", "Moves invalid third-party preference plists to Trash so macOS can regenerate them."),
  task("pruneNotificationDB", "Prune Notification Center database", "Deletes delivered records older than 30 days only after the database exceeds 50 MB."),
  task("pruneSpotlightOrphanRules", "Remove orphaned Spotlight rules", "Removes only valid third-party bundle IDs proven absent from Spotlight and app roots."),
  task("auditLoginItems", "Audit Login Items", "Read-only audit that reports launch items whose absolute executable no longer exists."),
  task("flushNetworkStack", "Flush network stack", "Flushes route and ARP caches only when network checks fail and no VPN or proxy is active.", true),
];

/** Seed the local maintenance ledger (ledger.ts reads it from localStorage)
 * so the task list shows a realistic log and one "ran today" task. */
(function seedLedger() {
  const KEY = "tidy.ledger.maintenance.v1";
  if (scene() === "fresh") {
    localStorage.removeItem(KEY);
    return;
  }
  const now = Date.now();
  const day = 86_400_000;
  /** One past run: task id, days ago, hours offset, outcome, duration. */
  const run = (id: string, daysAgo: number, outcome: string, ms: number, hour = 0) => ({
    id,
    title: CATALOG.find((t) => t.id === id)!.title,
    at: now - daysAgo * day - hour * 3_600_000,
    outcome,
    ms,
  });
  const rows = [
    // An older full pass, fifteen days back.
    ...CATALOG.slice(0, 14).map((t, i) => run(t.id, 15, "ok", 3_500 + i * 310, 2)),
    run("rebuildFontCache", 8, "apps_running", 1_200, 3),
    run("reindexSpotlight", 8, "requires_admin", 400, 3),
    run("pruneNotificationDB", 8, "ok", 6_100, 3),
    run("runPeriodicMaintenance", 1, "ok", 9_400, 4),
    run("rebuildQuickLook", 1, "ok", 4_200, 4),
    // Ran an hour ago → "今天已运行", unchosen by default.
    run("flushDNS", 0, "ok", 1_800, 1),
  ].sort((a, b) => a.at - b.at);
  localStorage.setItem(KEY, JSON.stringify(rows));
})();

on("list_optimize_tasks", () => CATALOG);

/** Fixed outcomes for a few tasks so a run ends in a mix of results. */
const OUTCOMES: Record<string, TaskResult["outcome"]> = {
  cleanQuarantineEvents: "unchanged",
  auditLegacyOverrides: "unchanged",
  cleanCoreDuet: "skipped",
  rebuildFontCache: "apps_running",
  vacuumSQLiteDatabases: "failed",
};

on("run_optimize", async (args) => {
  const ids = (args.taskIds as string[]) ?? [];
  await delay(450 + Math.random() * 500);
  return ids.map((id): TaskResult => {
    const outcome = OUTCOMES[id] ?? "ok";
    const output =
      outcome === "failed"
        ? "sqlite3: database is locked (Mail is still syncing)"
        : outcome === "apps_running"
          ? "Safari is running"
          : outcome === "skipped"
            ? "usage database is currently open"
            : "";
    return { id, outcome, output };
  });
});

if (scene() === "nohelper") {
  // Re-register app_meta with the helper missing (shell.ts registers first).
  on("app_meta", () => ({
    app_version: "0.1.3",
    protection_data_sha256: "3fa9c1e0b7d24a51",
    helper_available: false,
    home: "/Users/mike",
  }));
}
