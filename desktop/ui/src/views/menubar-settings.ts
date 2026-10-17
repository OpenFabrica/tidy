// Menu-bar configuration for the settings sheet: the "菜单栏设置" dialog
// (appearance, display items with pointer/keyboard reorder, fan mode) and the
// per-metric color-rule editor stacked above it. Like MangoDisk, every change
// persists at once through resident_set_prefs; "Done" only closes the dialog.
// The backend normalizes and returns the stored value, which becomes the new
// committed state — a failed write leaves the last committed state on screen.

import "../styles/menubar-settings.css";
import { residentGetReading, residentListInterfaces, residentSetPrefs } from "../ipc";
import { esc } from "../format";
import { t } from "../i18n";
import type {
  ColorRule,
  Condition,
  FanDisplay,
  FanReading,
  MetricId,
  NetInterface,
  ResidentPrefs,
  ResidentReading,
  Tone,
} from "../types";

/** Most rules one metric may carry (mirrors the backend cap). */
export const MAX_RULES = 16;

/** Palette order in the editor; "normal" is the menu bar's own text color. */
const TONES: Tone[] = ["normal", "green", "yellow", "blue", "warning", "critical"];
const CONDITIONS: Condition[] = ["below", "range", "at_least"];
const BATTERY_METRICS = new Set<MetricId>(["battery_temperature", "charging_power", "battery_power"]);

/** Localized display name of a metric. */
export function metricName(id: MetricId): string {
  return t(`mb.metric.${id}`);
}

/** Default rules for a metric; mirrors prefs.rs `default_rules`. */
export function defaultRules(id: MetricId): ColorRule[] {
  const three = (lo: number, hi: number): ColorRule[] => [
    { condition: "below", a: lo, b: null, tone: "green" },
    { condition: "range", a: lo, b: hi, tone: "warning" },
    { condition: "at_least", a: hi, b: null, tone: "critical" },
  ];
  switch (id) {
    case "cpu":
    case "gpu":
    case "memory":
    case "disk":
      return three(70, 90);
    case "cpu_temperature":
    case "gpu_temperature":
      return three(60, 80);
    case "disk_temperature":
      return three(50, 60);
    case "battery_temperature":
      return three(40, 45);
    case "fan_speed":
      return three(2000, 4000);
    default:
      return [];
  }
}

/** Unit shown beside a metric's thresholds. */
function ruleUnit(id: MetricId): string {
  if (id === "fan_speed") return "RPM";
  if (id.endsWith("_power")) return "W";
  if (id.endsWith("_temperature")) return "°C";
  return "%";
}

/** Largest threshold accepted for a metric (MangoDisk's limits). */
function ruleLimit(id: MetricId): number {
  if (id === "fan_speed") return 30000;
  if (id === "system_power") return 3000;
  if (id.endsWith("_power")) return 300;
  if (id === "battery_temperature") return 100;
  if (id.endsWith("_temperature")) return 150;
  return 100;
}

// --- committed prefs + serialized writes ------------------------------------

/** Menu-bar prefs shared by the settings row and the dialogs. `prefs` is
 * optimistic (shows a change at once, so quick keyboard moves build on each
 * other); writes are chained so they land in order, and a failed write rolls
 * the view back to the last value the backend confirmed. */
export interface PrefsStore {
  prefs: ResidentPrefs;
  saving: boolean;
  error: boolean;
  /** Persist `next`; resolves true when the backend stored it. */
  save(next: ResidentPrefs): Promise<boolean>;
  /** Run `fn` after every settled write; returns the unsubscribe. */
  subscribe(fn: () => void): () => void;
}

/** Create the store around the prefs loaded at mount. */
export function createPrefsStore(initial: ResidentPrefs): PrefsStore {
  let queue: Promise<unknown> = Promise.resolve();
  let committed = initial;
  let pending = 0;
  const listeners = new Set<() => void>();
  const store: PrefsStore = {
    prefs: initial,
    saving: false,
    error: false,
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    save(next) {
      store.prefs = next;
      store.saving = true;
      pending += 1;
      const run = queue.then(async () => {
        try {
          committed = await residentSetPrefs(next);
          store.error = false;
          return true;
        } catch {
          store.error = true;
          return false;
        } finally {
          pending -= 1;
          // Only the last write in a burst settles the view: adopting an
          // earlier result would briefly undo the newer optimistic change.
          if (pending === 0) {
            store.prefs = committed;
            store.saving = false;
          }
          listeners.forEach((fn) => fn());
        }
      });
      queue = run;
      return run;
    },
  };
  return store;
}

// --- shared dialog plumbing ---------------------------------------------------

/** Close handlers of the open dialogs, innermost last. */
const openDialogs: (() => void)[] = [];

/** Close every open menu-bar dialog (settings view unmount: a route change
 * from the tray menu must not leave a modal over another page). */
export function closeMenubarDialogs(): void {
  while (openDialogs.length > 0) openDialogs.pop()!();
}

