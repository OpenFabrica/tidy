// Apps view (newspaper skin): 卸载 / 更新 / 启动项 set as serif section words
// under the masthead.
//   卸载   — a three-column spread: the 名录 (directory of installed apps),
//            the selected app's dossier, and a "建议关注" rail.
//   更新   — an update ledger with a 来源诊断 rail.
//   启动项 — toggleable launchd items over read-only app-embedded helpers.
// Performance contract: the directory renders ONCE per structural change;
// checkbox toggles patch only the affected row summary and the footer.

import {
  appIcon,
  cachedAppUpdates,
  cancelTask,
  listApps,
  listAppUpdates,
  listEmbeddedLoginItems,
  listLoginItems,
  newTaskId,
  planUninstall,
  previewUninstall,
  refreshAppCache,
  revealInFinder,
  runAppUpdates,
  setAppUpdateIgnored,
  setLoginItemEnabled,
} from "../ipc";
import { confirmSheet, runExecution } from "../flow";
import { esc, humanKb } from "../format";
import { t } from "../i18n";
import { setNavMeta } from "../router";
import { stopwatch, track } from "../telemetry";
import type { View } from "../router";
import type {
  AppInfo,
  AppUpdate,
  EmbeddedLoginItem,
  LoginItem,
  PlanItem,
  UninstallNote,
  UpdateCatalog,
  UpdateResult,
} from "../types";

/** Client-side icon cache: app path → data URL (null = known miss). */
const iconCache = new Map<string, string | null>();

/** Leftover detail cache: app path → items + notes. */
interface Detail {
  items: PlanItem[];
  notes: UninstallNote[];
}
const detailCache = new Map<string, Detail>();

/** 13px circular-arrow glyph for the Apps toolbar refresh link. */
const REFRESH_SVG =
  '<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px"><path d="M14 8a6 6 0 1 1-1.76-4.24"/><path d="M14 2v4h-4"/></svg>';

/** Selection state: app path → checked item PATHS (stable across re-plans). */
const selected = new Map<string, Set<string>>();
/** Bundle size per app path (from list_apps) — the footer's provisional
 * total while an app's leftover scan is still running. */
const appSizes = new Map<string, number>();
/** The app whose dossier fills the middle column (path, or "" for none). */
let focused = "";

type SubTab = "uninstall" | "updates" | "login";
let subTab: SubTab = "uninstall";
let sortKey: "name" | "size" = "size";
let query = "";
let updateTaskId: string | null = null;
let lastUpdateResults: UpdateResult[] = [];

export const uninstall: View = {
  mount(container) {
    selected.clear();
    focused = "";
    void renderShell(container);
  },
};

/** Shell: the serif sub-tab row + the active section's body + the sticky
 * selection bar (tabloid layout: everything sits inside one .page). */
async function renderShell(container: HTMLElement): Promise<void> {
  container.innerHTML = `
    <div class="page apps-page">
      <nav class="subtabs" id="subtabs">
        ${(["uninstall", "updates", "login"] as SubTab[])
          .map(
            (tab) =>
              `<button data-subtab="${tab}" class="subtab ${tab === subTab ? "on" : ""}">${t(
                `apps.tab.${tab}`,
              )}</button>`,
          )
          .join("")}
        <span class="extra" id="tab-extra"></span>
      </nav>
      <div id="tab-body"><div class="placeholder">…</div></div>
      <div class="selbar" id="footer" style="display:none"></div>
    </div>`;

  container.querySelectorAll<HTMLButtonElement>("button[data-subtab]").forEach((btn) => {
    btn.addEventListener("click", () => {
      if (updateTaskId) void cancelTask(updateTaskId);
      subTab = btn.dataset.subtab as SubTab;
      void renderShell(container);
    });
  });

  const body = container.querySelector<HTMLElement>("#tab-body")!;
  if (subTab === "uninstall") await renderUninstall(container, body);
  else if (subTab === "updates") await renderUpdates(body);
  else await renderLogin(body);
}

// ---------------------------------------------------------------------------
// 卸载
// ---------------------------------------------------------------------------

