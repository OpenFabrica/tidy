// Settings (reference design): a centered sheet with 通用/工具/菜单栏 tabs.
// Rows are title + description left, control right. Only real capabilities
// appear — native-only features (wipe-screen guard, caffeinate, license)
// are not faked.

import {
  appMeta,
  appSettings,
  autostartGet,
  autostartSet,
  fdaStatus,
  fdaHelperShow,
  fdaHelperHide,
  openFdaSettings,
  purgePathsGet,
  residentGetPrefs,
  residentOpenPanel,
  setTelemetryEnabled,
  setUpdateAutocheck,
  touchidStatus,
  updateCheck,
  whitelistGet,
  whitelistSet,
} from "../ipc";
import { showUpdateBanner } from "../banner";
import type { AppSettings, ResidentPrefs } from "../types";
import {
  closeMenubarDialogs,
  createPrefsStore,
  openConfigureDialog,
  type PrefsStore,
} from "./menubar-settings";
import { PRODUCT_NAME } from "../brand";
import { esc } from "../format";
import { lang, langMode, setLang, t } from "../i18n";
import { listen } from "@tauri-apps/api/event";
import { dockerIdleMonths, pref, purgeIdleDays, setPref } from "../prefs";
import type { View } from "../router";

type Tab = "general" | "tools" | "menubar";
let tab: Tab = "general";

/** Menu-bar prefs for the 菜单栏 tab; null when the backend could not load them. */
let resident: PrefsStore | null = null;
/** "系统状态" could not open the panel. */
let panelFailed = false;

/** One settings row: title/desc left, control HTML right. */
function row(title: string, desc: string, control: string): string {
  return `<div class="set-row">
    <div class="set-text">
      <div class="set-title">${title}</div>
      <div class="set-desc muted">${desc}</div>
    </div>
    <div class="set-control">${control}</div>
  </div>`;
}

/** Segmented control HTML. */
function segmented(id: string, options: [string, string][], active: string): string {
  return `<div class="segmented" id="${id}">${options
    .map(
      ([value, label]) =>
        `<button data-value="${value}" class="${value === active ? "on" : ""}">${label}</button>`,
    )
    .join("")}</div>`;
}

/** Small numeric input (days / months thresholds). */
function numberInput(id: string, value: number, unit: string): string {
  return `<label class="num-field"><input type="number" id="${id}" min="1" max="3650" value="${value}" /> ${unit}</label>`;
}

/** Toggle switch HTML. */
function toggle(id: string, on: boolean): string {
  return `<button class="toggle ${on ? "on" : ""}" id="${id}"></button>`;
}

