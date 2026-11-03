// Mock data for the app shell: boot-time settings, app meta, telemetry,
// the shared scan meter and task cancellation. Scenes:
//   ?scene=telemetry  → the first-run telemetry notice banner
//   ?scene=update     → the "new version" banner (after the boot delay)
import { on, scene } from "../registry";

on("app_meta", () => ({
  app_version: "0.1.3",
  protection_data_sha256: "3fa9c1e0b7d24a51",
  helper_available: true,
  home: "/Users/mike",
}));

on("app_settings", () => ({
  update_autocheck: scene() === "update",
  telemetry_enabled: true,
  telemetry_configured: true,
  telemetry_notice_pending: scene() === "telemetry",
}));

on("update_check", () =>
  scene() === "update"
    ? { version: "0.1.4", current_version: "0.1.3", notes: null, mandatory: false }
    : null,
);

on("telemetry_track", () => null);
on("telemetry_notice_ack", () => null);
on("set_telemetry_enabled", () => null);
on("cancel_task", () => true);
on("fda_status", () => true);
on("reveal_in_finder", () => null);

// The scan meter only ever grows; each read advances it a little so the
// progress card looks alive.
let meterItems = 0;
let meterBytes = 0;
const meterPaths = [
  "/Users/mike/Library/Caches/com.apple.Safari/WebKitCache",
  "/Users/mike/Library/Developer/Xcode/DerivedData/Tidy-abcdefghij/Build",
  "/Users/mike/Library/Application Support/Code/CachedData",
  "/Users/mike/.npm/_cacache/content-v2/sha512/3f",
];
on("scan_meter", () => {
  meterItems += 2_317;
  meterBytes += 412_000_000;
  return {
    items: meterItems,
    bytes: meterBytes,
    path: meterPaths[Math.floor(meterItems / 9_000) % meterPaths.length],
  };
});