/** Uninstall spread: 名录 · dossier · 建议关注. */
async function renderUninstall(container: HTMLElement, body: HTMLElement): Promise<void> {
  body.innerHTML = `<div class="placeholder">${t("apps.loading")}</div>`;
  const apps = await listApps(newTaskId());
  for (const app of apps) appSizes.set(app.path, app.size_kb);
  const totalKb = apps.reduce((sum, app) => sum + app.size_kb, 0);
  setNavMeta(`${apps.length} ${t("apps.installed")} · ${humanKb(totalKb)}`);

  const extra = container.querySelector<HTMLElement>("#tab-extra")!;
  // "建议关注" without last-used data would be a guess, so the rail reports
  // the one thing the inventory really knows: the heaviest bundles.
  const heaviest = [...apps].sort((a, b) => b.size_kb - a.size_kb).slice(0, 3);

  // Tabloid spread: B1 名录 · B2 dossier · B3 建议关注 on the shared
  // .grid.apps (tabloid.css folds it to two columns + a full-width rail).
  body.innerHTML = `
    <div class="grid apps">
      <section class="mod">
        <div class="kick">
          <span class="code">B1</span><span class="t">${t("apps.directory")}</span>
          <span class="aside sorts">
            <a href="#" data-sort="name" class="${sortKey === "name" ? "on" : ""}">${t("apps.sort.name")} ⇅</a>
            <a href="#" data-sort="size" class="${sortKey === "size" ? "on" : ""}">${t("apps.sort.size")} ⇅</a>
          </span>
        </div>
        <label class="search"><span aria-hidden="true">⌕</span>
          <input type="search" id="app-search" placeholder="${t("apps.search")}" value="${esc(query)}" />
        </label>
        <div id="app-list"></div>
      </section>
      <article class="mod dossier" id="app-detail"></article>
      <div class="side">
        <aside class="mod">
          <div class="kick"><span class="code">B3</span><span class="t">${t("apps.watch")}</span></div>
          <p class="prose sm">${t("apps.watch.sub", {
            n: heaviest.length,
            size: humanKb(heaviest.reduce((sum, a) => sum + a.size_kb, 0)),
          })}</p>
          <ul class="kv watch">
            ${heaviest
              .map(
                (app) => `<li>
                  <a class="l" href="#" data-focus="${esc(app.path)}">${esc(app.name)}</a>
                  <span class="dots"></span>
                  <span class="v">${humanKb(app.size_kb)}</span>
                </li>`,
              )
              .join("")}
          </ul>
          <p class="note watch-note">${t("apps.watch.note")}</p>
        </aside>
      </div>
    </div>`;

  extra.innerHTML = `<a href="#" class="link mono plain" data-refresh="1" title="${t("apps.refresh.hint")}">${REFRESH_SVG} ${t("apps.refresh")}</a>`;
  // Manual refresh: clear both the backend caches and the client-side icon /
  // detail maps, then re-enter renderUninstall so the list is rebuilt from a
  // fresh scan. Clearing the client maps matters as much as the backend call
  // — a cached null icon here would otherwise never be re-requested.
  extra.querySelector<HTMLAnchorElement>("a[data-refresh]")!.addEventListener(
    "click",
    async (ev) => {
      ev.preventDefault();
      const link = ev.currentTarget as HTMLAnchorElement;
      link.textContent = t("apps.refreshing");
      link.style.pointerEvents = "none";
      try {
        await refreshAppCache();
      } finally {
        iconCache.clear();
        detailCache.clear();
        await renderUninstall(container, body);
      }
    },
  );

  const list = body.querySelector<HTMLElement>("#app-list")!;

  /** Rebuild the directory column for the current sort + search. */
  const drawList = () => {
    const filtered = apps.filter((app) =>
      query === "" ? true : app.name.toLowerCase().includes(query.toLowerCase()),
    );
    const sorted = filtered.sort((a, b) =>
      sortKey === "size" ? b.size_kb - a.size_kb : a.name.localeCompare(b.name),
    );
    list.innerHTML =
      sorted.length === 0
        ? `<div class="placeholder">${t("apps.noMatch")}</div>`
        : sorted.map((app) => rowHtml(app)).join("");
    sorted.forEach((app) => wireRow(container, list, app));
    void loadIcons(list, sorted);
    updateFooter(container);
  };

  body.querySelectorAll<HTMLAnchorElement>("a[data-sort]").forEach((a) => {
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      sortKey = a.dataset.sort as "name" | "size";
      // Visual only: the active sort reads as ink, the other stays a link.
      body
        .querySelectorAll<HTMLAnchorElement>("a[data-sort]")
        .forEach((other) => other.classList.toggle("on", other === a));
      drawList();
    });
  });
  const search = body.querySelector<HTMLInputElement>("#app-search")!;
  search.addEventListener("input", () => {
    query = search.value;
    drawList();
  });
  body.querySelectorAll<HTMLAnchorElement>("a[data-focus]").forEach((a) => {
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      const app = apps.find((candidate) => candidate.path === a.dataset.focus);
      if (app) void focusApp(container, app);
    });
  });

  drawList();
  // Open the heaviest app so the middle column is never an empty rectangle.
  const initial = apps.find((app) => app.path === focused) ?? heaviest[0];
  if (initial) void focusApp(container, initial);
}

/** Static HTML for one directory row: checkbox | icon | name · meta · picked | size. */
function rowHtml(app: AppInfo): string {
  const blocked = app.protected || app.official_uninstaller !== null;
  const why = app.protected
    ? t("apps.protected")
    : app.official_uninstaller
      ? `${t("apps.official")} (${app.official_uninstaller})`
      : "";
  return `<div class="app-row ${app.path === focused ? "current" : ""}" data-row="${esc(app.path)}">
    <div class="head">
      <input type="checkbox" data-app="${esc(app.path)}" ${blocked ? "disabled" : ""}
        title="${esc(why || t("apps.selectHint"))}" />
      <span class="icon" data-avatar="${esc(app.path)}">${esc(app.name.slice(0, 1).toUpperCase())}</span>
      <span class="app-main">
        <b class="name">${esc(app.name)}</b>
        <span class="meta">${esc(app.version || "—")}${app.running ? ` · ${t("apps.active")}` : ""}${
          why ? ` · ${esc(why)}` : ""
        }</span>
        <span class="picked" data-sum="${esc(app.path)}"></span>
      </span>
      <span class="num">${humanKb(app.size_kb)}</span>
    </div>
  </div>`;
}

