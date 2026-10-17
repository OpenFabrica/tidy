// Analyze view (newspaper skin): the "space investigation" column. A
// breadcrumb headline, then the directory listing set as a ranked account —
// name, size, share, and a solid ink bar — with the largest child previewed
// in the right rail and the whole-disk split printed as a footer strip. The
// squarified treemap survives behind a view switch for people who read shapes
// faster than numbers.
// Completed listings are cached for the session and an in-flight scan keeps
// running across tab switches — re-entering the tab never restarts a scan.

import { analyzeScan, appMeta, cancelTask, newTaskId, planDeletePaths, statusSnapshot } from "../ipc";
import { stopwatch, track } from "../telemetry";
import { open } from "@tauri-apps/plugin-dialog";
import { renderFlow } from "../flow";
import { renderFrontPage } from "../frontpage";
import { renderScanCard, type ScanCard } from "../scancard";
import { esc, humanBytes, humanKb, splitUnit } from "../format";
import { t, timestamp } from "../i18n";
import { setNavMeta } from "../router";
import { squarify } from "../treemap";
import type { View } from "../router";
import type { DirListing, ScanMeter } from "../types";

/** Rank-ordered bar washes: the leader is rust, the tail fades into paper. */
const BARS = ["#a4432c", "#c08468", "#d9c1ac", "#d9c1ac", "#e8ddce", "#efe9dc"];

/** Warm block palette for the optional treemap view. */
const PALETTE = [
  "#a4432c", "#b95f43", "#c08468", "#c9a25e", "#a3865a",
  "#8f7a92", "#7c6f83", "#6e6258", "#7f8a6a", "#6a7a8a",
];

/** Current directory (persists across navigations within the session). */
let currentRoot = "";
let home = "";
/** Ledger (ranked bars) or treemap; persists across navigations. */
let viewMode: "ledger" | "treemap" = "ledger";

/** Cache of completed listings by root, persisted to localStorage so a
 * relaunch re-opens the last listings without re-scanning. Tab switches
 * re-render from here; dropped on manual refresh and cleared whenever a
 * trash flow starts, because deletion invalidates every cached size. */
const listingCache = new Map<string, DirListing>();
/** When each cached listing was scanned (epoch ms), shown as "cached at". */
const cachedAt = new Map<string, number>();

const STORE_KEY = "tidy.analyze.listings.v1";
/** Most roots to persist; oldest scans are evicted first. */
const STORE_MAX = 40;

/** Load the persisted cache once at module init (corrupt data is ignored). */
(function restoreCache() {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (!raw) return;
    const saved = JSON.parse(raw) as { root: string; at: number; listing: DirListing }[];
    for (const e of saved) {
      if (!e.root || !e.listing?.entries) continue;
      listingCache.set(e.root, e.listing);
      cachedAt.set(e.root, e.at);
    }
  } catch {
    /* ignore: cache is an optimisation only */
  }
})();

/** Write the cache back to localStorage (newest STORE_MAX roots). */
function persistCache(): void {
  try {
    const rows = [...listingCache.entries()]
      .map(([root, listing]) => ({ root, at: cachedAt.get(root) ?? 0, listing }))
      .sort((a, b) => b.at - a.at)
      .slice(0, STORE_MAX);
    localStorage.setItem(STORE_KEY, JSON.stringify(rows));
  } catch {
    /* quota or private mode: keep the in-memory cache only */
  }
}

/** Store a completed listing and persist. */
function cacheListing(root: string, listing: DirListing): void {
  listingCache.set(root, listing);
  cachedAt.set(root, Date.now());
  persistCache();
}

/** Drop one root (manual rescan) and persist. */
function dropListing(root: string): void {
  listingCache.delete(root);
  cachedAt.delete(root);
  persistCache();
}

/** Drop everything (after a deletion) and persist. */
function clearListings(): void {
  listingCache.clear();
  cachedAt.clear();
  persistCache();
}

