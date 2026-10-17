// Progress card shared by every long-running surface (Clean's scan, Analyze's
// walk, Optimize's task run): an icon inside a spinning ring, the item being
// worked on, an activity bar, three live counters and an optional cancel
// button. Layout and motion follow MangoDisk's operation-progress card,
// re-inked in the newspaper palette. By default the counters come from the
// backend's process-wide scan meter, polled — not pushed — so every parallel
// scan feeds one display without each IPC command growing its own progress
// channel; surfaces that walk no files turn the meter off and set the first
// two tiles themselves.

import { scanMeter } from "./ipc";
import { esc, humanBytes } from "./format";
import { t } from "./i18n";
import type { ScanMeter } from "./types";

/** Lucide icons (ISC), drawn at the ring's centre. */
const ICONS = {
  /** "brush-cleaning": Clean. */
  broom:
    '<path d="m16 22-1-4"/><path d="M19 14a1 1 0 0 0 1-1v-1a2 2 0 0 0-2-2h-3a1 1 0 0 1-1-1V4a2 2 0 0 0-4 0v5a1 1 0 0 1-1 1H6a2 2 0 0 0-2 2v1a1 1 0 0 0 1 1"/><path d="M19 14H5l-1.973 6.767A1 1 0 0 0 4 22h16a1 1 0 0 0 .973-1.233z"/><path d="m8 22 1-4"/>',
  /** "chart-pie": Analyze. */
  chart:
    '<path d="M21 12c.552 0 1.005-.449.95-.998a10 10 0 0 0-8.953-8.951c-.55-.055-.998.398-.998.95v8a1 1 0 0 0 1 1z"/><path d="M21.21 15.89A10 10 0 1 1 8 2.83"/>',
  /** "gauge": Optimize. */
  gauge: '<path d="m12 14 4-4"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/>',
} as const;

/** Path card's leading icon (Lucide, ISC): a folder while files are walked,
 * a wrench while tasks run. */
const LEAD_ICONS = {
  folder:
    '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>',
  wrench:
    '<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/>',
} as const;

/** How often the meter is read: fast enough to read as motion, slow enough
 * that the IPC round trips stay negligible next to the scan itself. */
const POLL_MS = 250;

/** Characters kept whole at the end of a long path (the file or folder name
 * matters more than the middle of the tree). */
const PATH_TAIL = 24;

export interface ScanCardOptions {
  /** Home dir for ~-abbreviated paths. */
  home?: string;
  /** Omit for work that cannot be cancelled: the button is not drawn. */
  onCancel?: () => void;
  icon?: keyof typeof ICONS;
  /** Small rust kicker above the title (default "正在分析中"). */
  stage?: string;
  /** Heading (default "正在分析可清理内容…"). */
  title?: string;
  /** Label over the path line (default "正在扫描"). */
  pathLabel?: string;
  /** Line under the counters (default: the macOS permission hint). */
  hint?: string;
  /** Epoch ms the work started; a card re-rendered mid-scan keeps its clock. */
  startedAt?: number;
  /** false = no meter polling; the caller fills tiles 0 and 1 itself. */
  meter?: boolean;
  /** Meter zero point carried across re-renders of the same scan. */
  baseline?: ScanMeter | null;
  /** Receives the zero point once it is read, so the caller can keep it. */
  onBaseline?: (meter: ScanMeter) => void;
}

export interface ScanCard {
  /** Path line text. With the meter on it is the fallback until the meter
   * reports a directory (and while a stage walks nothing, e.g. waiting on a
   * CLI tool); with the meter off it is the line itself. */
  setLabel(label: string): void;
  /** Determinate progress 0–1; null returns to the sweeping activity bar. */
  setProgress(fraction: number | null): void;
  /** Overwrite counter tile 0 or 1 (meter-off surfaces). */
  setTile(index: 0 | 1, label: string, value: string): void;
  /** Stop polling and the clock; the caller then replaces the card. */
  stop(): void;
}