/** Wire one row: click focuses the dossier, the checkbox marks for removal. */
function wireRow(container: HTMLElement, list: HTMLElement, app: AppInfo): void {
  const row = list.querySelector<HTMLElement>(`[data-row="${cssEscape(app.path)}"]`)!;
  const checkbox = row.querySelector<HTMLInputElement>("input[data-app]")!;
  checkbox.checked = selected.has(app.path);
  row.classList.toggle("selected", selected.has(app.path));

  row.querySelector<HTMLElement>(".head")!.addEventListener("click", (ev) => {
    if ((ev.target as HTMLElement).tagName === "INPUT") return;
    void focusApp(container, app);
  });

  checkbox.addEventListener("change", async () => {
    row.classList.toggle("selected", checkbox.checked);
    if (!checkbox.checked) {
      selected.delete(app.path);
      patchRowSummary(container, app.path);
      if (focused === app.path) void focusApp(container, app);
      updateFooter(container);
      return;
    }
    // Optimistic: mark the app selected right away (the bundle itself is
    // always part of the plan) so the row and footer react instantly; the
    // leftover scan fills in the full item set when it returns.
    selected.set(app.path, new Set([app.path]));
    patchRowSummary(container, app.path);
    updateFooter(container);
    const detail = await loadDetail(app.path);
    // The user may have unchecked while the scan ran — do not resurrect.
    if (!checkbox.checked) return;
    selected.set(app.path, new Set(detail.items.map((i) => i.path)));
    patchRowSummary(container, app.path);
    if (focused === app.path) void focusApp(container, app);
    updateFooter(container);
  });
}

/** Move an app into the dossier column and render it (scan on first open). */
async function focusApp(container: HTMLElement, app: AppInfo): Promise<void> {
  focused = app.path;
  container
    .querySelectorAll<HTMLElement>(".app-row")
    .forEach((row) => row.classList.toggle("current", row.dataset.row === app.path));
  const el = container.querySelector<HTMLElement>("#app-detail");
  if (!el) return;
  const cached = detailCache.get(app.path);
  if (!cached) {
    el.innerHTML = `
      ${dossierKick()}
      <h1 class="dossier-name">${esc(app.name)}</h1>
      <div class="placeholder">${t("apps.scanning")}</div>`;
  }
  const detail = cached ?? (await loadDetail(app.path));
  // A slower scan must not overwrite a dossier the user has since moved off.
  if (focused !== app.path || !el.isConnected) return;
  renderDossier(container, el, app, detail);
}

/** The dossier column's kicker: B2 · 档案. */
function dossierKick(): string {
  return `<div class="kick"><span class="code">B2</span><span class="t">${t("apps.dossier")}</span></div>`;
}

/** Fetch (once) the leftover detail for an app. */
async function loadDetail(appPath: string): Promise<Detail> {
  const cached = detailCache.get(appPath);
  if (cached) return cached;
  const payload = await previewUninstall([appPath], newTaskId());
  const detail: Detail = {
    items: payload.summary.sections.flatMap((s) => s.items),
    notes: payload.notes,
  };
  detailCache.set(appPath, detail);
  return detail;
}

/** Leftover category derived from a path (display grouping only). */
function categoryOf(path: string): string {
  if (path.endsWith(".app")) return t("cat.app");
  if (path.includes("/Application Support/")) return t("cat.appsupport");
  if (path.includes("/Caches/")) return t("cat.cache");
  if (path.includes("/Logs/")) return t("cat.logs");
  if (path.includes("/Preferences/")) return t("cat.prefs");
  if (path.includes("/LaunchAgents/")) return t("cat.launchagents");
  if (path.includes("/HTTPStorages/")) return t("cat.httpstorage");
  if (path.includes("/WebKit/")) return t("cat.webkit");
  if (path.includes("/Saved Application State/")) return t("cat.savedstate");
  if (path.includes("/Containers/") || path.includes("/Group Containers/"))
    return t("cat.containers");
  return t("cat.other");
}

/**
 * The dossier: the app set as a small article — name, provenance line, the
 * leftovers summed per category with a 合计 row, the uninstall action, and a
 * collapsible per-file list where the selection can be trimmed.
 */