/** Focusable descendants in tab order (enabled, rendered). */
function focusables(root: HTMLElement): HTMLElement[] {
  return [
    ...root.querySelectorAll<HTMLElement>(
      'button, input, select, textarea, [tabindex]:not([tabindex="-1"])',
    ),
  ].filter((el) => !(el as HTMLButtonElement).disabled && el.offsetParent !== null);
}

/** Keep Tab / Shift+Tab inside the dialog (aria-modal alone does not). */
function trapTab(dialog: HTMLElement, ev: KeyboardEvent): void {
  if (ev.key !== "Tab") return;
  const items = focusables(dialog);
  if (items.length === 0) return;
  const first = items[0];
  const last = items[items.length - 1];
  const active = document.activeElement as HTMLElement | null;
  if (ev.shiftKey && (active === first || !dialog.contains(active))) {
    ev.preventDefault();
    last.focus();
  } else if (!ev.shiftKey && (active === last || !dialog.contains(active))) {
    ev.preventDefault();
    first.focus();
  }
}

/** Replace a container's markup and put focus back on the element that had
 * it (matched by id), so re-rendering never throws keyboard users out. */
function renderKeepingFocus(root: HTMLElement, html: string): void {
  const activeId = root.contains(document.activeElement) ? document.activeElement?.id : "";
  root.innerHTML = html;
  if (activeId) document.getElementById(activeId)?.focus({ preventScroll: true });
}

/** Build a modal backdrop + dialog shell and attach it to <body>. */
function mountDialog(cls: string, titleId: string): { backdrop: HTMLElement; dialog: HTMLElement } {
  const backdrop = document.createElement("div");
  backdrop.className = `mbset-backdrop ${cls}`;
  backdrop.innerHTML = `<div class="mbset-dialog" role="dialog" aria-modal="true" aria-labelledby="${titleId}"></div>`;
  document.body.append(backdrop);
  return { backdrop, dialog: backdrop.firstElementChild as HTMLElement };
}

/** Small grip glyph for the drag handle. */
const GRIP = `<svg width="10" height="14" viewBox="0 0 10 14" aria-hidden="true">${[2, 7, 12]
  .map((y) => `<circle cx="3" cy="${y}" r="1.2"/><circle cx="7" cy="${y}" r="1.2"/>`)
  .join("")}</svg>`;

// --- configure dialog ---------------------------------------------------------

