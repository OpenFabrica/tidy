// Tray panel entry (panel.html): the 390×610 window that drops from the
// menu-bar item. Owns the tab bar (C1), the stale/error banner (C2), the
// footer (C12), the backend event wiring and Escape-to-hide. Tab bodies are
// built once per tab switch and patched in place on every reading.

import "../styles/panel.css";
import { listen } from "@tauri-apps/api/event";
import { overviewTab } from "./cards";
import { cpuTab, diskTab, gpuTab, memoryTab, networkTab } from "./details";
import { dialogOpen } from "./dialog";
import { h, icon, show } from "./dom";
import { loadReleasePrefs, resetReleaseResult } from "./memory";
import { markSample, setSparkVisible } from "./sparkline";
import type { PanelContext, Widget } from "./widget";
import { PRODUCT_NAME } from "../brand";
import { lang, t } from "../i18n";
import {
  residentGetReading,
  residentHidePanel,
  residentOpenMain,
  residentPanelReady,
  residentQuit,
  residentRefresh,
  residentSelectTab,
  residentUpdateAvailable,
} from "../ipc";
import type { MainDestination, PanelTab, ResidentReading } from "../types";

const TABS: PanelTab[] = ["overview", "cpu", "gpu", "memory", "disk", "network"];
const TAB_KEY = "tidy-panel-tab";
/** No reading for this long while visible means updates have stalled. */
const STALL_MS = 6000;

/** Last tab the user chose (CPU on first run, like MangoDisk). */
function storedTab(): PanelTab {
  const v = localStorage.getItem(TAB_KEY) as PanelTab | null;
  return v && TABS.includes(v) ? v : "cpu";
}

const state = {
  tab: storedTab(),
  reading: null as ResidentReading | null,
  visible: true,
  error: false,
  lastReadingAt: 0,
};

let body: Widget | null = null;

const ctx: PanelContext = {
  selectTab: (tab) => selectTab(tab, false),
  openMain: (destination) => openMain(destination),
  fail: () => setError(true),
  visible: () => state.visible,
};

/** Build the body widget for a tab. */
function buildTab(tab: PanelTab): Widget {
  switch (tab) {
    case "overview":
      return overviewTab(ctx);
    case "cpu":
      return cpuTab(ctx);
    case "gpu":
      return gpuTab(ctx);
    case "memory":
      return memoryTab();
    case "disk":
      return diskTab(ctx);
    case "network":
      return networkTab(ctx);
  }
}

// ---- static shell ---------------------------------------------------------

const tabButtons = TABS.map((tab) =>
  h(
    "button",
    {
      type: "button",
      role: "tab",
      id: `tab-${tab}`,
      "aria-controls": "tabpanel",
      onclick: () => selectTab(tab, false),
      onkeydown: (ev: Event) => onTabKey(ev as KeyboardEvent, tab),
    },
    tab === "cpu" || tab === "gpu" ? tab.toUpperCase() : t(`rp.tab.${tab}`),
  ),
);
const tablist = h("div", { class: "tabs", role: "tablist", "aria-label": t("rp.details") }, ...tabButtons);
const banner = h(
  "div",
  { class: "banner", role: "alert" },
  h("span", {}, t("rp.unavailable")),
  h("button", { type: "button", class: "link", onclick: () => void refresh() }, t("rp.refresh")),
);
const tabpanel = h("section", { class: "tabpanel", id: "tabpanel", role: "tabpanel" });
// A sibling of "打开 Tidy", not nested in it (no button inside a button).
const updateDot = h("button", {
  type: "button",
  class: "update-dot",
  "aria-label": t("rp.updateAvailable"),
  title: t("rp.updateAvailable"),
  onclick: () => openMain("settings"),
});
updateDot.hidden = true;
const footer = h(
  "footer",
  {},
  h(
    "button",
    {
      type: "button",
      class: "icon-btn",
      "aria-label": t("rp.settings"),
      title: t("rp.settings"),
      onclick: () => openMain("settings"),
    },
    icon("gear", 16),
  ),
  h(
    "div",
    { class: "foot-center" },
    h(
      "button",
      { type: "button", class: "foot-main", onclick: () => openMain("main") },
      h("img", { src: "/tidy-logo.png", alt: "", class: "foot-logo" }),
      t("rp.openMain", { app: PRODUCT_NAME }),
    ),
    updateDot,
  ),
  h("button", { type: "button", class: "foot-quit", onclick: () => void residentQuit().catch(() => setError(true)) }, t("rp.quit")),
);
const root = document.getElementById("panel")!;
root.setAttribute("aria-label", t("rp.title"));
root.append(h("div", { class: "body" }, tablist, banner, tabpanel), footer);
document.documentElement.lang = lang() === "zh" ? "zh-CN" : "en";