export const settings: View = {
  unmount() {
    closeMenubarDialogs();
    stopFdaPoll();
    unlistenHelper?.();
    unlistenHelper = undefined;
  },
  async mount(container) {
    // `#/settings?section=menubar` (tray menu / panel footer) lands on the
    // 菜单栏 tab with that section scrolled into view and focused.
    const deepLink = new URLSearchParams(location.hash.split("?")[1] ?? "").get("section");
    if (deepLink === "menubar") tab = "menubar";
    const [meta, fda, autostartOn, wl, purgePaths, touchid, backend, residentPrefs] =
      await Promise.all([
        appMeta(),
        fdaStatus(),
        autostartGet(),
        whitelistGet(),
        purgePathsGet(),
        touchidStatus(),
        appSettings(),
        residentGetPrefs().catch((): ResidentPrefs | null => null),
      ]);
    // Rows in two tabs show launch-at-login; both read this live value.
    autostart = autostartOn;
    panelFailed = false;
    resident = residentPrefs ? createPrefsStore(residentPrefs) : null;

    const draw = () => {
      const tabs: [Tab, string][] = [
        ["general", t("set.tab.general")],
        ["tools", t("set.tab.tools")],
        ["menubar", t("set.tab.menubar")],
      ];
      const body =
        tab === "general"
          ? generalTab(fda, autostart, backend)
          : tab === "tools"
            ? toolsTab(wl.patterns, purgePaths, touchid.enabled)
            : menubarTab();

      container.innerHTML = `
        <div class="settings-sheet">
          <div class="preview-head" style="margin-bottom:8px">
            <h1 style="font-size:22px;margin:0">${t("set.title")}</h1>
            <button class="chev" id="set-close">✕</button>
          </div>
          <div class="mast-tabs" style="margin-bottom:18px">
            ${tabs
              .map(
                ([id, label]) =>
                  `<a href="#" data-tab="${id}" class="${id === tab ? "active" : ""}">${label}</a>`,
              )
              .join("")}
          </div>
          ${body}
          <div class="muted" style="margin-top:16px;font-family:var(--font-mono);font-size:11px">
            ${PRODUCT_NAME} v${esc(meta.app_version)} · ${esc(meta.protection_data_sha256.slice(0, 12))}…
            · ${meta.helper_available ? t("set.helper.on") : t("set.helper.off")}
          </div>
        </div>`;

      container.querySelector("#set-close")!.addEventListener("click", () => {
        history.length > 1 ? history.back() : (location.hash = "#/clean");
      });
      container.querySelectorAll<HTMLAnchorElement>("a[data-tab]").forEach((a) => {
        a.addEventListener("click", (ev) => {
          ev.preventDefault();
          tab = a.dataset.tab as Tab;
          draw();
        });
      });
      wire(container, draw);
    };
    resident?.subscribe(() => {
      if (tab === "menubar" && container.isConnected) draw();
    });
    draw();
    if (deepLink === "menubar") {
      const section = container.querySelector<HTMLElement>("#set-menubar");
      section?.scrollIntoView({ block: "start" });
      section?.focus({ preventScroll: true });
      // Drop the query without a hashchange so the same deep link fires
      // again next time (an unchanged hash would not re-navigate).
      history.replaceState(null, "", "#/settings");
    }
  },
};

/** Launch-at-login state shared by the 通用 and 菜单栏 rows. */
let autostart = false;

/** 通用 tab. */
function generalTab(fda: boolean, autostart: boolean, backend: AppSettings): string {
  const mode = langMode();
  return [
    row(
      t("set.fda"),
      t("set.fda.desc"),
      fda
        ? `<span class="badge ok">${t("set.fda.granted")}</span>`
        : `<button id="fda-open">${t("set.fda.open")}</button>`,
    ),
    row(
      t("set.lang"),
      t("set.lang.desc", { cur: lang() === "zh" ? "中文" : "English" }),
      segmented("seg-lang", [["auto", t("set.lang.auto")], ["zh", "中文"], ["en", "EN"]], mode),
    ),
    row(
      t("set.temp"),
      t("set.temp.desc"),
      segmented("seg-temp", [["c", "°C"], ["f", "°F"]], pref("temp-unit", "c")),
    ),
    row(t("set.autostart"), t("set.autostart.desc"), toggle("tg-autostart", autostart)),
    row(
      t("set.selfupdate"),
      t("set.selfupdate.desc"),
      `<div class="set-inline">
         <button id="upd-check">${t("set.selfupdate.check")}</button>
         ${toggle("tg-autocheck", backend.update_autocheck)}
       </div>`,
    ),
    `<div id="upd-check-status" class="muted set-note"></div>`,
    // 构建里没编译进上报地址时，这一行整行不出现——给一个恒为"关"且点不动
    // 的开关只会让人怀疑它到底关没关。
    backend.telemetry_configured
      ? row(
          t("set.telemetry"),
          t("set.telemetry.desc"),
          toggle("tg-telemetry", backend.telemetry_enabled),
        )
      : "",
  ].join("");
}

