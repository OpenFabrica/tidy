// Memory tab header (C6) and the release settings dialog (C8): pressure
// chip, usage bar, and a split button — "Release" on the left, a chevron
// menu on the right with the automatic-release state, a shortcut to the
// settings dialog, and free/swap facts. The release result stays on the
// button for three seconds, then the button returns to "Release".

import { openDialog } from "./dialog";
import { h, icon, setAttr, setIcon, setText, show } from "./dom";
import * as fmt from "./fmt";
import { pressureChip } from "./cards";
import type { Widget } from "./widget";
import { t } from "../i18n";
import { residentGetReleasePrefs, residentReleaseMemory, residentSetReleasePrefs } from "../ipc";
import type { ReleasePrefs, ReleaseReport, ResidentReading } from "../types";

/** Release state shared across tab switches (the panel is long-lived). */
const release = {
  running: false,
  result: null as ReleaseReport | null,
  clearTimer: 0,
  prefs: null as ReleasePrefs | null,
  prefsFailed: false,
  listeners: new Set<() => void>(),
};

/** Notify mounted memory headers that release state changed. */
function changed(): void {
  release.listeners.forEach((fn) => fn());
}

/** Reload the automatic-release prefs (panel shown, dialog saved). */
export function loadReleasePrefs(): void {
  void residentGetReleasePrefs()
    .then((p) => {
      release.prefs = p;
      release.prefsFailed = false;
    })
    .catch(() => {
      release.prefsFailed = true;
    })
    .finally(changed);
}

/** The panel was re-shown: an old result must not read as a new action. */
export function resetReleaseResult(): void {
  if (release.running) return;
  window.clearTimeout(release.clearTimer);
  release.result = null;
  changed();
}

/** Run one release; the result clears itself three seconds later. */
async function runRelease(): Promise<void> {
  if (release.running) return;
  release.running = true;
  release.result = null;
  window.clearTimeout(release.clearTimer);
  changed();
  try {
    release.result = await residentReleaseMemory();
  } catch {
    release.result = { status: "failed", observed_reduction_bytes: null, reduction_percent: null };
  }
  release.running = false;
  release.clearTimer = window.setTimeout(() => {
    release.result = null;
    changed();
  }, 3000);
  changed();
}

/** Short button label + long tooltip for the current release state. */
function releaseLabels(): { short: string; long: string; icon: string } {
  if (release.running) return { short: t("rp.release.running"), long: "", icon: "refresh" };
  const r = release.result;
  if (!r) return { short: t("rp.release.action"), long: t("rp.release.hint"), icon: "zap" };
  if (r.status === "completed") {
    const reduced = r.observed_reduction_bytes ?? null;
    if (reduced === null) return { short: t("rp.release.done"), long: t("rp.release.unmeasured"), icon: "check" };
    if (reduced > 0 && (r.reduction_percent ?? 0) > 0) {
      return {
        short: t("rp.release.down", { percent: r.reduction_percent ?? 0 }),
        long: t("rp.release.observed", { size: fmt.memory(reduced) }),
        icon: "check",
      };
    }
    if (reduced > 0) {
      return { short: t("rp.release.done"), long: t("rp.release.observed", { size: fmt.memory(reduced) }), icon: "check" };
    }
    return { short: t("rp.release.noChange"), long: t("rp.release.noChangeHint"), icon: "check" };
  }
  return {
    short: t(`rp.release.${r.status}`),
    long: t(`rp.release.${r.status}Hint`),
    icon: r.status === "failed" ? "refresh" : "zap",
  };
}

/** The automatic-release rule sentence for the dropdown. */
function ruleText(p: ReleasePrefs): string {
  return p.threshold_percent
    ? t("rp.release.ruleThreshold", { minutes: p.interval_minutes, percent: p.threshold_percent })
    : t("rp.release.ruleAny", { minutes: p.interval_minutes });
}