/** Render the progress card into `host` and start its live counters. */
export function renderScanCard(host: HTMLElement, opts: ScanCardOptions): ScanCard {
  const title = opts.title ?? t("scan.title");
  const useMeter = opts.meter !== false;
  host.innerHTML = `
    <div class="scan-stage">
      <section class="scan-card" aria-busy="true" aria-label="${esc(title)}">
        <div class="scan-heading">
          <span class="scan-icon"><svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[opts.icon ?? "broom"]}</svg><i class="scan-spinner"></i></span>
          <div class="scan-heading-text">
            <span class="scan-stage-label">${esc(opts.stage ?? t("scan.stage"))}</span>
            <h2>${esc(title)}</h2>
          </div>
        </div>
        <div class="scan-path-card">
          <span class="scan-folder"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${LEAD_ICONS[useMeter ? "folder" : "wrench"]}</svg></span>
          <span class="scan-path-content">
            <small class="scan-path-meta">
              <span>${esc(opts.pathLabel ?? t("scan.current"))}</span>
              <span data-scan="clock-inline"></span>
            </small>
            <strong class="scan-path-line" data-scan="path" title="">
              <span class="head"></span><span class="tail">${esc(t("scan.preparing"))}</span>
            </strong>
          </span>
        </div>
        <div class="scan-track"><span></span></div>
        <div class="scan-stats">
          <span><small data-scan="label0">${esc(t("scan.items"))}</small><strong data-scan="items">0</strong></span>
          <span><small data-scan="label1">${esc(t("scan.bytes"))}</small><strong data-scan="bytes">0B</strong></span>
          <span><small>${esc(t("scan.elapsed"))}</small><strong data-scan="clock"></strong></span>
        </div>
        <p class="scan-hint">${esc(opts.hint ?? t("scan.hint"))}</p>
        ${opts.onCancel ? `<button class="scan-cancel" type="button">${esc(t("flow.cancel"))}</button>` : ""}
      </section>
    </div>`;

  const q = (key: string) => host.querySelector<HTMLElement>(`[data-scan="${key}"]`)!;
  const pathEl = q("path");
  const tileValues = [q("items"), q("bytes")];
  const tileLabels = [q("label0"), q("label1")];
  const clockEl = q("clock");
  const clockInlineEl = q("clock-inline");
  const track = host.querySelector<HTMLElement>(".scan-track")!;
  const bar = track.querySelector<HTMLElement>("span")!;

  const startedAt = opts.startedAt ?? Date.now();
  let stopped = false;
  let baseline: ScanMeter | null = opts.baseline ?? null;
  let shownPath = "";
  let label = "";

  /** Write a path into the two-part line: the head ellipsizes, the tail stays. */
  const paintPath = (raw: string) => {
    const path = tildify(raw, opts.home);
    if (path === shownPath) return;
    shownPath = path;
    const cut = Math.max(0, path.length - PATH_TAIL);
    pathEl.querySelector(".head")!.textContent = path.slice(0, cut);
    pathEl.querySelector(".tail")!.textContent = path.slice(cut);
    pathEl.title = path;
  };

  const paintClock = () => {
    const text = elapsedText(Date.now() - startedAt);
    clockEl.textContent = text;
    clockInlineEl.textContent = text;
  };

  // The meter is monotonic and process-wide: the first reading is the zero
  // point, and only growth after it belongs to this scan.
  const poll = async () => {
    if (stopped) return;
    try {
      const m = await scanMeter();
      if (stopped) return;
      if (!baseline) {
        baseline = m;
        opts.onBaseline?.(m);
      }
      tileValues[0].textContent = Math.max(0, m.items - baseline.items).toLocaleString();
      tileValues[1].textContent = humanBytes(Math.max(0, m.bytes - baseline.bytes));
      // A path seen before this scan started is someone else's walk.
      if (m.path && (m.items > baseline.items || m.path !== baseline.path)) paintPath(m.path);
      else if (label) paintPath(label);
    } catch {
      // A failed read only skips one frame; the scan itself is unaffected.
    }
  };

  let pollTimer: number | undefined;
  if (useMeter) {
    void poll();
    pollTimer = window.setInterval(() => void poll(), POLL_MS);
  }
  // The clock ticks locally so the card never looks frozen, even while a
  // stage waits on a CLI tool and the meter stands still.
  const clockTimer = window.setInterval(paintClock, 1000);
  paintClock();

  const cancelBtn = host.querySelector<HTMLButtonElement>(".scan-cancel");
  cancelBtn?.addEventListener("click", () => {
    cancelBtn.disabled = true;
    opts.onCancel?.();
  });

  return {
    setLabel(next: string) {
      label = next;
      if (!useMeter || !shownPath) paintPath(next);
    },
    setProgress(fraction: number | null) {
      track.classList.toggle("determinate", fraction !== null);
      bar.style.width = fraction === null ? "" : `${Math.round(Math.min(1, Math.max(0, fraction)) * 100)}%`;
    },
    setTile(index: 0 | 1, tileLabel: string, value: string) {
      tileLabels[index].textContent = tileLabel;
      tileValues[index].textContent = value;
    },
    stop() {
      stopped = true;
      if (pollTimer !== undefined) window.clearInterval(pollTimer);
      window.clearInterval(clockTimer);
    },
  };
}

/** Elapsed time as the card shows it: "12 秒", then "3 分 05 秒". */
function elapsedText(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  if (total < 60) return t("scan.seconds", { s: total });
  return t("scan.minutes", { m: Math.floor(total / 60), s: String(total % 60).padStart(2, "0") });
}

/** Abbreviate the home prefix to ~. */
function tildify(path: string, home?: string): string {
  if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}