/** 工具 tab. */
function toolsTab(patterns: string[], purgePaths: string[] | null, touchidOn: boolean): string {
  return [
    row(
      t("set.delmode"),
      t("set.delmode.desc"),
      segmented(
        "seg-delmode",
        [
          ["permanent", t("set.delmode.perm")],
          ["trash", t("set.delmode.trash")],
        ],
        pref("delete-mode", "permanent"),
      ),
    ),
    `<div class="set-row" style="flex-direction:column;align-items:stretch;gap:8px">
      <div class="set-text">
        <div class="set-title">${t("set.protect")}</div>
        <div class="set-desc muted">${t("set.protect.desc")}</div>
      </div>
      <textarea id="wl">${esc(patterns.join("\n"))}</textarea>
      <div style="display:flex;gap:8px;align-items:center">
        <button id="wl-save">${t("set.save")}</button>
        <span id="wl-status" class="muted"></span>
      </div>
      <div id="wl-warnings"></div>
    </div>`,
    row(
      t("set.purgepaths"),
      purgePaths ? purgePaths.map(esc).join("<br>") : t("set.purgedefault"),
      "",
    ),
    row(
      t("set.purgeidle"),
      t("set.purgeidle.desc"),
      numberInput("num-purgeidle", purgeIdleDays(), t("set.unit.days")),
    ),
    row(
      t("set.dockeridle"),
      t("set.dockeridle.desc"),
      numberInput("num-dockeridle", dockerIdleMonths(), t("set.unit.months")),
    ),
    row(
      t("set.touchid"),
      t("set.touchid.hint"),
      touchidOn
        ? `<span class="badge ok">${t("set.enabled")}</span>`
        : `<span class="badge">${t("set.disabled")}</span>`,
    ),
  ].join("");
}

/** 菜单栏 tab: the master switch with its panel/configure actions, then
 * launch-at-login. resident.json is the only source of truth for the switch:
 * when it cannot be read the switch stays disabled behind a reload link. */
function menubarTab(): string {
  const store = resident;
  const enabled = store?.prefs.enabled ?? false;
  const busy = store ? store.saving : true;
  const actions =
    store && enabled
      ? `<button class="link-quiet mb-action" id="mb-open-panel" ${busy ? "disabled" : ""}>${t("mb.status")}</button>
         <button class="link-quiet mb-action" id="mb-configure" aria-haspopup="dialog" ${busy ? "disabled" : ""}>${t("mb.configure")}</button>`
      : "";
  const feedback = !store || store.error
    ? `<div class="set-note mb-feedback" role="status">${t(store ? "mb.saveFailed" : "mb.loadFailed")}
         <button class="link-quiet" id="mb-reload">${t("mb.reload")}</button></div>`
    : panelFailed
      ? `<div class="set-note mb-feedback" role="alert">${t("mb.panelFailed")}</div>`
      : "";
  return `<div id="set-menubar" class="mb-section" tabindex="-1" role="region" aria-label="${esc(t("mb.section"))}">
    <div class="set-row">
      <div class="set-text">
        <div class="set-title" id="mb-enabled-label">${t("mb.enabled")}</div>
        <div class="set-desc muted" id="mb-enabled-desc">${t("mb.enabled.desc")}</div>
      </div>
      <div class="set-control set-inline">
        ${actions}
        <button class="toggle ${enabled ? "on" : ""}" id="tg-resident" role="switch" aria-checked="${enabled}"
          aria-labelledby="mb-enabled-label" aria-describedby="mb-enabled-desc" ${busy ? "disabled" : ""}></button>
      </div>
    </div>
    ${feedback}
    ${row(t("mb.autostart"), t("mb.autostart.desc"), toggle("tg-autostart-mb", autostart))}
  </div>`;
}

/** Poll timer started by the FDA button; cleared on grant and on unmount. */
let fdaPoll: number | undefined;
function stopFdaPoll(): void {
  if (fdaPoll !== undefined) window.clearInterval(fdaPoll);
  fdaPoll = undefined;
}

/** Backend "helper window closed" subscription; the page re-reads the
 * permission once so the row reflects a grant made while it was open. */
let unlistenHelper: (() => void) | undefined;