/** "HH:MM" or "M/D HH:MM" for the cached-at label. */
function fmtTime(ms: number): string {
  const d = new Date(ms);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const today = new Date().toDateString() === d.toDateString();
  return today ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

/** The one in-flight scan, if any. It survives tab switches: the backend
 * keeps walking, progress accumulates in `count`/`label`, and the result lands
 * in the cache when done. Navigating to a different dir cancels it. */
interface ScanState {
  root: string;
  taskId: string;
  /** Directory the walker is reading right now (absolute path). */
  label: string;
  /** Directories visited so far; the total is unknown until the walk ends. */
  count: number;
  /** Epoch ms the scan started, for the elapsed clock. */
  startedAt: number;
  /** Scan-meter zero point, kept so a card re-rendered mid-scan (tab switch)
   * continues the counts instead of restarting them from zero. */
  baseline: ScanMeter | null;
  /** The user pressed cancel: drop the partial result and restore the page. */
  cancelled: boolean;
}
let pending: ScanState | null = null;

/** The progress card on screen, if any; only one ever runs. */
let card: ScanCard | null = null;

/** The container while this view is mounted; null lets a background scan
 * finish without touching another tab's DOM. */
let mounted: HTMLElement | null = null;

/** Whether a listing has been opened since launch. The front page is the
 * section's opener, so a cold start always lands there even with a warm
 * cache; once the reader has been inside, tab switches restore the listing. */
let visitedListing = false;

/** Root of the listing last put on screen, so a cancelled drill-in scan can
 * return to it. */
let lastListingRoot: string | null = null;

export const analyze: View = {
  async mount(container) {
    mounted = container;
    if (!home) home = (await appMeta()).home;
    if (mounted !== container) return; // switched away during the await
    if (!currentRoot) currentRoot = home;
    // Resume order: live scan → the listing the reader was already in →
    // the front page.
    if (pending) renderScanning(container, pending);
    else if (visitedListing && listingCache.has(currentRoot))
      renderListing(container, listingCache.get(currentRoot)!);
    else renderIdle(container);
  },
  unmount() {
    mounted = null;
    stopTick();
  },
};

/** Stop the card's clock and meter polling (the scan itself keeps running
 * in the backend). */
function stopTick(): void {
  card?.stop();
  card = null;
}

/**
 * Idle front page: the standing headline, the scope switcher and the disk
 * ledger. The whole-disk figures come from a status snapshot; the home figure
 * comes from the cache when that root has already been walked, because
 * measuring it just to label a button would cost a full scan.
 */
function renderIdle(container: HTMLElement): void {
  const homeListing = listingCache.get(home);
  const scopes: { label: string; root: string; size: string }[] = [
    { label: t("ana.whole"), root: "/", size: "" },
    {
      label: `${t("ana.homeScope")} ~`,
      root: home,
      size: homeListing ? humanKb(homeListing.total_kb) : "",
    },
  ];

  /** (Re)draw the whole page for the currently selected scope. */
  const draw = (disk: { total: number; used: number; free: number } | null) => {
    if (disk) scopes[0].size = humanBytes(disk.total);
    const active = scopes.find((scope) => scope.root === currentRoot);
    // The plate numeral quotes the selected scope's size when it is known.
    const known = active?.size ?? "";
    const cachedStamp = cachedAt.get(currentRoot);

    const scopeRow = `<div class="scope-row">
      ${scopes
        .map(
          (scope) =>
            `<button data-scope="${esc(scope.root)}" class="${scope.root === currentRoot ? "on" : ""}">${esc(
              scope.size ? `${scope.label} · ${scope.size}` : scope.label,
            )}</button>`,
        )
        .join("")}
      <button data-pick="1" class="${scopes.every((s) => s.root !== currentRoot) ? "on" : ""}">${esc(
        scopes.every((s) => s.root !== currentRoot) ? tildify(currentRoot) : t("ana.pickFolder"),
      )}</button>
    </div>`;

    const start = renderFrontPage(container, {
      kicker: t("ana.kicker"),
      strapline: t("ana.strapline"),
      // A plate number is a round figure: "761", never "761.86".
      watermark: known ? String(Math.round(Number(splitUnit(known)[0]))) : "",
      watermarkWide: true,
      headlineHtml: t("ana.headline"),
      desk: t("ana.desk"),
      dateline: cachedStamp
        ? `${t("ana.lastScan")} · ${timestamp(cachedStamp)}`
        : t("ana.neverScanned"),
      extraHtml: scopeRow,
      action: t("ana.start"),
      actionNote: cachedStamp
        ? `${t("ana.cachedResult")}\n${t("ana.cachedNote", { time: fmtTime(cachedStamp) })}`
        : t("ana.freshNote"),
      // The cached-result note doubles as the way back into it.
      onActionNote:
        cachedStamp !== undefined
          ? () => renderListing(container, listingCache.get(currentRoot)!)
          : undefined,
      noteBody: t("ana.standfirst"),
      stats: disk
        ? [
            { label: t("ana.diskTotal"), value: humanBytes(disk.total) },
            { label: t("ana.diskUsed"), value: humanBytes(disk.used) },
            { label: t("ana.diskFree"), value: humanBytes(disk.free) },
          ]
        : [
            { label: t("ana.diskTotal"), value: "…" },
            { label: t("ana.diskUsed"), value: "…" },
            { label: t("ana.diskFree"), value: "…" },
          ],
    });

    container.querySelectorAll<HTMLButtonElement>("[data-scope]").forEach((button) => {
      button.addEventListener("click", () => {
        currentRoot = button.dataset.scope!;
        draw(disk);
      });
    });
    container.querySelector<HTMLButtonElement>("[data-pick]")!.addEventListener("click", async () => {
      // The picker only returns a path; the scan itself still goes through
      // analyze_scan, which is the same funnel the built-in scopes use.
      const picked = await open({ directory: true, multiple: false, defaultPath: home });
      if (typeof picked !== "string") return;
      currentRoot = picked;
      draw(disk);
    });
    start.addEventListener("click", () => startScan(currentRoot));
  };

  draw(null);
  // The disk ledger is context, not content: a failed snapshot leaves the
  // dashes in place rather than blocking the page.
  void statusSnapshot()
    .then((snapshot) => {
      const root = snapshot.disks.find((d) => d.mount_point === "/") ?? snapshot.disks[0];
      if (!root || mounted !== container) return;
      draw({
        total: root.total_bytes,
        used: root.total_bytes - root.available_bytes,
        free: root.available_bytes,
      });
    })
    .catch(() => undefined);
}

/** Navigate to a directory: serve from cache or start a scan. */
function openDir(root: string): void {
  currentRoot = root;
  const cached = listingCache.get(root);
  if (cached) {
    // Drop any in-flight scan too, or its completion would replace this view.
    if (pending) {
      void cancelTask(pending.taskId);
      pending = null;
    }
    if (mounted) renderListing(mounted, cached);
    return;
  }
  startScan(root);
}

/** Kick off a background scan of `root`, superseding any in-flight scan. */
function startScan(root: string, fresh = false): void {
  currentRoot = root;
  // The user navigated away from the old target — stop paying for it.
  if (pending) void cancelTask(pending.taskId);
  const state: ScanState = {
    root,
    taskId: newTaskId(),
    label: "",
    count: 0,
    startedAt: Date.now(),
    baseline: null,
    cancelled: false,
  };
  pending = state;
  if (mounted) renderScanning(mounted, state);
  const scan = stopwatch();
  analyzeScan(root, state.taskId, fresh, (e) => {
    // Walk progress: directories visited so far (total unknown until done)
    // plus the directory being read right now.
    state.count = e.count;
    state.label = e.label;
    if (pending !== state) return;
    card?.setLabel(e.label);
  })
    .then((listing) => {
      // 只有跑完整的扫描才记时长：被取消的扫描（truncated）会把中位数拉低，
      // 让"扫描要多久"这个指标变成假的。
      if (!listing.truncated) {
        track({ kind: "scan_completed", scan: "analyze", duration_ms: scan() });
      }
      if (pending !== state) return; // superseded by a newer navigation
      pending = null;
      stopTick();
      if (state.cancelled) {
        restoreAfterCancel();
        return;
      }
      // Truncated scans are incomplete — render once, never cache.
      if (!listing.truncated) cacheListing(root, listing);
      if (mounted) renderListing(mounted, listing);
    })
    .catch((e: unknown) => {
      if (pending !== state) return;
      pending = null;
      stopTick();
      if (state.cancelled) {
        restoreAfterCancel();
        return;
      }
      if (mounted)
        mounted.innerHTML = `<div class="placeholder">${esc(String((e as Error)?.message ?? e))}</div>`;
    });
}

/** Scanning page: the shared progress card (same as Clean's scan), shown
 * fresh or when returning mid-scan with the clock and counts carried over. */
function renderScanning(container: HTMLElement, state: ScanState): void {
  stopTick();
  card = renderScanCard(container, {
    home,
    icon: "chart",
    title: t("scan.ana.title"),
    startedAt: state.startedAt,
    baseline: state.baseline,
    onBaseline: (m) => {
      state.baseline = m;
    },
    onCancel: () => {
      state.cancelled = true;
      void cancelTask(state.taskId);
    },
  });
  card.setLabel(state.label || state.root);
}

/** After a user cancel, show what the page showed before the scan: the
 * listing the reader came from (drill-in cancelled), else the front page. */
function restoreAfterCancel(): void {
  if (!mounted) return;
  const back = lastListingRoot;
  if (back !== null && listingCache.has(back)) {
    currentRoot = back;
    renderListing(mounted, listingCache.get(back)!);
  } else {
    renderIdle(mounted);
  }
}

/** Ranked ledger (or treemap) + child rail + whole-disk strip. */
function renderListing(container: HTMLElement, listing: DirListing): void {
  visitedListing = true;
  lastListingRoot = listing.root;
  const entries = listing.entries;
  const TOP = 6;
  const shown = entries.slice(0, TOP);
  const rest = entries.slice(TOP);
  const restKb = rest.reduce((sum, e) => sum + e.size_kb, 0);
  const maxKb = Math.max(1, shown[0]?.size_kb ?? 1, restKb);

  /** One ranked row: label + size/share on a line, a solid bar underneath. */
  const barRow = (
    name: string,
    kb: number,
    idx: number,
    path: string | null,
    lead: boolean,
  ) => {
    const share = listing.total_kb > 0 ? Math.round((kb / listing.total_kb) * 100) : 0;
    const width = Math.max(1, Math.round((kb / maxKb) * 100));
    return `<div class="bar-row ${path ? "dir" : ""} ${lead ? "lead" : ""}" ${path ? `data-dir="${esc(path)}"` : ""}>
      <div class="bar-line">
        <span class="bar-name">${esc(name)}</span>
        <span class="bar-size">${humanKb(kb)} · ${share}%</span>
      </div>
      <div class="bar" style="width:${width}%;background:${BARS[Math.min(idx, BARS.length - 1)]}"></div>
    </div>`;
  };

  const bars = [
    ...shown.map((e, idx) => barRow(e.name, e.size_kb, idx, e.is_dir ? e.path : null, idx === 0)),
    restKb > 0
      ? barRow(t("ana.restN", { n: rest.length }), restKb, BARS.length - 1, null, false)
      : "",
  ].join("");

  // The rail previews the largest child directory. Its own children are only
  // printed when that directory has already been scanned — a rail must never
  // trigger a second walk behind the reader's back.
  const leadEntry = shown.find((e) => e.is_dir);
  const leadListing = leadEntry ? listingCache.get(leadEntry.path) : undefined;
  const railRows = leadListing
    ? [
        ...leadListing.entries.slice(0, 4).map(
          (e) => `<div class="led">
            <span>${esc(e.name)}</span><span class="dots"></span>
            <span class="amt">${humanKb(e.size_kb)}</span>
          </div>`,
        ),
        leadListing.entries.length > 4
          ? `<div class="led"><span class="muted">${t("ana.restN", {
              n: leadListing.entries.length - 4,
            })}</span><span class="dots"></span><span class="amt dim">${humanKb(
              leadListing.entries.slice(4).reduce((sum, e) => sum + e.size_kb, 0),
            )}</span></div>`
          : "",
      ].join("")
    : `<div class="led"><span class="muted">${t("ana.railUnscanned")}</span></div>`;

  container.innerHTML = `
    <div class="ana-layout">
      <div class="col-main">
        <div style="display:flex;justify-content:space-between;align-items:baseline;gap:16px">
          <div class="crumbs">${breadcrumbs(listing.root)}</div>
          <span class="mono muted" style="font-size:12px;white-space:nowrap;flex-shrink:0">
            ${entries.length} ${t("ana.items")} · ${humanKb(listing.total_kb)}
            ${listing.truncated ? `<span class="badge warn">${t("ana.cancelled")}</span>` : ""}
          </span>
        </div>
        <div class="rule" style="padding-top:16px">
          ${viewMode === "ledger" ? `<div style="display:flex;flex-direction:column;gap:14px">${bars}</div>` : `<div class="treemap" id="treemap"></div>`}
        </div>
        <span class="muted" style="font-size:12px">${t("ana.hint")}</span>
      </div>
      <div class="col-main col-rule" style="flex:0 0 330px">
        <div class="rail-title">
          ${leadEntry ? `${esc(leadEntry.name)} <span class="unit">${t("ana.inside")}</span>` : t("ana.selection")}
        </div>
        <div class="ledger tight rule" style="padding-top:14px">${railRows}</div>
        ${leadEntry ? `<button class="link-cta sm" id="enter-lead">${t("ana.enter", { name: leadEntry.name })} →</button>` : ""}
        <button class="link-quiet" id="trash-selected" disabled>${t("ana.trash")}</button>
        <div class="rule-soft" style="padding-top:14px">
          <div class="sec-label" style="margin-bottom:8px">${t("ana.pick")}</div>
          <div class="ledger tight">
            ${entries
              .slice(0, 10)
              .map(
                (e) => `<div class="led center">
                  <input type="checkbox" data-path="${esc(e.path)}" />
                  <span class="note" style="font-size:12.5px;color:var(--ink)">${esc(e.name)}</span>
                  <span class="dots"></span>
                  <span class="amt dim">${humanKb(e.size_kb)}</span>
                </div>`,
              )
              .join("")}
          </div>
        </div>
      </div>
    </div>
    <div class="disk-strip" id="disk-strip">
      <span>${t("ana.disk")}</span>
      <div class="strip" id="strip"></div>
      <span id="disk-label">…</span>
    </div>`;

  setNavMeta(
    `${
      cachedAt.has(listing.root)
        ? esc(t("ana.cached", { time: fmtTime(cachedAt.get(listing.root)!) }))
        : ""
    } <a href="#" id="toggle-view">${esc(viewMode === "ledger" ? t("ana.viewTreemap") : t("ana.viewLedger"))}</a> · <a href="#" id="rescan">${esc(t("ana.refresh"))}</a>`,
  );
  document.getElementById("toggle-view")?.addEventListener("click", (ev) => {
    ev.preventDefault();
    viewMode = viewMode === "ledger" ? "treemap" : "ledger";
    renderListing(container, listing);
  });
  document.getElementById("rescan")?.addEventListener("click", (ev) => {
    ev.preventDefault();
    dropListing(listing.root);
    startScan(listing.root, true);
  });

  if (viewMode === "treemap") renderTreemap(container, listing);
  void renderDiskStrip(container);

  // Navigation: ranked rows, treemap blocks, the rail button, breadcrumbs.
  container.querySelectorAll<HTMLElement>(".bar-row.dir").forEach((row) => {
    row.addEventListener("click", () => openDir(row.dataset.dir!));
  });
  container.querySelector("#enter-lead")?.addEventListener("click", () => {
    if (leadEntry) openDir(leadEntry.path);
  });
  container.querySelectorAll<HTMLAnchorElement>("a.crumb").forEach((a) => {
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      openDir(a.dataset.dir!);
    });
  });

  // Selection → Trash flow.
  const trashBtn = container.querySelector<HTMLButtonElement>("#trash-selected")!;
  const update = () => {
    trashBtn.disabled =
      container.querySelectorAll("input[data-path]:checked").length === 0;
  };
  container
    .querySelectorAll<HTMLInputElement>("input[data-path]")
    .forEach((cb) => cb.addEventListener("change", update));
  trashBtn.addEventListener("click", async () => {
    const paths = Array.from(
      container.querySelectorAll<HTMLInputElement>("input[data-path]:checked"),
    ).map((c) => c.dataset.path!);
    const meta = await appMeta();
    // Deletion invalidates every cached size along the ancestor chain, so the
    // whole cache goes, not just this root.
    clearListings();
    setNavMeta(null);
    renderFlow(container, newTaskId(), {
      title: t("ana.trash"),
      verb: t("ana.trash"),
      helperAvailable: meta.helper_available,
      ledger: "analyze",
      home,
      plan: (taskId) => planDeletePaths(paths, taskId),
    });
  });
}

