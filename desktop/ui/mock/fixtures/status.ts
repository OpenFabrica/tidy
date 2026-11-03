// Mock data for the status area (see ../registry.ts for the API): a live-ish
// system snapshot that drifts a little on every 2s poll, a process detail
// sheet, and the root fan daemon. Scenes:
//   ?scene=fanless   → no fans, no battery (Mac mini style), no fan panel
//   ?scene=hot       → mds_stores over 100% CPU and memory pressure high
//   ?scene=fanmanual → fan daemon already running in manual mode at 3200 RPM
import { delay, on, scene } from "../registry";

const GB = 1024 ** 3;
const MB = 1024 ** 2;

/** Process table seed: [pid, name, cpu%, memory bytes, cpu time ms, app path]. */
const PROCS: [number, string, number, number, number, string | null][] = [
  [412, "WindowServer", 18.4, 512 * MB, 8_640_000, null],
  [388, "mds_stores", 64.2, 286.4 * MB, 3_960_000, null],
  [4512, "OrbStack Helper", 34.2, 4.39 * GB, 22_320_000, "/Applications/OrbStack.app"],
  [9821, "Google Chrome Helper", 12.7, 1.82 * GB, 10_800_000, "/Applications/Google Chrome.app"],
  [7310, "Tidy", 6.1, 192 * MB, 720_000, "/Applications/Tidy.app"],
  [5120, "Slack Helper", 4.8, 684.2 * MB, 2_880_000, "/Applications/Slack.app"],
  [6044, "Code Helper (Renderer)", 3.9, 912.5 * MB, 5_760_000, "/Applications/Visual Studio Code.app"],
  [0, "kernel_task", 3.2, 3.12 * GB, 0, null],
  [611, "Finder", 1.1, 148.3 * MB, 240_000, "/System/Library/CoreServices/Finder.app"],
  [8433, "Spotify Helper", 0.6, 221 * MB, 120_000, "/Applications/Spotify.app"],
  [1, "launchd", 0.2, 24.6 * MB, 0, null],
  [402, "coreaudiod", 0.1, 18.9 * MB, 60_000, null],
  [702, "Dock", 0.4, 96.2 * MB, 180_000, null],
  [733, "ControlCenter", 0.3, 88.7 * MB, 150_000, null],
  [881, "OrbStack", 1.4, 312.6 * MB, 1_200_000, "/Applications/OrbStack.app"],
  [9810, "Google Chrome", 2.6, 642.1 * MB, 4_200_000, "/Applications/Google Chrome.app"],
  [6031, "Code", 1.8, 486.3 * MB, 3_100_000, "/Applications/Visual Studio Code.app"],
  [5101, "Slack", 0.9, 402.8 * MB, 1_700_000, "/Applications/Slack.app"],
  [8420, "Spotify", 0.7, 356.4 * MB, 900_000, "/Applications/Spotify.app"],
  [1203, "cloudd", 0.2, 54.1 * MB, 90_000, null],
  [1310, "bird", 0.1, 41.7 * MB, 45_000, null],
  [522, "mDNSResponder", 0.2, 12.4 * MB, 70_000, null],
  [1488, "photoanalysisd", 2.1, 238.9 * MB, 1_400_000, null],
  [3021, "Terminal", 0.5, 132.6 * MB, 300_000, "/System/Applications/Utilities/Terminal.app"],
  [3090, "zsh", 0, 4.2 * MB, 2_000, null],
  [3341, "node", 7.4, 418.2 * MB, 2_400_000, null],
  [3342, "esbuild", 1.2, 64.8 * MB, 110_000, null],
  [1609, "softwareupdated", 0, 38.5 * MB, 20_000, null],
  [1777, "Notes", 0.1, 164.3 * MB, 80_000, "/System/Applications/Notes.app"],
  [1801, "Safari", 0.8, 284.7 * MB, 640_000, "/Applications/Safari.app"],
  [1802, "com.apple.WebKit.WebContent", 1.9, 368.5 * MB, 910_000, null],
  [455, "configd", 0.1, 9.8 * MB, 30_000, null],
  [470, "powerd", 0, 6.3 * MB, 12_000, null],
  [1990, "Preview", 0, 92.1 * MB, 25_000, "/System/Applications/Preview.app"],
  [2050, "Docker Desktop", 0.3, 210.4 * MB, 160_000, "/Applications/Docker.app"],
  [2210, "Raycast", 0.6, 176.8 * MB, 340_000, "/Applications/Raycast.app"],
];

let tick = 0;
let rxBase = 1_240_000;

/** Small deterministic wobble so successive polls visibly change. */
function wobble(seed: number, amp: number): number {
  return Math.sin(tick * 0.9 + seed * 1.7) * amp;
}

