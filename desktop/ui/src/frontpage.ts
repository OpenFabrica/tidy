// The front page shared by the three idle screens (清理 / 优化 / 分析).
//
// Tabloid layout (redesign B): the newspaper character lives in the page
// structure, not in giant type. A folio line (issue · strapline), a ticker
// band carrying the section's ledger figures, then a module grid: the lead
// story (A1 — capped headline with one word in rust, ruled byline, the solid
// call to action and its note) beside the "编者按" module with a drop cap.
// Only the copy and the numbers differ per section, so the shell lives here
// once.

import { esc } from "./format";
import { t } from "./i18n";

/** One ledger figure; rendered as a cell of the ticker band. */
export interface FrontStat {
  label: string;
  value: string;
}

export interface FrontPageOptions {
  /** Folio line at the top left, e.g. "第 47 期 · 智能清理专栏". */
  kicker: string;
  /** Muted folio line at the top right. */
  strapline: string;
  /** Kept for API compatibility. The tabloid layout prints no plate
   * numeral: the issue number already lives in the folio. */
  watermark: string;
  /** Kept for API compatibility (watermark geometry); ignored. */
  watermarkWide?: boolean;
  /** Display headline. Wrap the accented word in <em> — it turns rust. */
  headlineHtml: string;
  /** Byline desk, e.g. "本刊编辑部 · 磁盘管理组". */
  desk: string;
  /** Monospaced dateline beside the byline. */
  dateline: string;
  /** Optional block between the byline and the action (the scope row). */
  extraHtml?: string;
  /** Action verb; the arrow is added here. */
  action: string;
  /** Short mono note printed beside the action. */
  actionNote: string;
  /** Makes that note clickable (analyze uses it to reopen a cached result). */
  onActionNote?: () => void;
  /** Standfirst prose. Its first character is set as the drop cap. */
  noteBody: string;
  /** Ledger figures shown in the ticker band under the folio. */
  stats: FrontStat[];
}

/**
 * The section's page letter (A 清理, C 优化, D 分析…), read from the active
 * masthead tab so module codes (A1, A2…) always match the tab's page code
 * without every caller passing it. Falls back to "A" before the masthead
 * exists.
 */
export function pageLetter(): string {
  const pg = document.querySelector(".mast-tab.on .pg")?.textContent?.trim() ?? "";
  return /^[A-Z]/.test(pg) ? pg[0] : "A";
}

/** Module eyebrow: page code, title (may be empty) and an optional aside. */
export function kickHtml(n: number, title: string, asideHtml = ""): string {
  return `<div class="kick"><span class="code">${pageLetter()}${n}</span><span class="t">${esc(
    title,
  )}</span>${asideHtml}</div>`;
}

/**
 * A ledger value as a serif figure: "8.42GB" → 8.42 + small "GB",
 * "14 天前" → 14 + small "天前". Values whose tail still carries digits
 * (a timestamp such as "11月3日 20:56") or that start with a word stay whole.
 */
export function figureHtml(value: string): string {
  const m = /^([\d][\d.,]*)\s*(\D*)$/.exec(value);
  if (!m || !m[2]) return esc(value);
  return `${esc(m[1])}<small>${esc(m[2])}</small>`;
}

/**
 * Render the front page into `container` and return its action button so the
 * caller can wire the scan/check/analyse it starts.
 */
export function renderFrontPage(
  container: HTMLElement,
  opts: FrontPageOptions,
): HTMLButtonElement {
  // A missing key comes back verbatim from t(); the lead module then shows
  // only its page code instead of a raw key.
  const leadTitle = t("front.lead") === "front.lead" ? "" : t("front.lead");

  container.innerHTML = `
    <div class="page front">
      <div class="folio">
        <span class="issue">${esc(opts.kicker)}</span>
        <span>${esc(opts.strapline)}</span>
      </div>
      ${
        opts.stats.length > 0
          ? `<section class="band front-band">
              <div class="ticker">
                ${opts.stats
                  .map(
                    (stat) => `<div class="tick">
                      <div class="tick-k">${esc(stat.label)}</div>
                      <div class="tick-v">${figureHtml(stat.value)}</div>
                    </div>`,
                  )
                  .join("")}
              </div>
            </section>`
          : ""
      }
      <div class="grid front-grid">
        <article class="mod span-2 front-lead">
          ${kickHtml(1, leadTitle)}
          <h1 class="headline front-headline">${opts.headlineHtml}</h1>
          <div class="byline">
            <span class="desk">${esc(opts.desk)}</span>
            <span class="date">${esc(opts.dateline)}</span>
          </div>
          ${opts.extraHtml ?? ""}
          <div class="act-row front-action">
            <button class="btn lg" id="front-action">${esc(opts.action)} →</button>
            ${
              opts.onActionNote
                ? `<button class="front-action-note note link plain" id="front-action-note">${esc(opts.actionNote)}</button>`
                : `<span class="front-action-note note">${esc(opts.actionNote)}</span>`
            }
          </div>
        </article>
        <aside class="mod span-all-narrow front-note">
          ${kickHtml(2, t("front.note"))}
          <p class="prose dropcap">${esc(opts.noteBody)}</p>
        </aside>
      </div>
    </div>`;

  container
    .querySelector<HTMLButtonElement>("#front-action-note")
    ?.addEventListener("click", () => opts.onActionNote?.());

  return container.querySelector<HTMLButtonElement>("#front-action")!;
}
