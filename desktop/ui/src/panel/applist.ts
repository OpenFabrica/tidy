// Application list (C7) for the CPU and Memory tabs: processes grouped by
// app, a persisted sort header, expandable rows with "Open location" and a
// normal "Quit". Rows are keyed and patched in place each second; the order
// re-sorts with every sample except while a row is expanded, so the row the
// user is reading never jumps away from the pointer.

import { h, icon, setAttr, setIcon, setText, show } from "./dom";
import * as fmt from "./fmt";
import type { Widget } from "./widget";
import { t } from "../i18n";
import { appIcon, residentQuitApp, revealInFinder } from "../ipc";
import type { AppUsage, ResidentReading } from "../types";

type Metric = "cpu" | "memory";
type Column = "name" | "usage";
type Direction = "asc" | "desc";
interface Sort {
  column: Column;
  direction: Direction;
}
type QuitState = "idle" | "requesting" | "requested" | "unavailable" | "unsupported" | "failed";

/** How long an empty list after a tab switch or panel show reads as loading. */
const LOADING_GRACE_MS = 4000;

/** Icons are fetched once per bundle for the panel's lifetime. */
const iconCache = new Map<string, Promise<string | null>>();

/** Cached data-URL icon for an app bundle. */
function loadIcon(path: string): Promise<string | null> {
  let p = iconCache.get(path);
  if (!p) {
    p = appIcon(path).catch(() => null);
    iconCache.set(path, p);
  }
  return p;
}

/** Read the persisted sort for a metric (usage descending by default). */
function loadSort(metric: Metric): Sort {
  try {
    const raw = JSON.parse(localStorage.getItem(`tidy-panel-sort-${metric}`) ?? "null");
    if (
      raw &&
      (raw.column === "name" || raw.column === "usage") &&
      (raw.direction === "asc" || raw.direction === "desc")
    ) {
      return raw as Sort;
    }
  } catch {
    /* fall through to the default */
  }
  return { column: "usage", direction: "desc" };
}

/** Usage value an app is sorted and scaled by. */
function usageOf(app: AppUsage, metric: Metric): number {
  return metric === "cpu" ? app.cpu_percent : app.memory_bytes;
}

/** Formatted usage cell text. */
function usageText(app: AppUsage, metric: Metric): string {
  return metric === "cpu" ? `${app.cpu_percent.toFixed(1)}%` : fmt.memory(app.memory_bytes);
}

/** Sort a copy of the apps; ties break by name so the order is stable. */
function sorted(apps: AppUsage[], metric: Metric, sort: Sort): AppUsage[] {
  const dir = sort.direction === "asc" ? 1 : -1;
  const byName = (a: AppUsage, b: AppUsage) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true });
  return [...apps].sort((a, b) => {
    const primary = sort.column === "name" ? byName(a, b) : usageOf(a, metric) - usageOf(b, metric);
    return primary !== 0 ? primary * dir : byName(a, b);
  });
}

/** One rendered row and its mutable state. */
interface Row {
  li: HTMLLIElement;
  app: AppUsage;
  available: boolean;
  quit: QuitState;
  update(app: AppUsage, available: boolean, share: number): void;
  setExpanded(open: boolean): void;
}