/** Wire the active tab's controls. */
function wire(container: HTMLElement, redraw: () => void): void {
  // "去授权": open the pane, float the drag helper beside it, and poll until
  // the toggle flips so the row turns into the "granted" badge by itself.
  unlistenHelper?.();
  void listen("fda-helper-closed", () => {
    stopFdaPoll();
    redraw();
  }).then((un) => {
    unlistenHelper = un;
  });
  container.querySelector("#fda-open")?.addEventListener("click", () => {
    void openFdaSettings();
    void fdaHelperShow();
    stopFdaPoll();
    fdaPoll = window.setInterval(() => {
      void fdaStatus().then((ok) => {
        if (!ok) return;
        stopFdaPoll();
        void fdaHelperHide();
        redraw();
      });
    }, 1500);
  });


  container.querySelectorAll<HTMLElement>(".segmented").forEach((seg) => {
    seg.querySelectorAll<HTMLButtonElement>("button").forEach((btn) => {
      btn.addEventListener("click", () => {
        const value = btn.dataset.value!;
        if (seg.id === "seg-lang") {
          setLang(value as "auto" | "zh" | "en");
          return; // setLang re-renders the route
        }
        if (seg.id === "seg-temp") setPref("temp-unit", value);
        if (seg.id === "seg-delmode") setPref("delete-mode", value);
        redraw();
      });
    });
  });

  const wireToggle = (id: string, onChange: (on: boolean) => void) => {
    const el = container.querySelector<HTMLButtonElement>(`#${id}`);
    el?.addEventListener("click", () => {
      const on = !el.classList.contains("on");
      el.classList.toggle("on", on);
      onChange(on);
    });
  };
  wireToggle("tg-autostart", (on) => {
    autostart = on;
    void autostartSet(on);
  });
  wireToggle("tg-autostart-mb", (on) => {
    autostart = on;
    void autostartSet(on);
  });
  wireToggle("tg-autocheck", (on) => void setUpdateAutocheck(on));
  wireToggle("tg-telemetry", (on) => void setTelemetryEnabled(on));

  // 手动检查绕开 24 小时频率限制（auto=false）：用户主动点了就该真发请求。
  container.querySelector("#upd-check")?.addEventListener("click", async () => {
    const status = container.querySelector<HTMLElement>("#upd-check-status")!;
    status.textContent = t("set.selfupdate.checking");
    try {
      const info = await updateCheck(false);
      if (info) {
        status.textContent = t("set.selfupdate.found", { version: info.version });
        showUpdateBanner(info);
      } else {
        status.textContent = t("set.selfupdate.latest");
      }
    } catch {
      status.textContent = t("set.selfupdate.failed");
    }
  });
  // Master switch: persisted through resident prefs (disabled until loaded).
  container.querySelector("#tg-resident")?.addEventListener("click", () => {
    const store = resident;
    if (!store) return;
    void store.save({ ...store.prefs, enabled: !store.prefs.enabled });
    redraw();
  });
  container.querySelector("#mb-configure")?.addEventListener("click", (ev) => {
    if (resident) openConfigureDialog(resident, ev.currentTarget as HTMLElement);
  });
  container.querySelector("#mb-open-panel")?.addEventListener("click", () => {
    panelFailed = false;
    void residentOpenPanel().catch(() => {
      panelFailed = true;
      redraw();
    });
  });
  container.querySelector("#mb-reload")?.addEventListener("click", async () => {
    const fresh = await residentGetPrefs().catch(() => null);
    if (fresh && resident) {
      resident.prefs = fresh;
      resident.error = false;
    } else if (fresh) {
      // Prefs failed at mount: build the store now so the switch comes alive.
      resident = createPrefsStore(fresh);
      resident.subscribe(() => container.isConnected && redraw());
    }
    redraw();
  });

  // Threshold inputs save on every change; invalid values fall back to the
  // default at read time (see prefs.intPref).
  const wireNumber = (id: string, key: "purge-idle-days" | "docker-idle-months") => {
    container.querySelector<HTMLInputElement>(`#${id}`)?.addEventListener("change", (ev) => {
      setPref(key, (ev.target as HTMLInputElement).value.trim());
    });
  };
  wireNumber("num-purgeidle", "purge-idle-days");
  wireNumber("num-dockeridle", "docker-idle-months");

  container.querySelector("#wl-save")?.addEventListener("click", async () => {
    const textarea = container.querySelector<HTMLTextAreaElement>("#wl")!;
    const status = container.querySelector<HTMLElement>("#wl-status")!;
    const patterns = textarea.value
      .split("\n")
      .map((l) => l.trim())
      .filter((l) => l.length > 0);
    try {
      const warnings = await whitelistSet(patterns);
      status.textContent = t("set.saved");
      container.querySelector<HTMLElement>("#wl-warnings")!.innerHTML = warnings
        .map((w) => `<div class="badge warn" style="margin-top:6px">${esc(w)}</div>`)
        .join("");
    } catch (e) {
      status.textContent = String(e);
    }
  });
}