function renderDossier(
  container: HTMLElement,
  el: HTMLElement,
  app: AppInfo,
  detail: Detail,
): void {
  const checkedSet = selected.get(app.path);
  const editable = checkedSet !== undefined;
  const totalKb = detail.items.reduce((sum, i) => sum + (i.size_kb ?? 0), 0);

  // Group the leftovers by display category, preserving first-seen order so
  // the bundle itself stays at the top of the ledger.
  const groups = new Map<string, { kb: number; count: number }>();
  for (const item of detail.items) {
    const key = categoryOf(item.path);
    const bucket = groups.get(key) ?? { kb: 0, count: 0 };
    bucket.kb += item.size_kb ?? 0;
    bucket.count += 1;
    groups.set(key, bucket);
  }

  const notes = detail.notes
    .map((n) => `<span class="badge warn">${esc(n.note)}</span>`)
    .join("");

  const rows = detail.items
    .map(
      (i) => `<div class="tr det-item">
        <input type="checkbox" data-item="${esc(i.path)}" data-owner="${esc(app.path)}"
          ${checkedSet?.has(i.path) ? "checked" : ""} ${editable ? "" : "disabled"} />
        <span class="det-cat">${esc(categoryOf(i.path))}</span>
        <span class="det-path ell" title="${esc(i.path)}">${esc(i.path)}</span>
        <span class="num">${i.size_kb === null ? "?" : humanKb(i.size_kb)}</span>
      </div>`,
    )
    .join("");

  // Figure box (合计 + the uninstall action) above the per-category ledger,
  // then the collapsible 逐项明细 strip — the tabloid dossier order.
  el.innerHTML = `
    ${dossierKick()}
    <h1 class="dossier-name">${esc(app.name)}</h1>
    <p class="note dossier-path">${t("apps.version")} ${esc(app.version || "—")} · ${esc(app.path)}</p>
    ${notes ? `<div class="note-list">${notes}</div>` : ""}
    <div class="databox dossier-box">
      <div class="cell">
        <div class="cell-k">${t("apps.grandTotal")}</div>
        <div class="fig md">${figHtml(totalKb)}</div>
      </div>
      <div class="cell cell-actions">
        <button class="btn" id="det-remove">${t("apps.removeOne")} →</button>
        <p class="note">${t("apps.removeNote")}</p>
      </div>
    </div>
    <ul class="kv dossier-kv">
      ${[...groups.entries()]
        .map(
          ([name, bucket]) => `<li>
            <span class="l">${esc(name)}${bucket.count > 1 ? ` · ${bucket.count} ${t("apps.files")}` : ""}</span>
            <span class="dots"></span>
            <span class="v">${humanKb(bucket.kb)}</span>
          </li>`,
        )
        .join("")}
      <li class="total">
        <span class="l">${t("apps.grandTotal")}</span>
        <span class="dots"></span>
        <span class="v">${humanKb(totalKb)}</span>
      </li>
    </ul>
    <div class="det-card">
      <div class="detail-box">
        <b>${t("apps.detail.files", { n: detail.items.length })}</b>
        <span class="num det-count" data-count></span>
        <a href="#" class="link plain push" data-toggle ${editable ? "" : "hidden"}></a>
        <button class="chev" id="det-expand" title="${t("prev.expand")}">▾</button>
      </div>
      <div class="tbl" id="det-items" hidden>${rows}</div>
    </div>`;

  const items = [...el.querySelectorAll<HTMLInputElement>("input[data-item]")];
  const count = el.querySelector<HTMLElement>("[data-count]")!;
  const toggle = el.querySelector<HTMLAnchorElement>("[data-toggle]")!;
  const itemsBox = el.querySelector<HTMLElement>("#det-items")!;

  el.querySelector("#det-expand")!.addEventListener("click", (ev) => {
    itemsBox.hidden = !itemsBox.hidden;
    (ev.currentTarget as HTMLElement).textContent = itemsBox.hidden ? "▾" : "▴";
  });

  /** Sync the counter and the select-all link. */
  const syncHead = () => {
    const set = selected.get(app.path);
    const n = set ? set.size : 0;
    const kb = detail.items
      .filter((i) => set?.has(i.path))
      .reduce((sum, i) => sum + (i.size_kb ?? 0), 0);
    count.textContent = `${t("apps.detail.count", { n, total: detail.items.length })} · ${humanKb(kb)}`;
    toggle.textContent = n === detail.items.length ? t("apps.deselectAll") : t("apps.selectAll");
  };

  /** Set every item to one state (select-all link). */
  const setAll = (on: boolean) => {
    const set = selected.get(app.path);
    if (!set) return;
    for (const cb of items) {
      cb.checked = on;
      if (on) set.add(cb.dataset.item!);
      else set.delete(cb.dataset.item!);
    }
    syncHead();
    patchRowSummary(container, app.path);
    updateFooter(container);
  };

  items.forEach((cb) => {
    cb.addEventListener("change", () => {
      const set = selected.get(cb.dataset.owner!);
      if (!set) return;
      if (cb.checked) set.add(cb.dataset.item!);
      else set.delete(cb.dataset.item!);
      syncHead();
      patchRowSummary(container, app.path);
      updateFooter(container);
    });
  });
  toggle.addEventListener("click", (ev) => {
    ev.preventDefault();
    setAll(!(selected.get(app.path)?.size === detail.items.length));
  });

  // "完全卸载 →" ticks the app if it is not selected yet, then hands the
  // whole current selection to the shared removal funnel.
  el.querySelector("#det-remove")!.addEventListener("click", () => {
    if (!selected.has(app.path)) {
      selected.set(app.path, new Set(detail.items.map((i) => i.path)));
      const cb = container.querySelector<HTMLInputElement>(
        `input[data-app="${cssEscape(app.path)}"]`,
      );
      if (cb) cb.checked = true;
      container
        .querySelector<HTMLElement>(`[data-row="${cssEscape(app.path)}"]`)
        ?.classList.add("selected");
      patchRowSummary(container, app.path);
      updateFooter(container);
      renderDossier(container, el, app, detail);
    }
    void runRemoval(container);
  });

  syncHead();
}

/** Patch one row's "N selected · size" summary span in place. */
function patchRowSummary(container: HTMLElement, appPath: string): void {
  const span = container.querySelector<HTMLElement>(`[data-sum="${cssEscape(appPath)}"]`);
  if (!span) return;
  const set = selected.get(appPath);
  const detail = detailCache.get(appPath);
  if (!set) {
    span.textContent = "";
    return;
  }
  if (!detail) {
    // Scan still running: show the bundle size as a provisional summary.
    span.textContent = `1 ${t("apps.selected")}`;
    return;
  }
  span.textContent = `${set.size} ${t("apps.selected")}`;
}