/** Build the list widget for the CPU or Memory tab. */
export function appList(metric: Metric): Widget {
  let sort = loadSort(metric);
  let expandedKey: string | null = null;
  /** Display order while a row is expanded (frozen). */
  let order: string[] = [];
  let latest: AppUsage[] = [];
  /** The backend needs one baseline sample (~1 s) before a list exists, on
   * every tab switch and every panel show; until a non-empty list arrives or
   * LOADING_GRACE_MS pass, an empty list means "still loading", not "none". */
  let filled = false;
  let waitingSince = performance.now();
  const rows = new Map<string, Row>();
  const columnLabel = metric === "cpu" ? "CPU" : t("rp.tab.memory");

  const cachedChip = h("span", { class: "chip cached", tabindex: 0, title: t("rp.previousSample") }, t("rp.cached"));
  const sortButtons = (["name", "usage"] as Column[]).map((column) => {
    const arrow = icon("chevronDown", 11, "sort-arrow");
    const btn = h(
      "button",
      { type: "button", onclick: () => chooseSort(column) },
      column === "name" ? t("rp.appName") : columnLabel,
      arrow,
    );
    return { column, btn, arrow };
  });
  const head = h(
    "div",
    { class: "sort-head", role: "group", "aria-label": t("rp.appName") },
    h("div", { class: "sort-col" }, sortButtons[0].btn, cachedChip),
    sortButtons[1].btn,
  );
  const empty = h("p", { class: "list-empty", role: "status" });
  const ol = h("ol");
  const viewport = h("div", { class: "list-viewport", tabindex: 0, "aria-label": t("rp.appName") }, ol);
  const el = h("section", { class: "apps", "aria-label": t("rp.appName") }, head, empty, viewport);

  // Icons load only for rows scrolled into view; a long list stays cheap.
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const li = entry.target as HTMLLIElement;
        observer.unobserve(li);
        const path = rows.get(li.dataset.key ?? "")?.app.bundle_path;
        const holder = li.querySelector<HTMLElement>(".app-icon");
        if (!path || !holder) continue;
        void loadIcon(path).then((url) => {
          if (url && holder.isConnected) holder.replaceChildren(h("img", { src: url, alt: "" }));
        });
      }
    },
    { root: viewport, rootMargin: "120px 0px" },
  );

  /** Paint the sort header's pressed state and arrows. */
  const paintSort = () => {
    for (const { column, btn, arrow } of sortButtons) {
      const active = sort.column === column;
      setAttr(btn, "aria-pressed", String(active));
      setIcon(arrow, active && sort.direction === "asc" ? "chevronUp" : "chevronDown");
      arrow.classList.toggle("inactive", !active);
      const next: Direction = active ? (sort.direction === "asc" ? "desc" : "asc") : column === "name" ? "asc" : "desc";
      setAttr(
        btn,
        "aria-label",
        t("rp.sortBy", {
          column: column === "name" ? t("rp.appName") : columnLabel,
          direction: t(next === "asc" ? "rp.sortAsc" : "rp.sortDesc"),
        }),
      );
    }
  };

  /** Header click: toggle direction on the active column, else switch. */
  const chooseSort = (column: Column) => {
    sort =
      sort.column === column
        ? { column, direction: sort.direction === "asc" ? "desc" : "asc" }
        : { column, direction: column === "name" ? "asc" : "desc" };
    localStorage.setItem(`tidy-panel-sort-${metric}`, JSON.stringify(sort));
    paintSort();
    // A deliberate re-sort unfreezes the order even while a row is open.
    order = sorted(latest, metric, sort).map((a) => a.key);
    render();
  };

  /** Toggle one row open; opening another closes the previous one. */
  const toggle = (key: string) => {
    const previous = expandedKey;
    expandedKey = previous === key ? null : key;
    if (previous && previous !== key) rows.get(previous)?.setExpanded(false);
    rows.get(key)?.setExpanded(expandedKey === key);
    if (expandedKey === null) render();
  };

  /** Create a row for an app. */
  const makeRow = (app: AppUsage): Row => {
    const detailsId = `app-${metric}-${rows.size}-${Date.now()}`;
    const share = h("span", { class: "share", "aria-hidden": "true" });
    const iconHolder = h("span", { class: "app-icon" }, icon("app", 18));
    const name = h("span", { class: "app-name-text" });
    const pcount = h("span", { class: "pcount" });
    const partial = h("span", { class: "badge" }, t("rp.partialData"));
    const value = h("strong", { class: "app-val" });
    const disclosure = icon("chevronDown", 11, "disclosure");
    const button = h(
      "button",
      { type: "button", class: "app-row", "aria-expanded": "false", onclick: () => toggle(row.app.key) },
      share,
      iconHolder,
      h("span", { class: "app-name" }, name, pcount),
      partial,
      value,
      disclosure,
    );
    const status = h("span", { role: "status" });
    const ident = h("span", { class: "mono" });
    const path = h("span", { class: "path" });
    const pids = h("span", { class: "pids mono" });
    const failed = h("span", { role: "alert", class: "alert" }, t("rp.revealFailed"));
    const reveal = h(
      "button",
      {
        type: "button",
        class: "act",
        title: t("rp.revealHint"),
        onclick: () => {
          const target = row.app.bundle_path ?? row.app.executable;
          if (!target) return;
          show(failed, false);
          void revealInFinder(target).catch(() => show(failed, true));
        },
      },
      icon("folder", 12),
      t("rp.reveal"),
    );
    const quitIcon = icon("power", 12);
    const quitLabel = h("span", { role: "status", "aria-atomic": "true" });
    const quit = h("button", { type: "button", class: "act quit", onclick: () => void requestQuit() }, quitIcon, quitLabel);
    const details = h(
      "div",
      { class: "app-details", id: detailsId },
      status,
      ident,
      path,
      pids,
      h("div", { class: "app-actions" }, reveal, quit),
      failed,
    );
    details.hidden = true;
    show(failed, false);
    const li = h("li", { "data-key": app.key }, button, details);

    /** Paint the quit button for its current state. */
    const paintQuit = () => {
      const s = row.quit;
      const label =
        s === "idle" ? t("rp.quitApp.action") : t(`rp.quitApp.${s}`);
      setText(quitLabel, label);
      setIcon(quitIcon, s === "requesting" ? "refresh" : s === "requested" ? "check" : "power");
      quitIcon.classList.toggle("spin", s === "requesting");
      quit.disabled = !row.available || s === "requesting";
      setAttr(quit, "aria-busy", s === "requesting" ? "true" : null);
      setAttr(quit, "title", s === "idle" ? t("rp.quitApp.hint") : t(`rp.quitApp.${s}Hint`));
      setAttr(quit, "aria-label", t("rp.quitApp.named", { name: row.app.name }));
    };

    /** Ask the app to quit normally; map backend codes to button states. */
    const requestQuit = async () => {
      row.quit = "requesting";
      paintQuit();
      try {
        await residentQuitApp(row.app.pid);
        row.quit = "requested";
      } catch (err) {
        const code = (err as { code?: string } | null)?.code;
        row.quit = code === "unavailable" ? "unavailable" : code === "unsupported" ? "unsupported" : "failed";
      }
      paintQuit();
    };

    const row: Row = {
      li,
      app,
      available: true,
      quit: "idle",
      update(next, available, pct) {
        row.app = next;
        row.available = available;
        share.style.width = `${pct.toFixed(1)}%`;
        setText(name, next.name);
        setAttr(button, "title", next.name);
        setText(pcount, next.process_count > 1 ? ` (${next.process_count})` : "");
        show(partial, next.partial);
        setText(value, usageText(next, metric));
        li.classList.toggle("gone", !available);
        if (details.hidden) return;
        setText(status, available ? (next.partial ? t("rp.partialMemory") : "") : t("rp.appUnavailable"));
        show(status, !available || next.partial);
        setText(
          ident,
          next.process_count > 1 ? t("rp.processCount", { count: next.process_count }) : t("rp.processId", { pid: next.pid }),
        );
        setText(path, next.bundle_path ?? next.executable ?? t("rp.locationUnavailable"));
        const others = next.pids.length > 1 ? next.pids.slice(0, 12).join(", ") + (next.pids.length > 12 ? " …" : "") : "";
        setText(pids, others ? `PID ${others}` : "");
        show(pids, others !== "");
        reveal.disabled = !available || !(next.bundle_path ?? next.executable);
        show(quit, next.quittable);
        paintQuit();
      },
      setExpanded(open) {
        details.hidden = !open;
        setAttr(button, "aria-expanded", String(open));
        setAttr(button, "aria-controls", open ? detailsId : null);
        setIcon(disclosure, open ? "chevronUp" : "chevronDown");
        li.classList.toggle("open", open);
        if (!open) {
          row.quit = row.quit === "requesting" ? row.quit : "idle";
          show(failed, false);
        }
        row.update(row.app, row.available, parseFloat(share.style.width) || 0);
      },
    };
    observer.observe(li);
    return row;
  };

  /** Reconcile rows with `latest` in the current (or frozen) order. */
  const render = () => {
    const byKey = new Map(latest.map((a) => [a.key, a]));
    const fresh = sorted(latest, metric, sort).map((a) => a.key);
    if (expandedKey === null) {
      order = fresh;
    } else {
      // Frozen: keep known rows in place (the expanded one even if it left
      // the sample), drop vanished others, append newcomers at the end.
      const kept = order.filter((k) => byKey.has(k) || k === expandedKey);
      const known = new Set(kept);
      order = [...kept, ...fresh.filter((k) => !known.has(k))];
    }
    const largest = Math.max(0, ...latest.map((a) => usageOf(a, metric)));
    order.forEach((key, index) => {
      const app = byKey.get(key);
      let row = rows.get(key);
      if (!row) {
        if (!app) return;
        row = makeRow(app);
        rows.set(key, row);
      }
      const data = app ?? row.app;
      row.update(data, !!app, app && largest > 0 ? (usageOf(app, metric) / largest) * 100 : 0);
      if (ol.children[index] !== row.li) ol.insertBefore(row.li, ol.children[index] ?? null);
    });
    const wanted = new Set(order);
    for (const [key, row] of rows) {
      if (wanted.has(key)) continue;
      observer.unobserve(row.li);
      row.li.remove();
      rows.delete(key);
    }
    const hasRows = order.length > 0;
    show(viewport, hasRows);
    show(empty, !hasRows);
    if (!hasRows) {
      const loading = waiting();
      setText(empty, t(loading ? "rp.loadingApps" : "rp.noApps"));
    }
  };

  /** True while an empty list should still read as loading. */
  const waiting = () => !filled && performance.now() - waitingSince < LOADING_GRACE_MS;

  paintSort();
  show(viewport, false);
  setText(empty, t("rp.loadingApps"));
  return {
    el,
    update(r: ResidentReading) {
      const incoming = metric === "cpu" ? r.apps_cpu : r.apps_memory;
      if (incoming.length) filled = true;
      // During the baseline gap keep the rows from the previous visit rather
      // than flashing an empty list.
      if (incoming.length || !waiting()) latest = incoming;
      show(cachedChip, r.stale && latest.length > 0);
      render();
    },
    resume() {
      filled = false;
      waitingSince = performance.now();
    },
    dispose() {
      observer.disconnect();
    },
  };
}
