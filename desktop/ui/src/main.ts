// App boot: register themed views, start the pill-nav router, and listen
// for tray-menu navigation events.

import { listen } from "@tauri-apps/api/event";
import { showTelemetryNotice, showUpdateBanner } from "./banner";
import { mountFdaHelper } from "./fda-helper";
import { appSettings, updateCheck } from "./ipc";
import { register, start } from "./router";
import { analyze } from "./views/analyze";
import { clean } from "./views/clean";
import { dashboard } from "./views/dashboard";
import { optimize } from "./views/optimize";
import { settings } from "./views/settings";
import { uninstall } from "./views/uninstall";

// The FDA drag helper is its own tiny window: no nav, no router, no prefs.
if (location.hash === "#/fda-helper") {
  mountFdaHelper();
} else {
  bootMain();
}

/** Register the themed views and start the pill-nav router. */
function bootMain(): void {
// Purge and installer are folded into the clean flow; history is log-only
// storage (no screen).
register("#/clean", "nav.clean", "clean", clean);
register("#/apps", "nav.apps", "apps", uninstall);
register("#/optimize", "nav.optimize", "optimize", optimize);
register("#/analyze", "nav.analyze", "analyze", analyze);
register("#/status", "nav.status", "status", dashboard);
// Settings is tray-only: navigable via the tray menu, no pill-nav link.
register("#/settings", "nav.settings", "", settings, true, true);

start();
void listen<string>("mole-nav", (event) => {
  location.hash = event.payload;
});
void bootBanner();
}

/** 启动后延迟拉起的检查间隔：先让窗口画完、扫描预热跑起来。 */
const UPDATE_CHECK_DELAY_MS = 5000;

/**
 * 决定报头下面那条提示显示什么。
 *
 * 首次遥测告知优先于更新提示：刚装上的用户不可能有更新可装，而"我们在采
 * 什么数据"是他第一次运行时就该看到的事。
 *
 * 自动检查关掉时这里一个网络请求都不发——包括强制更新。止血能力不该成为
 * 绕过用户明确关闭的理由，代价是那部分用户只能靠公告知道要升级。
 */
async function bootBanner(): Promise<void> {
  const settings = await appSettings().catch(() => null);
  if (!settings) return;
  if (settings.telemetry_notice_pending) {
    showTelemetryNotice();
    return;
  }
  if (!settings.update_autocheck) return;
  window.setTimeout(() => {
    void updateCheck(true)
      .then((info) => {
        if (info) showUpdateBanner(info);
      })
      .catch(() => {});
  }, UPDATE_CHECK_DELAY_MS);
}