/** Memory tab header: title + pressure, percent, bar, used/total + split button. */
export function memoryHeader(): Widget {
  const chip = pressureChip();
  const num = h("span", { class: "num" }, "—");
  const unit = h("small", {}, "%");
  const fill = h("i");
  const bar = h(
    "div",
    { class: "bar", role: "progressbar", "aria-label": t("rp.memoryUsage"), "aria-valuemin": 0, "aria-valuemax": 100 },
    fill,
  );
  const used = h("span", { class: "ell mono" });

  const relIcon = icon("zap", 12);
  const relLabel = h("span", { class: "rel-label", role: "status", "aria-live": "polite", "aria-atomic": "true" });
  const relBtn = h(
    "button",
    { type: "button", class: "split-main", "aria-label": t("rp.release.action"), onclick: () => void runRelease() },
    relIcon,
    relLabel,
  );
  const menuBtn = h(
    "button",
    {
      type: "button",
      class: "split-menu",
      "aria-label": t("rp.memoryOptions"),
      "aria-haspopup": "menu",
      "aria-expanded": "false",
    },
    icon("chevronDown", 12),
  );

  // Dropdown menu: state label, rule note, settings item, free/swap facts.
  const menuState = h("div", { class: "menu-label" });
  const menuRule = h("p", { class: "menu-note" });
  const reloadItem = h("button", { type: "button", role: "menuitem", class: "menu-item" }, t("rp.release.reload"));
  const settingsItem = h(
    "button",
    { type: "button", role: "menuitem", class: "menu-item" },
    icon("gear", 13),
    t("rp.release.settings"),
  );
  const freeVal = h("span", { class: "mono" });
  const swapVal = h("span", { class: "mono" });
  const menu = h(
    "div",
    { class: "menu", role: "menu", "aria-label": t("rp.memoryOptions") },
    menuState,
    menuRule,
    reloadItem,
    settingsItem,
    h("div", { class: "menu-sep", role: "separator" }),
    h("div", { class: "menu-fact" }, h("span", {}, t("rp.memFree")), freeVal),
    h("div", { class: "menu-fact" }, h("span", {}, t("rp.memSwap")), swapVal),
  );
  menu.hidden = true;
  const split = h("div", { class: "split" }, relBtn, menuBtn, menu);

  /** Close the dropdown, optionally returning focus to its trigger. */
  const closeMenu = (refocus: boolean) => {
    if (menu.hidden) return;
    menu.hidden = true;
    setAttr(menuBtn, "aria-expanded", "false");
    document.removeEventListener("pointerdown", onOutside, true);
    if (refocus) menuBtn.focus();
  };
  /** Pointer outside the split button closes the menu. */
  const onOutside = (ev: PointerEvent) => {
    if (!split.contains(ev.target as Node)) closeMenu(false);
  };
  /** Open the dropdown and focus its first actionable item. */
  const openMenu = () => {
    menu.hidden = false;
    setAttr(menuBtn, "aria-expanded", "true");
    document.addEventListener("pointerdown", onOutside, true);
    paint();
    (reloadItem.hidden ? settingsItem : reloadItem).focus();
  };
  menuBtn.addEventListener("click", () => (menu.hidden ? openMenu() : closeMenu(true)));
  // Menu keyboard: Escape closes (and stays inside the panel), arrows move.
  menu.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      closeMenu(true);
    } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      const items = [reloadItem, settingsItem].filter((b) => !b.hidden);
      const i = items.indexOf(document.activeElement as HTMLButtonElement);
      items[(i + (ev.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
    } else if (ev.key === "Tab") {
      closeMenu(false);
    }
  });
  reloadItem.addEventListener("click", () => loadReleasePrefs());
  settingsItem.addEventListener("click", () => {
    closeMenu(false);
    openReleaseSettings(menuBtn);
  });

  /** Repaint everything that depends on release state. */
  const paint = () => {
    const labels = releaseLabels();
    setText(relLabel, labels.short);
    setIcon(relIcon, labels.icon);
    relIcon.classList.toggle("spin", release.running);
    relBtn.disabled = release.running;
    setAttr(relBtn, "aria-busy", release.running ? "true" : null);
    setAttr(relBtn, "title", labels.long || null);
    relBtn.dataset.state = release.running ? "running" : release.result?.status ?? "idle";
    const p = release.prefs;
    setText(
      menuState,
      release.prefsFailed
        ? t("rp.release.loadFailed")
        : p === null
          ? t("rp.loading")
          : t(p.automatic ? "rp.release.autoOn" : "rp.release.autoOff"),
    );
    show(menuRule, !!p?.automatic && !release.prefsFailed);
    if (p?.automatic) setText(menuRule, ruleText(p));
    show(reloadItem, release.prefsFailed);
  };
  release.listeners.add(paint);
  paint();

  const el = h(
    "section",
    { class: "card mem-head", "aria-label": t("rp.tab.memory") },
    h(
      "header",
      {},
      h("div", { class: "mem-title" }, h("span", { class: "sum-title" }, t("rp.tab.memory")), chip.el),
      h("strong", { class: "sum-value" }, num, unit),
    ),
    bar,
    h("div", { class: "sum-meta mem-foot" }, used, split),
  );
  return {
    el,
    update(r: ResidentReading) {
      const m = r.memory;
      const text = fmt.pct(m.usage_percent);
      setText(num, text);
      show(unit, text !== "—");
      const v = m.usage_percent === null ? 0 : Math.max(0, Math.min(100, m.usage_percent));
      fill.style.width = `${v}%`;
      setAttr(bar, "aria-valuenow", m.usage_percent === null ? null : String(Math.round(v)));
      chip.set(m.pressure, r.stale && m.usage_percent === null);
      setText(
        used,
        m.total_bytes ? t("rp.usedCapacity", { used: fmt.memory(m.used_bytes), total: fmt.memory(m.total_bytes) }) : "—",
      );
      setText(freeVal, fmt.memory(m.free_bytes));
      setText(swapVal, fmt.memory(m.swap_used_bytes));
    },
    dispose() {
      release.listeners.delete(paint);
      closeMenu(false);
    },
  };
}