/** Open "菜单栏设置". `opener` gets focus back on close. */
export function openConfigureDialog(store: PrefsStore, opener: HTMLElement | null): void {
  const { backdrop, dialog } = mountDialog("", "mbset-title");
  let reading: ResidentReading | null = null;
  let ruleEditorOpen = false;
  let drag: DragState | null = null;
  let announcement = "";
  /** Interface choices; null while loading. Listed only while the dialog is
   * open (each call costs a few configd round-trips). */
  let interfaces: NetInterface[] | null = null;
  let interfacesFailed = false;

  void residentListInterfaces()
    .then((list) => {
      interfaces = list;
    })
    .catch(() => {
      interfaces = [];
      interfacesFailed = true;
    })
    .finally(() => draw());

  // Fans and battery presence come from the latest cached sample; without
  // one every item stays listed (we never hide what we cannot disprove).
  void residentGetReading()
    .then((r) => {
      reading = r;
      draw();
    })
    .catch(() => {});

  /** Battery rows hide only when a sample positively reports no battery. */
  const visibleIds = (): MetricId[] =>
    store.prefs.metrics
      .map((m) => m.id)
      .filter((id) => !(BATTERY_METRICS.has(id) && reading && reading.power.battery_percent === null));

  /** The logo also renders when nothing else would (prefs.effective_icon). */
  const showIcon = () => store.prefs.show_icon || !store.prefs.metrics.some((m) => m.enabled);

  /** Selected items (logo + enabled visible metrics); the last one is locked. */
  const selectedCount = () => {
    const visible = new Set(visibleIds());
    return (
      Number(showIcon()) + store.prefs.metrics.filter((m) => m.enabled && visible.has(m.id)).length
    );
  };

  /** Persist a partial change on top of the committed prefs. */
  const change = (patch: Partial<ResidentPrefs>) => {
    void store.save({ ...store.prefs, ...patch });
    draw();
  };

  /** Fan mode + fan choices, shown under the fan row. */
  const fanBlock = (enabled: boolean): string => {
    const fans: FanReading[] = reading?.fans ?? [];
    const ids = [...new Set([...fans.map((f) => f.id), ...store.prefs.fan_ids])].sort((a, b) => a - b);
    const mode = store.prefs.fan_display;
    const modes: FanDisplay[] = ["highest", "average", "selected"];
    const chosen = store.prefs.fan_ids;
    const choices =
      mode !== "selected"
        ? ""
        : ids.length === 0
          ? `<div class="mbset-fan-none muted">${t("mb.fan.none")}</div>`
          : `<div class="mbset-fans" role="group" aria-label="${esc(t("mb.fan.limit"))}">
              ${ids
                .map((id) => {
                  const on = chosen.includes(id);
                  const locked = !enabled || (on ? chosen.length === 1 : chosen.length >= 2);
                  const fan = fans.find((f) => f.id === id);
                  const missing = reading !== null && !fan;
                  return `<label class="mbset-fan">
                    <input type="checkbox" id="mbset-fan-${id}" data-fan="${id}" ${on ? "checked" : ""} ${locked ? "disabled" : ""} />
                    <span>${esc(t("mb.fan.number", { number: id + 1 }))}${missing ? ` · ${esc(t("mb.fan.missing"))}` : ""}</span>
                    <span class="mbset-rpm">${fan ? Math.round(fan.rpm) : "—"}</span>
                  </label>`;
                })
                .join("")}
              <span class="mbset-fan-hint muted">${t("mb.fan.limit")}</span>
            </div>`;
    return `<div class="mbset-sub">
      <div class="segmented mbset-seg" role="radiogroup" aria-label="${esc(t("mb.fan.mode"))}">
        ${modes
          .map(
            (m) =>
              `<button id="mbset-fanmode-${m}" data-fanmode="${m}" role="radio" aria-checked="${m === mode}"
                 class="${m === mode ? "on" : ""}" ${enabled ? "" : "disabled"}>${t(`mb.fan.${m}`)}</button>`,
          )
          .join("")}
      </div>
      ${choices}
    </div>`;
  };

  /** Interface picker under the network row: 自动 (null) + every service as
   * "display (name)"; a saved interface that is gone stays selectable. */
  const netBlock = (): string => {
    const saved = store.prefs.network_interface;
    const list = interfaces ?? [];
    const options = [
      `<option value="" ${saved === null ? "selected" : ""}>${esc(t("mb.net.auto"))}</option>`,
      ...list.map(
        (i) =>
          `<option value="${esc(i.name)}" ${i.name === saved ? "selected" : ""}>${esc(
            `${i.display} (${i.name})${i.active ? "" : ` · ${t("mb.net.inactive")}`}`,
          )}</option>`,
      ),
    ];
    if (saved !== null && !list.some((i) => i.name === saved)) {
      options.push(
        `<option value="${esc(saved)}" selected>${esc(`${saved} · ${t(interfaces ? "mb.net.missing" : "mb.net.loading")}`)}</option>`,
      );
    }
    return `<div class="mbset-sub mbset-net">
      <label class="mbset-net-row" for="mbset-netif">
        <span>${t("mb.net.label")}</span>
        <select id="mbset-netif" ${interfaces === null || store.saving ? "disabled" : ""}>${options.join("")}</select>
      </label>
      ${
        interfaces === null
          ? `<span class="mbset-fan-hint">${t("mb.net.loading")}</span>`
          : interfacesFailed
            ? `<span class="mbset-fan-hint">${t("mb.net.failed")}</span>`
            : ""
      }
    </div>`;
  };

  /** Tone dots previewing a metric's rules on its Colors button. */
  const toneDots = (rules: ColorRule[]) =>
    [...new Set(rules.map((r) => r.tone))]
      .slice(0, 4)
      .map((tone) => `<i class="mbset-dot" data-tone="${tone}"></i>`)
      .join("");

  /** One metric row of the display list. */
  const metricRow = (id: MetricId): string => {
    const entry = store.prefs.metrics.find((m) => m.id === id)!;
    const name = metricName(id);
    const locked = entry.enabled && selectedCount() <= 1;
    const color =
      id === "network"
        ? ""
        : `<button class="mbset-color" id="mbset-color-${id}" data-color="${id}" aria-haspopup="dialog"
             aria-label="${esc(t("mb.color.configure", { name }))}" ${store.saving ? "disabled" : ""}>
             <span class="mbset-dots" aria-hidden="true">${toneDots(entry.rules)}</span>${t("mb.color.action")}
           </button>`;
    return `<div class="mbset-item mbset-metric" data-metric="${id}">
      <div class="mbset-line">
        <button class="mbset-grip" id="mbset-grip-${id}" data-grip="${id}"
          aria-label="${esc(t("mb.reorder", { name }))}" aria-describedby="mbset-kbd">${GRIP}</button>
        <label class="mbset-check" for="mbset-m-${id}">
          <input type="checkbox" id="mbset-m-${id}" data-toggle="${id}" ${entry.enabled ? "checked" : ""} ${locked ? "disabled" : ""} />
          <span>${esc(name)}</span>
        </label>
        ${color}
      </div>
      ${id === "fan_speed" ? fanBlock(entry.enabled) : ""}
      ${id === "network" ? netBlock() : ""}
    </div>`;
  };

  /** Full dialog markup. */
  const markup = (): string => {
    const p = store.prefs;
    const logoLocked = showIcon() && selectedCount() <= 1;
    return `
      <header class="mbset-head">
        <h2 id="mbset-title">${t("mb.title")}</h2>
      </header>
      <div class="mbset-body">
        <section class="mbset-group" aria-labelledby="mbset-appearance">
          <h3 id="mbset-appearance" class="mbset-kicker">${t("mb.appearance")}</h3>
          <div class="mbset-opt">
            <div class="set-text">
              <div class="set-title" id="mbset-compact-label">${t("mb.compact")}</div>
              <div class="set-desc" id="mbset-compact-desc">${t("mb.compact.desc")}</div>
            </div>
            <button class="toggle ${p.compact ? "on" : ""}" id="mbset-compact" role="switch" aria-checked="${p.compact}"
              aria-labelledby="mbset-compact-label" aria-describedby="mbset-compact-desc"></button>
          </div>
          <div class="mbset-opt">
            <div class="set-text">
              <div class="set-title" id="mbset-colors-label">${t("mb.colors")}</div>
              <div class="set-desc" id="mbset-colors-desc">${t("mb.colors.desc")}</div>
            </div>
            <button class="toggle ${p.usage_colors ? "on" : ""}" id="mbset-colors" role="switch" aria-checked="${p.usage_colors}"
              aria-labelledby="mbset-colors-label" aria-describedby="mbset-colors-desc"></button>
          </div>
        </section>
        <section class="mbset-group" aria-labelledby="mbset-items">
          <div class="mbset-items-head">
            <h3 id="mbset-items" class="mbset-kicker">${t("mb.items")}</h3>
            <span class="muted" id="mbset-kbd">${t("mb.items.drag")} · ${t("mb.items.kbd")}</span>
          </div>
          <div class="mbset-list" id="mbset-list">
            <div class="mbset-item mbset-logo">
              <div class="mbset-line">
                <span class="mbset-logo-mark" aria-hidden="true"><img src="/tidy-logo.png" alt="" /></span>
                <label class="mbset-check" for="mbset-logo">
                  <input type="checkbox" id="mbset-logo" ${showIcon() ? "checked" : ""} ${logoLocked ? "disabled" : ""} />
                  <span>${t("mb.logo")}</span>
                </label>
              </div>
            </div>
            ${visibleIds().map(metricRow).join("")}
          </div>
          <span class="mbset-sr" aria-live="polite" id="mbset-live">${esc(announcement)}</span>
        </section>
      </div>
      <footer class="mbset-foot">
        <span class="mbset-status" role="status">${
          store.error
            ? `<span class="mbset-err">${t("mb.saveFailed")}</span>`
            : store.saving
              ? `<span class="muted">${t("mb.saving")}</span>`
              : ""
        }</span>
        <button class="mbset-primary" id="mbset-done">${t("mb.done")}</button>
      </footer>`;
  };

  /** Re-render unless a pointer drag owns the list's DOM order. */
  function draw(): void {
    if (!backdrop.isConnected || drag?.active) return;
    renderKeepingFocus(dialog, markup());
  }

  /** Announce a row's new position to screen readers. */
  const announce = (id: MetricId, order: MetricId[]) => {
    announcement = t("mb.moved", {
      name: metricName(id),
      position: order.indexOf(id) + 1,
      count: order.length,
    });
    const live = dialog.querySelector("#mbset-live");
    if (live) live.textContent = announcement;
  };

  /** Move `id` to `target`'s slot in the full metric list (hidden rows keep
   * their relative place) and persist the new order. */
  const reorderTo = (id: MetricId, target: MetricId) => {
    const metrics = [...store.prefs.metrics];
    const from = metrics.findIndex((m) => m.id === id);
    const to = metrics.findIndex((m) => m.id === target);
    if (from < 0 || to < 0 || from === to) return;
    const [item] = metrics.splice(from, 1);
    metrics.splice(to, 0, item);
    change({ metrics });
  };

  /** Keyboard reorder: arrows on the grip, Alt+arrows anywhere in a row. */
  const keyMove = (ev: KeyboardEvent): boolean => {
    if (ev.key !== "ArrowUp" && ev.key !== "ArrowDown") return false;
    const target = ev.target as HTMLElement;
    const row = target.closest<HTMLElement>(".mbset-metric");
    if (!row || !(ev.altKey || target.classList.contains("mbset-grip"))) return false;
    ev.preventDefault();
    const id = row.dataset.metric as MetricId;
    const order = visibleIds();
    const next = order[order.indexOf(id) + (ev.key === "ArrowUp" ? -1 : 1)];
    if (next) {
      reorderTo(id, next);
      // draw() above restored focus by id; WebKit can still drop it when the
      // node is replaced, so pin it explicitly.
      document.getElementById(target.id)?.focus({ preventScroll: true });
    }
    announce(id, visibleIds());
    return true;
  };

  /** Close the dialog and give focus back to the opener. */
  const close = () => {
    if (!backdrop.isConnected) return;
    openDialogs.splice(openDialogs.indexOf(close), 1);
    cancelDrag();
    unsubscribe();
    backdrop.remove();
    document.removeEventListener("keydown", onKey, true);
    // The settings tab may have re-rendered behind us; find the new button.
    const target = opener?.isConnected ? opener : opener?.id ? document.getElementById(opener.id) : null;
    target?.focus();
  };
  openDialogs.push(close);

  /** Dialog-level keys: Escape (drag first), focus trap, keyboard reorder. */
  function onKey(ev: KeyboardEvent): void {
    if (ruleEditorOpen) return; // the stacked editor owns the keyboard
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      if (drag) cancelDrag();
      else close();
      return;
    }
    if (dialog.contains(ev.target as Node) && keyMove(ev)) return;
    trapTab(dialog, ev);
  }
  document.addEventListener("keydown", onKey, true);

  // --- pointer drag ---------------------------------------------------------

  interface DragState {
    id: MetricId;
    pointerId: number;
    x: number;
    y: number;
    active: boolean;
    row: HTMLElement;
    list: HTMLElement;
    preview: HTMLElement | null;
    /** Stable slot rects captured at press time; hit-testing these instead
     * of the moving rows stops two neighbours from swapping back and forth. */
    slots: DOMRect[];
    order: MetricId[];
    /** Row index at press time (its slot anchors the pointer offset). */
    start: number;
  }

  /** Abort a drag: drop the preview and redraw from committed prefs. */
  function cancelDrag(): void {
    if (!drag) return;
    drag.preview?.remove();
    const wasActive = drag.active;
    drag = null;
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", onPointerUp);
    window.removeEventListener("pointercancel", cancelDrag);
    window.removeEventListener("blur", cancelDrag);
    window.removeEventListener("scroll", cancelDrag, true);
    if (wasActive) draw();
  }

  /** Follow the pointer; reorder rows in the DOM as the preview crosses slots. */
  function onPointerMove(ev: PointerEvent): void {
    if (!drag || ev.pointerId !== drag.pointerId) return;
    const dx = ev.clientX - drag.x;
    const dy = ev.clientY - drag.y;
    if (!drag.active) {
      if (Math.hypot(dx, dy) < 4) return;
      drag.active = true;
      const rect = drag.row.getBoundingClientRect();
      // An inert presentation-only clone floats under the pointer; the real
      // row stays in the list as the drop placeholder.
      const preview = drag.row.cloneNode(true) as HTMLElement;
      preview.classList.add("mbset-preview");
      preview.setAttribute("aria-hidden", "true");
      preview.inert = true;
      preview.querySelectorAll("[id]").forEach((el) => el.removeAttribute("id"));
      Object.assign(preview.style, {
        left: `${rect.left}px`,
        top: `${rect.top}px`,
        width: `${rect.width}px`,
      });
      document.body.append(preview);
      drag.preview = preview;
      drag.row.classList.add("mbset-placeholder");
      drag.list.classList.add("mbset-dragging");
    }
    drag.preview!.style.transform = `translate3d(${dx}px, ${dy}px, 0)`;
    const startRect = drag.slots[drag.start];
    const cy = startRect.top + startRect.height / 2 + dy;
    let index = drag.slots.findIndex((s) => cy >= s.top && cy <= s.bottom);
    if (index < 0) index = cy < drag.slots[0].top ? 0 : drag.slots.length - 1;
    const current = drag.order.indexOf(drag.id);
    if (index === current) return;
    drag.order.splice(current, 1);
    drag.order.splice(index, 0, drag.id);
    // Re-append rows in the new order (the logo row stays first).
    for (const id of drag.order) {
      const el = drag.list.querySelector(`[data-metric="${id}"]`);
      if (el) drag.list.append(el);
    }
    announce(drag.id, drag.order);
  }

  /** Drop: commit when released over the list and the order changed. */
  function onPointerUp(ev: PointerEvent): void {
    if (!drag || ev.pointerId !== drag.pointerId) return;
    const state = drag;
    const over = document.elementFromPoint(ev.clientX, ev.clientY);
    const changed = state.order.some((id, i) => id !== visibleIds()[i]);
    if (!state.active || !over || !state.list.contains(over) || !changed) {
      cancelDrag();
      return;
    }
    state.preview?.remove();
    drag = null;
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", onPointerUp);
    window.removeEventListener("pointercancel", cancelDrag);
    window.removeEventListener("blur", cancelDrag);
    window.removeEventListener("scroll", cancelDrag, true);
    // Merge the visible order back into the full list: hidden (battery) rows
    // keep their absolute slots, visible rows fill the rest in drag order.
    const byId = new Map(store.prefs.metrics.map((m) => [m.id, m]));
    const queue = [...state.order];
    const metrics = store.prefs.metrics.map((m) =>
      state.order.includes(m.id) ? byId.get(queue.shift()!)! : m,
    );
    change({ metrics });
    document.getElementById(`mbset-grip-${state.id}`)?.focus({ preventScroll: true });
  }

  // --- event wiring (delegated; survives re-renders) ------------------------

  dialog.addEventListener("pointerdown", (ev) => {
    const grip = (ev.target as HTMLElement).closest<HTMLElement>(".mbset-grip");
    if (!grip || ev.button !== 0 || ruleEditorOpen) return;
    ev.preventDefault();
    grip.focus({ preventScroll: true });
    cancelDrag();
    const list = dialog.querySelector<HTMLElement>("#mbset-list")!;
    const rows = [...list.querySelectorAll<HTMLElement>(".mbset-metric")];
    const order = rows.map((r) => r.dataset.metric as MetricId);
    drag = {
      id: grip.dataset.grip as MetricId,
      pointerId: ev.pointerId,
      x: ev.clientX,
      y: ev.clientY,
      active: false,
      row: grip.closest<HTMLElement>(".mbset-metric")!,
      list,
      preview: null,
      slots: rows.map((r) => r.getBoundingClientRect()),
      order,
      start: order.indexOf(grip.dataset.grip as MetricId),
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", cancelDrag);
    window.addEventListener("blur", cancelDrag);
    // Slot rects are captured at press time, so any scroll invalidates them.
    window.addEventListener("scroll", cancelDrag, true);
  });

  dialog.addEventListener("click", (ev) => {
    const el = ev.target as HTMLElement;
    if (el.closest("#mbset-done")) return close();
    if (el.closest("#mbset-compact")) return change({ compact: !store.prefs.compact });
    if (el.closest("#mbset-colors")) return change({ usage_colors: !store.prefs.usage_colors });
    const mode = el.closest<HTMLElement>("[data-fanmode]");
    if (mode) return change({ fan_display: mode.dataset.fanmode as FanDisplay });
    const color = el.closest<HTMLElement>("[data-color]");
    if (color) {
      cancelDrag();
      ruleEditorOpen = true;
      openRuleEditor(store, color.dataset.color as MetricId, () => {
        ruleEditorOpen = false;
        draw();
        document.getElementById(`mbset-color-${color.dataset.color}`)?.focus();
      });
    }
  });

  dialog.addEventListener("change", (ev) => {
    const input = ev.target as HTMLInputElement;
    if (input.id === "mbset-netif") {
      return change({ network_interface: input.value === "" ? null : input.value });
    }
    if (input.id === "mbset-logo") {
      // The logo is the last-resort item: it cannot be cleared when it is
      // the only thing left in the menu bar.
      if (!input.checked && selectedCount() <= 1) return draw();
      return change({ show_icon: input.checked });
    }
    const metric = input.dataset.toggle as MetricId | undefined;
    if (metric) {
      if (!input.checked && selectedCount() <= 1) return draw();
      // Freeze the logo's effective state so enabling the first metric does
      // not silently drop a logo that was shown only as the fallback.
      return change({
        show_icon: showIcon(),
        metrics: store.prefs.metrics.map((m) =>
          m.id === metric ? { ...m, enabled: input.checked } : m,
        ),
      });
    }
    const fan = input.dataset.fan;
    if (fan !== undefined) {
      const id = Number(fan);
      const ids = input.checked
        ? [...store.prefs.fan_ids, id]
        : store.prefs.fan_ids.filter((x) => x !== id);
      if (ids.length === 0 || ids.length > 2) return draw();
      change({ fan_display: "selected", fan_ids: ids.sort((a, b) => a - b) });
    }
  });

  // Clicking the dimmed page does nothing (MangoDisk: interact-outside is
  // prevented) — only Done / Escape close, so a stray click cannot lose focus.
  // Re-render on settled writes too (saving → saved / failed).
  const unsubscribe = store.subscribe(draw);
  draw();
  (dialog.querySelector<HTMLElement>("#mbset-compact") ?? dialog).focus();
}

