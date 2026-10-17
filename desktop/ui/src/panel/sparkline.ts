// One-minute SVG sparklines (C11). The backend publishes 60 one-second
// slots, oldest → newest, null = no sample. Paths are rebuilt only when a new
// reading arrives; between readings a shared ~30 fps loop slides every
// visible strip left by up to one slot so the chart scrolls smoothly instead
// of jumping once a second. Motion stops when the panel is hidden, when no
// fresh sample arrived (the strip parks on the newest point), and entirely
// under prefers-reduced-motion.

import { svg } from "./dom";
import type { DuplexSeries, Series } from "../types";

/** Slots per strip (HISTORY_LEN in the Rust sampler). */
const SLOTS = 60;
/** Frame budget: a minute-wide strip moves a fraction of a pixel per frame. */
const FRAME_MS = 1000 / 30;
/** One slot of travel per sample. */
const SLOT_MS = 1000;
/** Up to this many missing slots are bridged; longer runs are real gaps.
 * Slower metrics (memory every 3 s, temperatures every 4 s) may leave empty
 * slots between samples, which are not outages. */
const BRIDGE = 4;
/** Duplex charts never zoom below this ceiling, so idle chatter of a few
 * hundred bytes per second stays a flat line instead of filling the strip. */
const DUPLEX_FLOOR = 16 * 1024;
/** Keep a raised duplex ceiling this long before shrinking it again. */
const SHRINK_HOLD_MS = 30_000;

export type SparkKind = "usage" | "temperature" | "duplex";

/** A mounted sparkline; `update` feeds a new history snapshot. */
export interface Sparkline {
  el: HTMLElement;
  update(history: Series | DuplexSeries): void;
}

/** Registry of mounted strips the shared animation loop moves. */
const live = new Set<{ group: SVGGElement; el: HTMLElement }>();
/** performance.now() when the latest reading arrived (phase origin). */
let receivedAt = 0;
let panelVisible = true;
let timer: number | null = null;
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");

/** Current scroll phase 0…1 (slots travelled since the latest sample). */
function phase(clock: number): number {
  if (reduced.matches) return 1;
  return Math.max(0, Math.min(1, (clock - receivedAt) / SLOT_MS));
}

/** Apply the phase to every connected strip; drop detached ones. */
function paint(clock: number): void {
  const p = phase(clock);
  const transform = `translate(${(-p).toFixed(4)} 0)`;
  for (const entry of live) {
    if (!entry.el.isConnected) {
      live.delete(entry);
      continue;
    }
    entry.group.setAttribute("transform", transform);
  }
}

/** One animation step: paint, then schedule the next frame while moving. */
function tick(): void {
  timer = null;
  const clock = performance.now();
  paint(clock);
  if (panelVisible && !document.hidden && phase(clock) < 1 && live.size > 0) {
    timer = window.setTimeout(() => requestAnimationFrame(tick), FRAME_MS);
  }
}

/** Start the loop if it should run and is not running. */
function kick(): void {
  if (timer === null && panelVisible && !document.hidden) {
    timer = window.setTimeout(() => requestAnimationFrame(tick), 0);
  }
}

/** A new reading arrived: restart the one-slot slide. */
export function markSample(): void {
  receivedAt = performance.now();
  paint(receivedAt);
  kick();
}

/** Panel shown/hidden: animation runs only while visible. */
export function setSparkVisible(visible: boolean): void {
  panelVisible = visible;
  if (visible) kick();
}

document.addEventListener("visibilitychange", kick);
reduced.addEventListener("change", () => paint(performance.now()));

/** Smallest power of two ≥ v (auto-scale buckets for rates). */
function pow2(v: number): number {
  return 2 ** Math.ceil(Math.log2(Math.max(1, v)));
}