/** Whole-disk footer strip: used vs available on the boot volume. */
async function renderDiskStrip(container: HTMLElement): Promise<void> {
  const strip = container.querySelector<HTMLElement>("#strip");
  const label = container.querySelector<HTMLElement>("#disk-label");
  if (!strip || !label) return;
  try {
    const snapshot = await statusSnapshot();
    const root = snapshot.disks.find((d) => d.mount_point === "/") ?? snapshot.disks[0];
    if (!root || !strip.isConnected) return;
    const used = root.total_bytes - root.available_bytes;
    const usedPct = (used / root.total_bytes) * 100;
    strip.innerHTML = `
      <div style="width:${usedPct.toFixed(1)}%;background:var(--rust)"></div>
      <div style="flex:1;background:var(--wash-1);border:1px dashed var(--ink-ghost);box-sizing:border-box"></div>`;
    label.textContent = `${t("st.used")} ${humanBytes(used)} · ${t("st.avail")} ${humanBytes(root.available_bytes)}`;
  } catch {
    // The strip is context, not content: a failed snapshot just leaves it out.
    container.querySelector("#disk-strip")?.remove();
  }
}

/** Fill the treemap area: top blocks + one aggregate block for the tail. */
function renderTreemap(container: HTMLElement, listing: DirListing): void {
  const box = container.querySelector<HTMLElement>("#treemap");
  if (!box) return;
  const W = box.clientWidth || 700;
  const H = box.clientHeight || 460;

  const TOP = 9;
  const shown = listing.entries.slice(0, TOP).filter((e) => e.size_kb > 0);
  const rest = listing.entries.slice(TOP);
  const restKb = rest.reduce((sum, e) => sum + e.size_kb, 0);
  const weights = shown.map((e) => e.size_kb);
  if (restKb > 0) weights.push(restKb);
  const rects = squarify(weights, W, H);

  box.innerHTML = rects
    .map((r, idx) => {
      const isRest = idx >= shown.length;
      const entry = shown[idx];
      const label = isRest ? t("ana.restN", { n: rest.length }) : esc(entry.name);
      const size = isRest ? restKb : entry.size_kb;
      const showText = r.w > 70 && r.h > 40;
      return `<div class="block" data-dir="${!isRest && entry.is_dir ? esc(entry.path) : ""}"
        style="left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;
        background:${isRest ? "#6e6258" : PALETTE[idx % PALETTE.length]}">
        ${showText ? `<div class="bname">${label}</div><div class="bsize">${humanKb(size)}</div>` : ""}
      </div>`;
    })
    .join("");

  box.querySelectorAll<HTMLElement>(".block").forEach((block) => {
    block.addEventListener("click", () => {
      const dir = block.dataset.dir;
      if (dir) openDir(dir);
    });
  });
}