// ---- behaviour ------------------------------------------------------------

/** Show or clear the "could not update" banner. */
function setError(on: boolean): void {
  state.error = on;
  show(banner, on);
}

/** Switch tabs: rebuild the body, persist, and tell the sampler. */
function selectTab(tab: PanelTab, focus: boolean): void {
  const changed = tab !== state.tab || body === null;
  state.tab = tab;
  tabButtons.forEach((btn, i) => {
    const on = TABS[i] === tab;
    btn.setAttribute("aria-selected", String(on));
    btn.tabIndex = on ? 0 : -1;
  });
  tabpanel.setAttribute("aria-labelledby", `tab-${tab}`);
  if (focus) tabButtons[TABS.indexOf(tab)].focus();
  if (!changed) return;
  localStorage.setItem(TAB_KEY, tab);
  body?.dispose?.();
  body = buildTab(tab);
  tabpanel.dataset.tab = tab;
  tabpanel.replaceChildren(body.el);
  if (state.reading) body.update(state.reading);
  void residentSelectTab(tab).catch(() => {});
}

/** Roving focus: ←/→ (and Home/End) move between tabs. */
function onTabKey(ev: KeyboardEvent, tab: PanelTab): void {
  const i = TABS.indexOf(tab);
  let next: number | null = null;
  if (ev.key === "ArrowRight") next = (i + 1) % TABS.length;
  else if (ev.key === "ArrowLeft") next = (i - 1 + TABS.length) % TABS.length;
  else if (ev.key === "Home") next = 0;
  else if (ev.key === "End") next = TABS.length - 1;
  if (next === null) return;
  ev.preventDefault();
  selectTab(TABS[next], true);
}

/** Accept a reading from the event stream, a seed or a manual refresh. */
function accept(reading: ResidentReading): void {
  state.reading = reading;
  state.lastReadingAt = performance.now();
  setError(false);
  markSample();
  body?.update(reading);
}

/** Banner "刷新": force one sample now. */
async function refresh(): Promise<void> {
  try {
    accept(await residentRefresh());
  } catch {
    setError(true);
  }
}

/** Footer / card navigation into the main window. */
function openMain(destination: MainDestination): void {
  void residentOpenMain(destination).catch(() => setError(true));
}

/** Show the footer dot when the main app already knows of a new version
 * (cached, no network). */
async function refreshUpdateDot(): Promise<void> {
  const available = await residentUpdateAvailable().catch(() => false);
  show(updateDot, available);
}

/** Panel shown or hidden by the shell. */
function onVisibility(visible: boolean): void {
  state.visible = visible;
  setSparkVisible(visible);
  if (!visible) return;
  // The prewarmed page survives hides: re-seed instead of waiting a tick,
  // and drop a release result from an earlier visit.
  resetReleaseResult();
  loadReleasePrefs();
  body?.resume?.();
  void refreshUpdateDot();
  state.lastReadingAt = performance.now();
  void residentGetReading()
    .then((r) => r && accept(r))
    .catch(() => setError(true));
  root.focus({ preventScroll: true });
}

// Escape hides the panel unless a dialog or menu is handling it.
window.addEventListener("keydown", (ev) => {
  if (ev.key !== "Escape" || ev.defaultPrevented || dialogOpen()) return;
  if (ev.target instanceof Element && ev.target.closest('[role="dialog"], [role="menu"]')) return;
  ev.preventDefault();
  void residentHidePanel().catch(() => {});
});

// Updates stalled while on screen → show the banner (values stay visible).
window.setInterval(() => {
  if (state.visible && !document.hidden && state.reading && performance.now() - state.lastReadingAt > STALL_MS) {
    setError(true);
  }
}, 2000);

/** Boot: render the remembered tab, report ready, subscribe, seed. */
async function boot(): Promise<void> {
  setError(false);
  selectTab(state.tab, false);
  loadReleasePrefs();
  void refreshUpdateDot();
  // Ready first: the shell may reveal the window as soon as a frame exists.
  void residentPanelReady()
    .then(() => root.focus({ preventScroll: true }))
    .catch(() => {});
  try {
    await listen<ResidentReading>("resident-reading", (ev) => accept(ev.payload));
    await listen<boolean>("resident-panel-visibility", (ev) => onVisibility(ev.payload));
    const seed = await residentGetReading();
    if (seed) accept(seed);
  } catch {
    setError(true);
  }
}

void boot();