/** Rebuild ONLY the footer bar (cheap; icons come from the client cache). */
function updateFooter(container: HTMLElement): void {
  const footer = container.querySelector<HTMLElement>("#footer");
  if (!footer) return;
  const paths = [...selected.keys()];
  if (paths.length === 0) {
    footer.style.display = "none";
    return;
  }
  let totalKb = 0;
  for (const [path, set] of selected) {
    const detail = detailCache.get(path);
    if (!detail) {
      // Leftover scan pending: count the bundle with its listed size.
      totalKb += appSizes.get(path) ?? 0;
      continue;
    }
    totalKb += detail.items
      .filter((i) => set.has(i.path))
      .reduce((sum, i) => sum + (i.size_kb ?? 0), 0);
  }
  footer.style.display = "";
  footer.innerHTML = `
    <span class="count">${t("apps.selectedN", { n: paths.length })}</span>
    <span class="num">${humanKb(totalKb)}</span>
    <a href="#" class="link plain" id="clear-sel">${t("apps.cancel")}</a>
    <span class="dots"></span>
    <button class="btn" id="remove">${esc(t("apps.removeN", { n: paths.length }))} →</button>`;

  footer.querySelector("#clear-sel")!.addEventListener("click", (ev) => {
    ev.preventDefault();
    selected.clear();
    container
      .querySelectorAll<HTMLInputElement>("input[data-app]")
      .forEach((cb) => (cb.checked = false));
    container
      .querySelectorAll<HTMLElement>(".app-row.selected")
      .forEach((r) => r.classList.remove("selected"));
    container
      .querySelectorAll<HTMLElement>("[data-sum]")
      .forEach((s) => (s.textContent = ""));
    footer.style.display = "none";
  });

  footer.querySelector("#remove")!.addEventListener("click", () => void runRemoval(container));
}

/** Re-plan every selected app atomically and hand it to the confirm funnel. */
async function runRemoval(container: HTMLElement): Promise<void> {
  const paths = [...selected.keys()];
  if (paths.length === 0) return;
  // Apps whose leftover scan is still pending hold only the bundle path
  // (optimistic selection); resolve them so the full item set is removed.
  for (const path of paths) {
    if (detailCache.has(path)) continue;
    const detail = await loadDetail(path);
    if (selected.has(path)) selected.set(path, new Set(detail.items.map((i) => i.path)));
  }
  // Previews are display state, never the executable plan.
  const payload = await planUninstall(paths, newTaskId());
  const wanted = new Set([...selected.values()].flatMap((set) => [...set]));
  const items = payload.summary.sections.flatMap((s) => s.items);
  const selection = items.filter((i) => wanted.has(i.path)).map((i) => i.id);
  const kb = items
    .filter((i) => wanted.has(i.path))
    .reduce((sum, i) => sum + (i.size_kb ?? 0), 0);
  const bodyEl = container.querySelector<HTMLElement>(".apps-page")!;
  confirmSheet(selection.length, kb, t("apps.remove"), () => {
    setNavMeta(null);
    runExecution(bodyEl, [{ planId: payload.summary.plan_id, selection }], { ledger: "apps" });
  });
}

/** Load icons lazily with a small concurrency window; patch avatars in place. */
async function loadIcons(list: HTMLElement, apps: AppInfo[]): Promise<void> {
  const queue = [...apps];
  const worker = async () => {
    while (queue.length > 0) {
      const app = queue.shift()!;
      if (!iconCache.has(app.path)) {
        try {
          iconCache.set(app.path, await appIcon(app.path));
        } catch {
          iconCache.set(app.path, null);
        }
      }
      const icon = iconCache.get(app.path);
      if (icon) {
        const el = list.querySelector<HTMLElement>(`[data-avatar="${cssEscape(app.path)}"]`);
        if (el) {
          el.style.background = "transparent";
          el.innerHTML = `<img src="${icon}" alt="" />`;
        }
      }
    }
  };
  await Promise.all([worker(), worker(), worker(), worker()]);
}

/** A size as a tabloid figure: serif number with the unit set small ("14.86<small>GB</small>"). */
function figHtml(kb: number): string {
  const text = humanKb(kb);
  const match = /^([\d.]+)(\D+)$/.exec(text);
  return match ? `${match[1]}<small>${match[2]}</small>` : esc(text);
}

/** Escape a value for use inside an attribute selector. */
function cssEscape(value: string): string {
  return CSS.escape(value);
}

// ---------------------------------------------------------------------------
// 更新
// ---------------------------------------------------------------------------

/** Updates tab: an update ledger with a source-diagnosis rail. Loading
 * contract: paint the newest known catalog instantly (stale ids ⇒ actions
 * disabled + a checking indicator), then run the real scan in the background
 * and swap it in. A live memory snapshot skips the rescan. */
async function renderUpdates(body: HTMLElement, force = false): Promise<void> {
  const taskId = newTaskId();
  updateTaskId = taskId;
  const extra = document.getElementById("tab-extra");
  if (extra) extra.innerHTML = "";
  // The app inventory only maps update names to app paths for icons; it
  // resolves independently so it never delays the first paint.
  const appsPromise = listApps(newTaskId()).catch(() => [] as AppInfo[]);
  const wireIcons = () =>
    void appsPromise.then((apps) => {
      const map = new Map(apps.map((app) => [app.name.toLowerCase(), app.path]));
      void loadUpdateIcons(body, map);
    });
  const showChecked = (catalog: UpdateCatalog) => {
    setNavMeta(
      `${t("upd.checked")} ${new Date(catalog.checked_at * 1000).toLocaleTimeString()}`,
    );
    if (!extra) return;
    extra.innerHTML = `<a href="#" id="refresh-updates">${REFRESH_SVG} ${t("upd.refresh")}</a>`;
    extra.querySelector("#refresh-updates")?.addEventListener("click", (ev) => {
      ev.preventDefault();
      lastUpdateResults = [];
      void renderUpdates(body, true);
    });
  };

  // Instant first paint from the backend's cached catalog. A live snapshot is
  // current and actionable — render it and stop, no background rescan.
  let stalePainted = false;
  if (!force) {
    const cached = await cachedAppUpdates().catch(() => null);
    if (updateTaskId !== taskId) return;
    if (cached?.live) {
      updateTaskId = null;
      paintUpdates(body, cached.catalog, false);
      wireIcons();
      showChecked(cached.catalog);
      return;
    }
    if (cached && cached.catalog.updates.length > 0) {
      paintUpdates(body, cached.catalog, true);
      wireIcons();
      stalePainted = true;
      setNavMeta(`↻ ${t("upd.checking")}`);
    }
  }
  if (!stalePainted) {
    body.innerHTML = `<div class="placeholder">${t("upd.loading")} <button id="cancel-update-scan">${t("upd.cancel")}</button></div>`;
    body.querySelector("#cancel-update-scan")?.addEventListener("click", () => {
      void cancelTask(taskId);
    });
  }

  const scan = stopwatch();
  try {
    const catalog = await listAppUpdates(taskId, force);
    track({ kind: "scan_completed", scan: "updates", duration_ms: scan() });
    if (updateTaskId !== taskId) return;
    updateTaskId = null;
    paintUpdates(body, catalog, false);
    wireIcons();
    showChecked(catalog);
  } catch (e) {
    if (updateTaskId === taskId) updateTaskId = null;
    if (stalePainted) {
      // Background check failed or was cancelled: keep the cached list on
      // screen and just drop the checking indicator.
      setNavMeta(null);
    } else {
      body.innerHTML = `<div class="placeholder">${esc(String((e as Error)?.message ?? e))}</div>`;
    }
  }
}

