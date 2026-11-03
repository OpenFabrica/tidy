// Mock data for the settings area (see ../registry.ts for the API): the
// protection list, project scan paths, launch-at-login, Touch ID, the
// update-autocheck switch, and the menu-bar (resident) prefs the 菜单栏 tab
// and its configure dialog read. Scenes:
//   ?scene=nofda       → Full Disk Access not granted (shows 去授权)
//   ?scene=residentfail → menu-bar prefs cannot be read (reload link)
import { delay, on, scene } from "../registry";

// Overrides the shell's always-granted fda_status only for ?scene=nofda.
on("fda_status", () => scene() !== "nofda");
on("open_fda_settings", () => null);
on("fda_helper_show", () => null);
on("fda_helper_hide", () => null);
on("fda_drag_source", () => ({
  app_path: "/Applications/Tidy.app",
  icon_path: "/Applications/Tidy.app/Contents/Resources/icon.icns",
}));

let patterns = [
  "~/Projects/tidy/target",
  "~/Library/Caches/com.figma.Desktop",
  "~/Library/Application Support/OrbStack/*",
  "~/.cargo/registry",
];
on("whitelist_get", () => ({ patterns, warnings: [] }));
on("whitelist_set", async (args) => {
  await delay(200);
  patterns = (args.patterns as string[]) ?? [];
  // The backend warns about patterns that match nothing protectable.
  return patterns.filter((p) => !p.startsWith("~") && !p.startsWith("/")).map(
    (p) => `「${p}」不是绝对路径或 ~ 开头的路径，已忽略`,
  );
});

on("purge_paths_get", () => ["~/Projects", "~/Code", "~/work/clients"]);

let autostart = true;
on("autostart_get", () => autostart);
on("autostart_set", (args) => {
  autostart = Boolean(args.enable);
  return null;
});

on("touchid_status", () => ({ enabled: true, source: "/etc/pam.d/sudo_local" }));
on("set_update_autocheck", () => null);

/** Default menu-bar prefs: CPU, memory and network shown, the rest listed off. */
const metric = (id: string, enabled: boolean) => ({ id, enabled, rules: [] });
let resident = {
  enabled: true,
  show_icon: true,
  compact: false,
  usage_colors: true,
  metrics: [
    metric("cpu", true),
    metric("memory", true),
    metric("network", true),
    metric("gpu", false),
    metric("disk", false),
    metric("cpu_temperature", false),
    metric("gpu_temperature", false),
    metric("disk_temperature", false),
    metric("battery_temperature", false),
    metric("charging_power", false),
    metric("battery_power", false),
    metric("system_power", false),
    metric("fan_speed", false),
  ],
  fan_display: "highest",
  fan_ids: [],
  network_interface: null,
};

on("resident_get_prefs", () => {
  if (scene() === "residentfail") throw { code: "io", message: "resident.json unreadable" };
  return structuredClone(resident);
});
on("resident_set_prefs", async (args) => {
  await delay(120);
  resident = structuredClone(args.prefs as typeof resident);
  return structuredClone(resident);
});
on("resident_open_panel", () => null);
on("resident_list_interfaces", async () => {
  await delay(200);
  return [
    { name: "en0", display: "Wi-Fi", active: true },
    { name: "en5", display: "USB 10/100/1000 LAN", active: false },
    { name: "utun4", display: "Tailscale", active: true },
  ];
});
// Only the fields the configure dialog reads (fans, battery presence).
on("resident_get_reading", () => ({
  power: { battery_percent: 87 },
  fans: [
    { id: 0, rpm: 2400 },
    { id: 1, rpm: 2380 },
  ],
}));