/** The release settings dialog (C8): automatic switch, interval, threshold. */
export function openReleaseSettings(returnTo: HTMLElement): void {
  const current: ReleasePrefs = release.prefs ?? { automatic: false, interval_minutes: 15, threshold_percent: 80 };
  const draft = { ...current };
  const toggle = h("button", {
    type: "button",
    class: `switch ${draft.automatic ? "on" : ""}`,
    role: "switch",
    "aria-checked": String(draft.automatic),
    "aria-labelledby": "rel-auto-label",
  });
  const interval = h(
    "select",
    { id: "rel-interval" },
    ...[3, 5, 15, 30, 60, 120].map((m) =>
      h("option", { value: m, selected: m === draft.interval_minutes }, t("rp.release.minutes", { count: m })),
    ),
  );
  const threshold = h(
    "select",
    { id: "rel-threshold" },
    ...[null, 70, 80, 90].map((v) =>
      h("option", { value: v ?? "", selected: v === draft.threshold_percent }, v ? `${v}%` : t("rp.release.any")),
    ),
  );
  /** Interval/threshold only matter while automatic release is on. */
  const syncDisabled = () => {
    interval.disabled = !draft.automatic;
    threshold.disabled = !draft.automatic;
  };
  toggle.addEventListener("click", () => {
    draft.automatic = !draft.automatic;
    toggle.classList.toggle("on", draft.automatic);
    toggle.setAttribute("aria-checked", String(draft.automatic));
    syncDisabled();
  });
  interval.addEventListener("change", () => (draft.interval_minutes = Number(interval.value)));
  threshold.addEventListener("change", () => {
    draft.threshold_percent = threshold.value ? Number(threshold.value) : null;
  });
  syncDisabled();

  const error = h("p", { class: "alert", role: "alert" });
  error.hidden = true;
  const cancel = h("button", { type: "button", class: "btn" }, t("rp.cancel"));
  const save = h("button", { type: "button", class: "btn primary" }, t("rp.save"));
  const body = h(
    "div",
    { class: "form" },
    h(
      "div",
      { class: "form-row" },
      h(
        "div",
        { class: "form-text" },
        h("span", { id: "rel-auto-label", class: "form-title" }, t("rp.release.automatic")),
        h("span", { class: "form-desc" }, t("rp.release.automaticDesc")),
      ),
      toggle,
    ),
    h("label", { class: "form-row", for: "rel-interval" }, h("span", { class: "form-title" }, t("rp.release.interval")), interval),
    h(
      "label",
      { class: "form-row", for: "rel-threshold" },
      h("span", { class: "form-title" }, t("rp.release.threshold")),
      threshold,
    ),
    error,
  );
  const dlg = openDialog({
    title: t("rp.release.title"),
    body,
    footer: h("div", { class: "dialog-actions" }, cancel, save),
    onClose: () => returnTo.isConnected && returnTo.focus(),
  });
  cancel.addEventListener("click", () => dlg.close());
  save.addEventListener("click", async () => {
    dlg.setBusy(true);
    save.disabled = true;
    cancel.disabled = true;
    setText(save, t("rp.saving"));
    error.hidden = true;
    try {
      release.prefs = await residentSetReleasePrefs(draft);
      release.prefsFailed = false;
      changed();
      dlg.setBusy(false);
      dlg.close();
    } catch {
      dlg.setBusy(false);
      save.disabled = false;
      cancel.disabled = false;
      setText(save, t("rp.save"));
      setText(error, t("rp.release.saveFailed"));
      error.hidden = false;
    }
  });
}