/** Abbreviate the home prefix to ~ for display. */
function tildify(path: string): string {
  if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

/** How many crumbs a deep path keeps: the disk root plus the last two levels. */
const CRUMB_KEEP = 3;

/**
 * Clickable breadcrumb chain, set as a headline. A deep path would otherwise
 * wrap to four lines and run into the rail, so anything longer than
 * CRUMB_KEEP collapses in the middle to an ellipsis that jumps to the parent;
 * the full path stays available as a tooltip.
 */
function breadcrumbs(root: string): string {
  const crumbs = [{ name: t("ana.whole"), dir: "/" }];
  let acc = "";
  for (const part of root.split("/").filter(Boolean)) {
    acc += "/" + part;
    crumbs.push({ name: part, dir: acc });
  }
  const link = (c: { name: string; dir: string }) =>
    `<a href="#" class="crumb" data-dir="${esc(c.dir)}" title="${esc(c.dir)}">${esc(c.name)}</a>`;
  const parts =
    crumbs.length > CRUMB_KEEP + 1
      ? [
          link(crumbs[0]),
          link({ ...crumbs[crumbs.length - 3], name: "…" }),
          ...crumbs.slice(-2).map(link),
        ]
      : crumbs.map(link);
  return parts.join(' <span class="sep">›</span> ');
}