on("status_snapshot", async () => {
  tick += 1;
  await delay(tick === 1 ? 300 : 40);
  const fanless = scene() === "fanless";
  const hot = scene() === "hot";
  const cpu = Math.max(4, 23 + wobble(1, 6) + (hot ? 40 : 0));
  rxBase = Math.max(80_000, rxBase + wobble(2, 260_000));
  const procs = PROCS.map(([pid, name, c, mem, ms, app], i) => ({
    pid,
    name,
    cpu_percent: Math.max(
      0,
      (name === "mds_stores" && hot ? 104.2 : c) + (c > 0.5 ? wobble(i, c * 0.15) : 0),
    ),
    memory_bytes: Math.round(mem + wobble(i + 3, mem * 0.01)),
    cpu_time_ms: ms + tick * 1_000,
    app_path: app,
  }));
  return {
    host: "Mikes-MacBook-Pro",
    platform: "macOS 15.1 (24B83)",
    hardware: { chip: "Apple M3 Pro", memory_gb: 36, os_version: "15.1" },
    uptime_seconds: 4 * 86400 + 12 * 3600 + tick * 2,
    cpu_usage_percent: cpu,
    cpu_count: 12,
    per_core_percent: [62, 48, 35, 30, 22, 18, 14, 12, 9, 8, 6, 4].map((v, i) =>
      Math.max(1, Math.min(100, v + wobble(i, 8) + (hot ? 30 : 0))),
    ),
    load_avg_1m: hot ? 9.8 : 3.4 + wobble(4, 0.4),
    memory_total_bytes: 36 * GB,
    memory_used_bytes: Math.round((hot ? 0.93 : 0.68) * 36 * GB + wobble(5, 0.2 * GB)),
    swap_used_bytes: 1.2 * GB,
    memory_pressure_percent: hot ? 58 : 12,
    gpu: { utilization_percent: Math.round(Math.max(2, 14 + wobble(6, 5))), core_count: 18 },
    disks: [
      {
        name: "Macintosh HD",
        mount_point: "/",
        total_bytes: 994.66e9,
        available_bytes: 282.4e9 - tick * 1e6,
      },
    ],
    battery: fanless ? null : { percent: 87, charging: true, cycle_count: 312, health_percent: 94, watts: 18.4 },
    network: {
      rx_bytes: 18.2e9 + tick * rxBase * 2,
      tx_bytes: 2.4e9 + tick * 98_000,
      rx_rate_bps: rxBase,
      tx_rate_bps: Math.max(4_000, 48_000 + wobble(7, 20_000)),
      interface: "en0",
    },
    fans: fanless
      ? []
      : [
          { actual_rpm: 2400 + wobble(8, 60), min_rpm: 1200, max_rpm: 5800, target_rpm: 2400 },
          { actual_rpm: 2380 + wobble(9, 60), min_rpm: 1200, max_rpm: 5800, target_rpm: 2400 },
        ],
    temperatures: { cpu_c: 52 + wobble(10, 2), gpu_c: 48 + wobble(11, 2) },
    top_processes: procs,
  };
});


on("process_detail", ({ pid }) => {
  const p = PROCS.find(([id]) => id === Number(pid)) ?? PROCS[2];
  const [id, name, cpu, mem, , app] = p;
  const appName = app ? app.split("/").pop()!.replace(".app", "") : null;
  return {
    pid: id,
    name,
    app_path: app,
    cpu_percent: cpu,
    memory_bytes: mem,
    user: id < 500 ? "root" : "mike",
    parent_chain: appName && appName !== name ? [[1, "launchd"], [881, appName]] : [[1, "launchd"]],
    threads: 42,
    open_files: 318,
    listen_ports: name === "OrbStack Helper" ? ["*:8080", "*:5432"] : [],
    disk_read_bytes: 1.2 * GB,
    disk_written_bytes: 340 * MB,
    children: name === "OrbStack Helper" ? 6 : 0,
    run_time_seconds: 4 * 86400 + 11 * 3600,
    cwd: "/",
    exe: app
      ? `${app}/Contents/MacOS/${name}`
      : `/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/${name}`,
    cmd: app
      ? [`${app}/Contents/MacOS/${name}`, "--type=renderer", "--lang=zh-CN", "--enable-features=SharedArrayBuffer"]
      : [name],
  };
});

on("signal_process", () => null);

// Root fan daemon. ?scene=fanless reports zero fans so the panel hides.
const manual = scene() === "fanmanual";
const fan = {
  running: manual,
  forced: manual,
  target_rpm: (manual ? 3200 : null) as number | null,
  min_rpm: 1200,
  max_rpm: 5800,
  fan_count: 2,
};
/** Current daemon state (copy, as the backend would serialize it). */
const fanState = () => ({ ...fan, fan_count: scene() === "fanless" ? 0 : fan.fan_count });

on("fan_control_status", () => fanState());
on("fan_control_start", async () => {
  await delay(400);
  fan.running = true;
  return fanState();
});
on("fan_control_set", async ({ rpm }) => {
  await delay(150);
  fan.forced = true;
  fan.target_rpm = Number(rpm);
  return fanState();
});
on("fan_control_auto", async () => {
  await delay(150);
  fan.forced = false;
  return fanState();
});
on("fan_control_stop", async () => {
  await delay(200);
  Object.assign(fan, { running: false, forced: false, target_rpm: null });
  return fanState();
});