/** Build and wire the Updates body for one catalog. stale=true renders the
 * same layout with update/ignore actions disabled: those ids are not backed
 * by the backend's live authorization snapshot yet. */
function paintUpdates(body: HTMLElement, catalog: UpdateCatalog, stale: boolean): void {
  const visible = catalog.updates.filter((update) => !update.ignored);
  const hidden = catalog.updates.filter((update) => update.ignored);
  // Reference grouping: rows Tidy can act on vs. terminal-owned packages.
  const inMole = visible.filter((update) => update.action !== "terminal");
  const terminal = visible.filter((update) => update.action === "terminal");

  /** One update row: icon | name + source badge + notes chevron / version line | ignore | action. */
  const rowHtml = (update: AppUpdate, hiddenRow: boolean): string => {
    const chev = update.release_notes
      ? `<button class="chev" data-notes="${esc(update.id)}" title="${t("upd.notes")}">▾</button>`
      : "";
    const action = hiddenRow
      ? ""
      : update.action === "terminal"
        ? `<code class="upd-cmd" title="${t("upd.terminal")}">${esc(update.command_hint ?? t("upd.terminal"))}</code>`
        : `<button class="upd-btn" data-update="${esc(update.id)}" ${stale ? "disabled" : ""} title="${esc(actionLabel(update))}">${t("upd.update")} →</button>`;
    return `<div class="tr upd-row">
      <span class="icon" data-upname="${esc(update.name)}">${esc(update.name.slice(0, 1).toUpperCase())}</span>
      <div class="info">
        <span class="title-line">
          <b class="name">${esc(update.name)}</b>
          <span class="badge">${esc(sourceLabel(update.source))}</span>
          ${chev}
        </span>
        <span class="ver">
          <span class="old">${esc(update.installed)}</span>
          <span class="arrow">→</span>
          <span class="new">${esc(update.latest)}</span>
        </span>
        ${update.release_notes ? `<div class="notes" data-notesbody="${esc(update.id)}" style="display:none">${esc(update.release_notes)}</div>` : ""}
      </div>
      <a href="#" class="upd-ignore ${stale ? "disabled" : ""}" data-ignore="${esc(update.id)}" data-ignored="${hiddenRow}">${
        hiddenRow ? t("upd.unignore") : t("upd.ignoreRow")
      }</a>
      <span class="upd-act">${action}</span>
    </div>`;
  };

  /** Module eyebrow for one group of rows (code, title, mono aside). */
  const kick = (code: string, title: string, aside = ""): string =>
    `<div class="kick"><span class="code">${code}</span><span class="t">${title}</span>${
      aside ? `<span class="aside">${aside}</span>` : ""
    }</div>`;

  const resultBlock = lastUpdateResults.length
    ? `<div class="tbl upd-results">${lastUpdateResults
        .map(
          (result) =>
            `<div class="tr"><span class="badge ${result.outcome === "updated" ? "ok" : result.outcome === "failed" ? "danger" : ""}">${esc(result.outcome)}</span><span class="msg">${esc(result.message)}</span></div>`,
        )
        .join("")}</div>`
    : "";
  const upToDateBlock = catalog.up_to_date.length
    ? `<details class="fold uptodate"><summary><span>${t("upd.uptodate")}</span><span class="num muted">${catalog.up_to_date.length}</span><span class="chev">▸</span></summary>
        <div class="tbl">
        ${catalog.up_to_date
          .map(
            (app) =>
              `<div class="tr c2"><span class="ell">${esc(app.name)}</span><span class="note">${esc(app.version)} · ${esc(sourceLabel(app.source))}</span></div>`,
          )
          .join("")}
        </div>
      </details>`
    : "";

  // Source diagnosis rail: how the known apps are reachable, plus whatever
  // the backend could not classify (its warnings, verbatim).
  const bySource = new Map<string, number>();
  for (const app of [...catalog.updates, ...catalog.up_to_date]) {
    const key = sourceLabel(app.source);
    bySource.set(key, (bySource.get(key) ?? 0) + 1);
  }
  const rail = `
    <div class="side">
      <aside class="mod">
        ${kick("B4", t("upd.sources"))}
        <div class="tbl">
          <div class="tr th c2"><span>${t("upd.source.head")}</span><span>${t("upd.source.count")}</span></div>
          ${[...bySource.entries()]
            .sort((a, b) => b[1] - a[1])
            .map(
              ([name, n]) =>
                `<div class="tr c2"><span class="ell">${esc(name)}</span><span class="num">${n}</span></div>`,
            )
            .join("")}
        </div>
        ${
          catalog.warnings.length
            ? `<details class="fold warnings">
                <summary><span class="link plain">${t("upd.warnings")} · ${catalog.warnings.length}</span><span class="chev">▸</span></summary>
                <div class="note">
                  ${catalog.warnings.map((warning) => `<div>${esc(warning)}</div>`).join("")}
                </div>
              </details>`
            : ""
        }
        ${hidden.length ? `<p class="note side-foot">${t("upd.hiddenCount", { n: hidden.length })}</p>` : ""}
      </aside>
    </div>`;

  const header = kick(
    "B1",
    t("upd.available"),
    `${t("upd.count", { n: inMole.length })}${
      inMole.length && !stale ? ` <a href="#" id="update-all" class="link plain upd-all">${t("upd.updateAll")} →</a>` : ""
    }`,
  );

  const main =
    visible.length === 0 && hidden.length === 0
      ? `<div class="placeholder">${catalog.warnings.length ? t("upd.noneKnown") : t("upd.none")}</div>`
      : `${header}
        <div class="tbl">${inMole.map((update) => rowHtml(update, false)).join("")}</div>
        ${
          terminal.length
            ? `<div class="grp">
                ${kick("B2", t("upd.packages"), t("upd.count", { n: terminal.length }))}
                <div class="tbl">${terminal.map((update) => rowHtml(update, false)).join("")}</div>
                <p class="note upd-hint">${t("upd.hint")}</p>
              </div>`
            : ""
        }
        ${
          hidden.length
            ? `<div class="grp">
                ${kick("B3", t("upd.hidden"), t("upd.count", { n: hidden.length }))}
                <div class="tbl">${hidden.map((update) => rowHtml(update, true)).join("")}</div>
              </div>`
            : ""
        }`;

  body.innerHTML = `
    ${resultBlock}
    <div class="grid split wide-side upd-grid">
      <section class="mod">${main}${upToDateBlock}</section>
      ${rail}
    </div>`;

  // Notes chevrons work in both modes; actions are wired only on live data.
  body.querySelectorAll<HTMLButtonElement>("button[data-notes]").forEach((button) => {
    button.addEventListener("click", () => {
      const notes = body.querySelector<HTMLElement>(
        `[data-notesbody="${cssEscape(button.dataset.notes!)}"]`,
      );
      if (!notes) return;
      const open = notes.style.display !== "none";
      notes.style.display = open ? "none" : "";
      button.textContent = open ? "▾" : "▴";
    });
  });
  if (stale) return;
  body.querySelector("#update-all")?.addEventListener("click", (ev) => {
    ev.preventDefault();
    confirmSheet(inMole.length, 0, t("upd.updateAll"), () => {
      void runUpdateSelection(body, inMole);
    });
  });
  body.querySelectorAll<HTMLButtonElement>("button[data-update]").forEach((button) => {
    button.addEventListener("click", () => {
      const update = catalog.updates.find((row) => row.id === button.dataset.update);
      if (update) void runUpdateSelection(body, [update]);
    });
  });
  body.querySelectorAll<HTMLAnchorElement>("a[data-ignore]").forEach((link) => {
    link.addEventListener("click", async (ev) => {
      ev.preventDefault();
      link.style.pointerEvents = "none";
      await setAppUpdateIgnored(link.dataset.ignore!, link.dataset.ignored !== "true");
      lastUpdateResults = [];
      await renderUpdates(body);
    });
  });
}