/** Split a series into drawable runs, bridging short gaps (see BRIDGE). */
function runs(values: (number | null)[]): [number, number][][] {
  const out: [number, number][][] = [];
  let current: [number, number][] = [];
  let missing = 0;
  values.forEach((v, i) => {
    if (v === null || !Number.isFinite(v)) {
      missing += 1;
      return;
    }
    if (current.length && missing > BRIDGE) {
      out.push(current);
      current = [];
    }
    missing = 0;
    current.push([i, v]);
  });
  if (current.length) out.push(current);
  return out;
}

/** Line + closed area path strings for one channel. `y` maps value → 0…100. */
function paths(values: (number | null)[], y: (v: number) => number, base: number) {
  const line: string[] = [];
  const area: string[] = [];
  // Slot i sits at x = i + 1, so the newest point starts one slot beyond the
  // right edge and slides in as the phase advances (viewBox starts at x = 1).
  for (const run of runs(values)) {
    const pts = run.map(([i, v]) => `${i + 1},${y(v).toFixed(2)}`);
    const d = `M${pts.join(" L")}`;
    // A zero-length round-capped segment makes an isolated sample visible.
    line.push(run.length === 1 ? `${d} l0.001,0` : d);
    const first = run[0][0] + 1;
    const last = run[run.length - 1][0] + 1;
    area.push(`${d} L${last},${base} L${first},${base} Z`);
  }
  return { line: line.join(" "), area: area.join(" ") };
}

/** Mount a sparkline of the given kind; `label` is its accessible name. */
export function sparkline(kind: SparkKind, label: string, cls = ""): Sparkline {
  const el = document.createElement("div");
  el.className = `spark spark-${kind} ${cls}`.trim();
  el.setAttribute("role", "img");
  el.setAttribute("aria-label", label);
  const root = svg("svg", {
    viewBox: `1 0 ${SLOTS - 1} 100`,
    preserveAspectRatio: "none",
    "aria-hidden": "true",
  });
  const group = svg("g");
  const channels = (kind === "duplex" ? ["down", "up"] : ["main"]).map((name) => {
    const area = svg("path", { class: `spark-area ${name}` });
    const line = svg("path", { class: `spark-line ${name}`, "vector-effect": "non-scaling-stroke" });
    group.append(area, line);
    return { area, line };
  });
  root.append(group);
  el.append(root);
  const entry = { group, el };
  live.add(entry);

  // Duplex auto-scale state: grow at once, shrink only after a quiet hold.
  let ceiling = 0;
  let holdUntil = 0;

  const update = (history: Series | DuplexSeries) => {
    if (!live.has(entry)) live.add(entry);
    if (kind === "duplex") {
      const h = history as DuplexSeries;
      const down = h.map((s) => (s ? s[0] : null));
      const up = h.map((s) => (s ? s[1] : null));
      const peak = Math.max(DUPLEX_FLOOR, ...down.map((v) => v ?? 0), ...up.map((v) => v ?? 0));
      const now = performance.now();
      if (ceiling === 0 || peak > ceiling || (peak < ceiling / 4 && now >= holdUntil)) {
        ceiling = pow2(peak);
        holdUntil = now + SHRINK_HOLD_MS;
      }
      const frac = (v: number) => Math.max(0, Math.min(1, v / ceiling)) * 47;
      // Download/read rises above the midline; upload/write hangs below it.
      const d = paths(down, (v) => 50 - frac(v), 50);
      const u = paths(up, (v) => 50 + frac(v), 50);
      channels[0].line.setAttribute("d", d.line);
      channels[0].area.setAttribute("d", d.area);
      channels[1].line.setAttribute("d", u.line);
      channels[1].area.setAttribute("d", u.area);
    } else {
      const h = history as Series;
      let top = 100;
      if (kind === "temperature") {
        const peak = Math.max(100, ...h.map((v) => v ?? 0));
        top = Math.ceil(peak / 20) * 20;
      }
      const p = paths(h, (v) => 98 - Math.max(0, Math.min(1, v / top)) * 96, 100);
      channels[0].line.setAttribute("d", p.line);
      channels[0].area.setAttribute("d", p.area);
    }
    paint(performance.now());
  };
  return { el, update };
}