// --- color rule editor ----------------------------------------------------------

/** Editable copy of a rule; thresholds stay null while the field is empty. */
interface DraftRule {
  key: number;
  condition: Condition;
  a: number | null;
  b: number | null;
  tone: Tone;
}

/** Open the stacked "<metric> 颜色" editor; `onClose` runs after it closes. */
function openRuleEditor(store: PrefsStore, id: MetricId, onClose: () => void): void {
  const { backdrop, dialog } = mountDialog("mbset-stack", "mbrule-title");
  dialog.classList.add("mbrule");
  const name = metricName(id);
  const unit = ruleUnit(id);
  const limit = ruleLimit(id);
  let nextKey = 0;
  const editable = (r: ColorRule): DraftRule => ({
    key: nextKey++,
    condition: r.condition,
    a: r.a,
    b: r.condition === "range" ? r.b : null,
    tone: r.tone,
  });
  let rules: DraftRule[] = (store.prefs.metrics.find((m) => m.id === id)?.rules ?? []).map(editable);
  let saving = false;
  let failed = false;

  /** A whole number inside the metric's range. */
  const whole = (v: number | null): v is number =>
    v !== null && Number.isInteger(v) && v >= 0 && v <= limit;

  /** Half-open interval a rule covers, or null when its numbers are invalid. */
  const bounds = (r: DraftRule): [number, number] | null => {
    if (!whole(r.a)) return null;
    if (r.condition === "below") return r.a > 0 ? [0, r.a] : null;
    if (r.condition === "at_least") return [r.a, Infinity];
    return whole(r.b) && r.b > r.a ? [r.a, r.b] : null;
  };

  /** First validation problem, or null when the rules can be saved. */
  const validation = (): string | null => {
    const spans = rules.map(bounds);
    if (spans.some((s) => s === null)) {
      // Symbol units hug the number ("100%", "150°C"); word units take a space.
      const spaced = unit === "%" || unit === "°C" ? unit : ` ${unit}`;
      return t("mb.rule.invalid", { max: limit, unit: spaced });
    }
    for (let i = 0; i < spans.length; i++) {
      for (let j = i + 1; j < spans.length; j++) {
        const [a, b] = [spans[i]!, spans[j]!];
        if (a[0] < b[1] && b[0] < a[1]) return t("mb.rule.overlap", { first: i + 1, second: j + 1 });
      }
    }
    if (rules.length > MAX_RULES) return t("mb.rule.limit", { max: MAX_RULES });
    return null;
  };

  /** Markup for one rule card. */
  const ruleRow = (r: DraftRule, i: number): string => {
    const n = i + 1;
    const numberField = (field: "a" | "b", label: string) =>
      `<input type="number" class="mbrule-num" id="mbrule-${field}-${r.key}" data-key="${r.key}" data-field="${field}"
         min="0" max="${limit}" step="1" inputmode="numeric" value="${r[field] ?? ""}"
         aria-label="${esc(label)}" ${saving ? "disabled" : ""} />`;
    return `<div class="mbrule-row" data-key="${r.key}">
      <div class="mbrule-row-head">
        <span class="mbrule-index">${t("mb.rule.index", { index: n })}</span>
        <button class="link-quiet mbrule-remove" id="mbrule-del-${r.key}" data-remove="${r.key}"
          aria-label="${esc(t("mb.rule.removeAria", { index: n }))}" ${saving ? "disabled" : ""}>${t("mb.rule.remove")}</button>
      </div>
      <div class="mbrule-fields">
        <select class="mbrule-cond" id="mbrule-cond-${r.key}" data-key="${r.key}" aria-label="${esc(t("mb.rule.condition"))}" ${saving ? "disabled" : ""}>
          ${CONDITIONS.map((c) => `<option value="${c}" ${c === r.condition ? "selected" : ""}>${t(`mb.rule.${c}`)}</option>`).join("")}
        </select>
        ${numberField("a", t("mb.rule.value", { index: n }))}
        ${r.condition === "range" ? `<span class="mbrule-to">${t("mb.rule.to")}</span>${numberField("b", t("mb.rule.upper", { index: n }))}` : ""}
        <span class="mbrule-unit">${unit}</span>
        <div class="mbrule-tones" role="radiogroup" aria-label="${esc(t("mb.rule.tone", { index: n }))}">
          ${TONES.map(
            (tone) =>
              `<button class="mbrule-tone ${tone === r.tone ? "on" : ""}" id="mbrule-tone-${r.key}-${tone}" role="radio"
                 aria-checked="${tone === r.tone}" tabindex="${tone === r.tone ? 0 : -1}" data-key="${r.key}" data-tone="${tone}"
                 title="${esc(t(`mb.tone.${tone}`))}" aria-label="${esc(t(`mb.tone.${tone}`))}" ${saving ? "disabled" : ""}>
                 <i data-tone="${tone}"></i></button>`,
          ).join("")}
        </div>
      </div>
    </div>`;
  };

  /** Full editor markup. */
  const markup = (): string => {
    const problem = validation();
    return `
      <header class="mbset-head">
        <h2 id="mbrule-title">${esc(t("mb.rule.title", { name }))}</h2>
      </header>
      <div class="mbset-body">
        <p class="mbrule-note" id="mbrule-desc">${t("mb.rule.fallback")}</p>
        ${store.prefs.usage_colors ? "" : `<p class="mbrule-note mbrule-off">${t("mb.rule.off")}</p>`}
        ${rules.length === 0 ? `<p class="mbrule-empty">${t("mb.rule.empty")}</p>` : ""}
        <div class="mbrule-list">${rules.map(ruleRow).join("")}</div>
        <p class="mbrule-error" role="alert" id="mbrule-error">${problem ? esc(problem) : ""}</p>
        <div class="mbrule-addline">
          <button id="mbrule-add" ${saving || rules.length >= MAX_RULES ? "disabled" : ""}>+ ${t("mb.rule.add")}</button>
          ${rules.length >= MAX_RULES ? `<span class="muted">${t("mb.rule.limit", { max: MAX_RULES })}</span>` : ""}
        </div>
      </div>
      <footer class="mbset-foot">
        <button class="link-quiet" id="mbrule-restore" ${saving ? "disabled" : ""}>${t("mb.rule.restore")}</button>
        <span class="mbset-status" role="status">${
          failed ? `<span class="mbset-err">${t("mb.saveFailed")}</span>` : saving ? `<span class="muted">${t("mb.saving")}</span>` : ""
        }</span>
        <button id="mbrule-cancel" ${saving ? "disabled" : ""}>${t("mb.rule.cancel")}</button>
        <button class="mbset-primary" id="mbrule-save" ${saving || problem ? "disabled" : ""}>${t("mb.rule.save")}</button>
      </footer>`;
  };

  /** Re-render, keeping focus on the same control. */
  const draw = () => renderKeepingFocus(dialog, markup());

  /** Refresh only the error line and Save state (keeps the caret in place
   * while typing a threshold). */
  const revalidate = () => {
    const problem = validation();
    dialog.querySelector("#mbrule-error")!.textContent = problem ?? "";
    (dialog.querySelector("#mbrule-save") as HTMLButtonElement).disabled = saving || problem !== null;
  };

  /** Close and hand control back to the configure dialog. */
  const close = () => {
    if (!backdrop.isConnected) return;
    openDialogs.splice(openDialogs.indexOf(close), 1);
    backdrop.remove();
    document.removeEventListener("keydown", onKey, true);
    onClose();
  };
  openDialogs.push(close);

  /** Escape cancels (not while saving); Tab stays inside; arrows walk tones. */
  function onKey(ev: KeyboardEvent): void {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      if (!saving) close();
      return;
    }
    const tone = (ev.target as HTMLElement).closest<HTMLElement>(".mbrule-tone");
    if (tone && ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(ev.key)) {
      ev.preventDefault();
      const step = ev.key === "ArrowLeft" || ev.key === "ArrowUp" ? -1 : 1;
      const next = TONES[(TONES.indexOf(tone.dataset.tone as Tone) + step + TONES.length) % TONES.length];
      const rule = rules.find((r) => r.key === Number(tone.dataset.key));
      if (rule) {
        rule.tone = next;
        draw();
        document.getElementById(`mbrule-tone-${rule.key}-${next}`)?.focus();
      }
      return;
    }
    trapTab(dialog, ev);
  }
  document.addEventListener("keydown", onKey, true);

  /** Persist the edited rules into this metric's entry. */
  const save = async () => {
    if (saving || validation()) return;
    saving = true;
    failed = false;
    draw();
    const out: ColorRule[] = rules.map((r) => ({
      condition: r.condition,
      a: r.a!,
      b: r.condition === "range" ? r.b : null,
      tone: r.tone,
    }));
    const ok = await store.save({
      ...store.prefs,
      metrics: store.prefs.metrics.map((m) => (m.id === id ? { ...m, rules: out } : m)),
    });
    saving = false;
    if (ok) return close();
    failed = true;
    draw();
  };

  dialog.addEventListener("click", (ev) => {
    const el = ev.target as HTMLElement;
    if (el.closest("#mbrule-cancel")) return close();
    if (el.closest("#mbrule-save")) return void save();
    if (el.closest("#mbrule-restore")) {
      rules = defaultRules(id).map(editable);
      return draw();
    }
    if (el.closest("#mbrule-add")) {
      if (rules.length >= MAX_RULES) return;
      const rule: DraftRule = { key: nextKey++, condition: "at_least", a: null, b: null, tone: "critical" };
      rules.push(rule);
      draw();
      document.getElementById(`mbrule-a-${rule.key}`)?.focus();
      return;
    }
    const remove = el.closest<HTMLElement>("[data-remove]");
    if (remove) {
      const idx = rules.findIndex((r) => r.key === Number(remove.dataset.remove));
      rules.splice(idx, 1);
      draw();
      // Focus the next rule's delete button, else Add rule.
      const next = rules[idx] ?? rules[idx - 1];
      (document.getElementById(next ? `mbrule-del-${next.key}` : "mbrule-add") as HTMLElement | null)?.focus();
      return;
    }
    const tone = el.closest<HTMLElement>(".mbrule-tone");
    if (tone) {
      const rule = rules.find((r) => r.key === Number(tone.dataset.key));
      if (rule) rule.tone = tone.dataset.tone as Tone;
      draw();
      document.getElementById(tone.id)?.focus();
    }
  });

  dialog.addEventListener("input", (ev) => {
    const input = ev.target as HTMLInputElement;
    if (!input.classList.contains("mbrule-num")) return;
    const rule = rules.find((r) => r.key === Number(input.dataset.key));
    if (!rule) return;
    const raw = input.value.trim();
    const value = raw === "" ? null : Number(raw);
    rule[input.dataset.field as "a" | "b"] = value !== null && Number.isFinite(value) ? value : null;
    revalidate();
  });

  dialog.addEventListener("change", (ev) => {
    const select = ev.target as HTMLSelectElement;
    if (!select.classList.contains("mbrule-cond")) return;
    const rule = rules.find((r) => r.key === Number(select.dataset.key));
    if (!rule) return;
    rule.condition = select.value as Condition;
    if (rule.condition !== "range") rule.b = null;
    draw();
  });

  draw();
  dialog.setAttribute("aria-describedby", "mbrule-desc");
  (dialog.querySelector<HTMLElement>(".mbrule-cond") ?? dialog.querySelector<HTMLElement>("#mbrule-add"))?.focus();
}