/** Fill update-row avatars via the name→path inventory map (lazy, cached). */
async function loadUpdateIcons(
  body: HTMLElement,
  appPathByName: Map<string, string>,
): Promise<void> {
  const targets = [...body.querySelectorAll<HTMLElement>("[data-upname]")];
  for (const el of targets) {
    const path = appPathByName.get((el.dataset.upname ?? "").toLowerCase());
    if (!path) continue;
    if (!iconCache.has(path)) {
      try {
        iconCache.set(path, await appIcon(path));
      } catch {
        iconCache.set(path, null);
      }
    }
    const icon = iconCache.get(path);
    if (icon && el.isConnected) {
      el.style.background = "transparent";
      el.innerHTML = `<img src="${icon}" alt="" />`;
    }
  }
}

async function runUpdateSelection(body: HTMLElement, updates: AppUpdate[]): Promise<void> {
  if (updates.length === 0) return;
  const ids = updates.map((update) => update.id);
  // 一次批量升级里每种来源只记一条：想知道的是"Homebrew 这条路有人走吗"，
  // 而不是这次点了几个应用（数量不上报）。
  for (const source of new Set(updates.map((update) => update.source))) {
    track({ kind: "updates_run", source });
  }
  body.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
    button.disabled = true;
  });
  const taskId = newTaskId();
  updateTaskId = taskId;
  body.innerHTML = `<div class="placeholder">${t("upd.running")} <button id="cancel-update-run">${t("upd.cancel")}</button></div>`;
  body.querySelector("#cancel-update-run")?.addEventListener("click", () => {
    void cancelTask(taskId);
  });
  try {
    lastUpdateResults = await runAppUpdates(ids, taskId);
  } catch (error) {
    lastUpdateResults = [
      {
        id: "request",
        outcome: "failed",
        cause: "request_failed",
        message: String((error as Error)?.message ?? error),
      },
    ];
  } finally {
    if (updateTaskId === taskId) updateTaskId = null;
  }
  await renderUpdates(body);
}

function sourceLabel(source: string): string {
  return t(`upd.source.${source}`);
}

function actionLabel(update: AppUpdate): string {
  if (update.action === "terminal") return t("upd.terminal");
  if (update.action === "open_app_store") return t("upd.openStore");
  if (update.action === "open_app") return t("upd.openApp");
  if (update.action === "open_website") return t("upd.openWebsite");
  return t("upd.update");
}

// ---------------------------------------------------------------------------
// 启动项
// ---------------------------------------------------------------------------

/** Login-items tab: launchd rows with working toggles first (user scope;
 * system scope needs the helper), then the app-embedded helpers, which are
 * owned by their host app and therefore listed read-only. */
async function renderLogin(body: HTMLElement): Promise<void> {
  body.innerHTML = `<div class="placeholder">…</div>`;
  const [embedded, services] = await Promise.all([
    listEmbeddedLoginItems(),
    listLoginItems(),
  ]);
  const total = embedded.length + services.length;
  setNavMeta(t("login.meta", { n: total }));
  if (total === 0) {
    body.innerHTML = `<div class="placeholder">${t("login.none")}</div>`;
    return;
  }

  const serviceRows = services
    .map(
      (item: LoginItem, idx: number) => `<div class="tr li-row">
        <span class="li-glyph">▸</span>
        <span class="li-main">
          <span class="li-title ell">${esc(item.label)}</span>
          <span class="li-sub">
            <span class="ell">${item.path.includes("LaunchDaemons") ? t("login.daemon") : t("login.agent")} · ${esc(item.program ?? item.label)}</span>
            ${item.program !== null && !item.program_exists ? `<span class="badge danger">${t("login.orphan")}</span>` : ""}
          </span>
        </span>
        <button class="reveal" data-reveal="${esc(item.path)}" title="Finder">↗</button>
        <button class="toggle ${item.enabled ? "on" : ""}" data-toggle="${idx}"
          ${item.scope === "system" ? `disabled title="${t("login.admin.toggle")}"` : ""}></button>
      </div>`,
    )
    .join("");

  const embeddedRows = embedded
    .map(
      (item: EmbeddedLoginItem, idx: number) => `<div class="tr li-row">
        <span class="icon" data-liicon="${idx}">${esc(item.app_name.slice(0, 1).toUpperCase())}</span>
        <span class="li-main">
          <span class="li-title ell">${esc(item.app_name)}</span>
          <span class="li-sub"><span class="ell">${esc(item.item_name)} · ${
            item.kind === "login" ? t("login.kind.login") : t("login.kind.helper")
          } · ${t("login.embedded")}</span></span>
        </span>
        <button class="reveal" data-reveal="${esc(item.app_path)}" title="Finder">↗</button>
        <span class="li-ro">${t("login.readonly")}</span>
      </div>`,
    )
    .join("");

  const enabled = services.filter((item) => item.enabled).length;

  // Main module: B1 services (toggles) over B2 embedded helpers (read-only);
  // the rail carries the 随系统启动 figure and the explanation.
  body.innerHTML = `
    <div class="grid split login-grid">
      <section class="mod">
        <div class="kick">
          <span class="code">B1</span><span class="t">${t("login.services.head")}</span>
          <span class="aside">${t("login.onHint")}</span>
        </div>
        <div class="tbl">${serviceRows || `<p class="note">${t("login.none")}</p>`}</div>
        <div class="grp">
          <div class="kick">
            <span class="code">B2</span><span class="t">${t("login.items.head")} · ${t("login.confirmOnly")}</span>
            <span class="aside">${embedded.length} ${t("login.count")}</span>
          </div>
          <div class="tbl">${embeddedRows || `<p class="note">${t("login.none")}</p>`}</div>
        </div>
      </section>
      <div class="side">
        <aside class="mod">
          <div class="kick"><span class="code">B3</span><span class="t">${t("login.autostart")}</span></div>
          <div class="fig md">${enabled}<small>/ ${total}</small></div>
          <p class="prose sm login-explain">${t("login.explain")}</p>
          <p class="note side-foot">${t("login.admin.toggle")}</p>
        </aside>
      </div>
    </div>`;

  // Reveal buttons.
  body.querySelectorAll<HTMLButtonElement>("button[data-reveal]").forEach((btn) => {
    btn.addEventListener("click", () => void revealInFinder(btn.dataset.reveal!));
  });

  // App icons for embedded rows (lazy, cached client-side).
  void (async () => {
    for (let i = 0; i < embedded.length; i++) {
      const path = embedded[i].app_path;
      if (!iconCache.has(path)) {
        try {
          iconCache.set(path, await appIcon(path));
        } catch {
          iconCache.set(path, null);
        }
      }
      const icon = iconCache.get(path);
      if (icon) {
        const el = body.querySelector<HTMLElement>(`[data-liicon="${i}"]`);
        if (el) el.innerHTML = `<img src="${icon}" alt="" />`;
      }
    }
  })();

  // Working toggles for user-scope services.
  body.querySelectorAll<HTMLButtonElement>("button[data-toggle]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const item = services[Number(btn.dataset.toggle)];
      const target = !btn.classList.contains("on");
      btn.disabled = true;
      try {
        await setLoginItemEnabled(item.label, item.path, item.scope, target);
        btn.classList.toggle("on", target);
      } catch {
        // Refusal (test guard / admin) leaves the visual state untouched.
      }
      btn.disabled = false;
    });
  });
}
