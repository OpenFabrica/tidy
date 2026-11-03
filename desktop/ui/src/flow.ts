// The shared destructive-flow skeleton, set as a tabloid page (redesign B):
// scanning → ledger preview (databox with the reclaimable figure and the
// action, a full-width ledger module, side modules) → confirm sheet → live
// execution card → reclaimed-space page. One confirmation pattern for
// every destructive surface.
// Performance contract: checkbox toggles patch section rows and the subtotal
// in place — the ledger never re-renders wholesale.

import { cancelTask, executePlan, revealInFinder } from "./ipc";
import { esc, humanKb, splitUnit } from "./format";
import { figureHtml, kickHtml } from "./frontpage";
import { daysSince, lastReclaim, recordReclaim, totalFreedKb } from "./ledger";
import { relativeDay, t } from "./i18n";
import { renderScanCard } from "./scancard";
import { stopwatch, track } from "./telemetry";
import type { ExecItem, ExecReport, PlanItem, PlanSection, PlanSummary } from "./types";

export interface FlowOptions {
  title: string;
  /** Folio line on every stage (e.g. "第 47 期 · 智能清理专栏"); omitted =
   * the title (preview) or nothing (scan card). */
  issue?: string;
  /** Kick off the scan; receives a progress-label setter. A view may return
   * several plans (clean + purge + installer) — each keeps its own backend
   * plan/execute funnel; the flow only merges their sections for display. */
  plan: (
    taskId: string,
    onLabel: (label: string) => void,
  ) => Promise<PlanSummary | PlanSummary[]>;
  /** Extra cancel hook for plans scanning under additional task ids. */
  onCancel?: () => void;
  /** Verb shown on buttons and the confirm sheet. */
  verb: string;
  /** Optional hint line under the header (blocked-apps notice, blockers). */
  subtitleHtml?: () => string;
  helperAvailable: boolean;
  /** Home dir for ~-abbreviated paths in item rows. */
  home?: string;
  /** Clean-only: route deletes to Trash instead of permanent removal. */
  trashOverride?: boolean;
  /** Which surface to credit in the local reclaim ledger. */
  ledger?: "clean" | "apps" | "analyze";
  /** Items to leave unchecked on first render (e.g. deps of a project
   * edited this week). The user can still tick them. */
  defaultUnchecked?: (item: PlanItem, sectionTitle: string) => boolean;
  /** Extra badge HTML for an item row (already escaped by the caller). */
  itemBadge?: (item: PlanItem, sectionTitle: string) => string;
  /** Extra warning line for the confirm sheet, computed from the checked
   * items; empty string = no line. */
  confirmNote?: (selected: PlanItem[]) => string;
  /** Per-plan executor override (e.g. Docker items go through the docker
   * CLI). Return undefined to use the default file-deletion funnel. */
  execute?: (job: ExecJob, onItem: (item: ExecItem) => void) => Promise<ExecReport> | undefined;
  /** Hook run after the reclaimed-space page renders, with what was freed
   * and how much of it only moved to the Trash (not yet freed on disk). */
  afterDone?: (body: HTMLElement, result: { freedKb: number; trashedKb: number }) => void;
}

/** Match the software list's outlined disclosure icon. The SVG rotates as a
 * whole so collapsed and expanded states keep identical stroke geometry. */
const CHEVRON_SVG =
  '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6l4 4 4-4"/></svg>';

/** Render the flow into a container (fresh instance per mount). */
export function renderFlow(container: HTMLElement, taskId: string, opts: FlowOptions): void {
  container.innerHTML = `
    <div id="flow-hero"></div>
    <div id="flow-body"></div>`;

  const body = container.querySelector<HTMLElement>("#flow-body")!;
  const hero = container.querySelector<HTMLElement>("#flow-hero")!;
  // A user cancel is not an error: the view goes back to how it looks on
  // entry instead of printing the backend's "scan cancelled" message.
  let cancelled = false;
  const card = renderScanCard(hero, {
    home: opts.home,
    folio: opts.issue,
    onCancel: () => {
      cancelled = true;
      void cancelTask(taskId);
      opts.onCancel?.();
    },
  });

  // 扫描与执行的埋点都收在这里：clean / apps / analyze 三个界面共用这套骨架，
  // 埋在各自的 view 里迟早会漏掉一个分支，也会各写各的口径。
  const scan = stopwatch();
  opts
    .plan(taskId, (label) => card.setLabel(label))
    .then((result) => {
      card.stop();
      track({ kind: "scan_completed", scan: surfaceOf(opts), duration_ms: scan() });
      hero.remove();
      renderPreview(body, Array.isArray(result) ? result : [result], opts);
    })
    .catch((e) => {
      card.stop();
      if (cancelled || (e as { code?: unknown } | null)?.code === "cancelled") {
        // Re-mounting the current route renders its initial state.
        window.dispatchEvent(new HashChangeEvent("hashchange"));
        return;
      }
      hero.innerHTML = `<div class="page"><div class="placeholder">${esc(String(e?.message ?? e))}</div></div>`;
    });
}

/** 埋点用的界面名。ledger 已经是这三个面的稳定标识，不再另立一套。 */
function surfaceOf(opts: Pick<FlowOptions, "ledger">): string {
  return opts.ledger ?? "clean";
}

/** Section title/desc from i18n keys, falling back to the raw title. */
function sectionMeta(title: string): { name: string; desc: string } {
  const name = t(`sec.${title}.title`);
  if (name !== `sec.${title}.title`) return { name, desc: t(`sec.${title}.desc`) };
  return { name: title, desc: "" };
}

/** Abbreviate the home prefix to ~ for item rows. */
function tildify(path: string, home?: string): string {
  if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

/** Display name for an item: its final path component. */
function displayName(path: string): string {
  return path.replace(/\/$/, "").split("/").pop() ?? path;
}

/** Folio line for the flow's pages: the issue (when the view has one) on
 * the left, a stage word on the right. */
function folioHtml(issue: string | undefined, stage: string): string {
  return issue
    ? `<div class="folio"><span class="issue">${esc(issue)}</span><span>${esc(stage)}</span></div>`
    : `<div class="folio"><span class="issue">${esc(stage)}</span></div>`;
}

/** Split "清理规则：…" into the eyebrow title and the prose after the colon,
 * so the rules module reads like the reference without new copy. */
function splitRules(text: string): [string, string] {
  const m = /^([^：:]{1,16})[：:]\s*(.+)$/s.exec(text);
  return m ? [m[1], m[2]] : ["", text];
}

/** Stage 2: the ledger page — databox (reclaimable figure, lede, action),
 * the ledger module, and the side modules (last clean, lifetime, rules). */
function renderPreview(body: HTMLElement, summaries: PlanSummary[], opts: FlowOptions): void {
  if (summaries.reduce((n, s) => n + s.count, 0) === 0) {
    body.innerHTML = `
      <div class="page flow-empty">
        ${folioHtml(opts.issue, opts.title)}
        <section class="mod">
          ${kickHtml(1, opts.title)}
          <h1 class="headline">${t("flow.nothing")}</h1>
        </section>
      </div>`;
    return;
  }

  // Flattened sections across all plans; planOf[idx] names the owning plan
  // (item ids are only unique per plan, so grouping stays plan-scoped).
  const sections = summaries.flatMap((s) => s.sections);
  const planOf = summaries.flatMap((s) => s.sections.map(() => s.plan_id));
  const grandKb = sections.reduce((sum, s) => sum + s.total_kb, 0);

  // Selection state: per section, the set of checked item ids.
  const checked: Set<string>[] = sections.map((section) =>
    new Set(
      section.items
        .filter((i) => !(i.scope === "system" && !opts.helperAvailable))
        .filter((i) => !opts.defaultUnchecked?.(i, section.title))
        .map((i) => i.id),
    ),
  );

  const rows = sections
    .map((section, idx) => {
      const meta = sectionMeta(section.title);
      return `<div class="clean-card" data-card="${idx}">
        <div class="sec">
          <input type="checkbox" class="sec-check" data-seccheck="${idx}" />
          <div class="sec-main">
            <div class="sec-title"><b class="sec-name">${esc(meta.name)}</b><span class="cnt" data-seccount="${idx}"></span></div>
            ${meta.desc ? `<div class="sec-desc" title="${esc(meta.desc)}">${esc(meta.desc)}</div>` : ""}
          </div>
          <span class="sec-size" data-secsize="${idx}"></span>
          <button class="chev" data-chev="${idx}" title="${t("prev.expand")}">${CHEVRON_SVG}</button>
        </div>
        <div class="sec-items" data-items="${idx}" hidden></div>
      </div>`;
    })
    .join("");

  const previous = lastReclaim(opts.ledger ?? "clean");
  const lifetime = totalFreedKb();

  const [rulesTitle, rulesBody] = splitRules(
    opts.trashOverride ? t("prev.side.rulesTrash") : t("prev.side.rules"),
  );

  body.innerHTML = `
    <div class="page flow-ledger">
      ${folioHtml(opts.issue, opts.title)}
      <section class="databox">
        <div class="cell">
          <div class="cell-k">${t("prev.willFree")}</div>
          <div class="fig" id="sel-total"></div>
          <div class="sel-share">
            <div class="meter"><i id="sel-meter"></i></div>
            <div class="note">${esc(t("prev.ofTotal", { size: humanKb(grandKb) }))}</div>
          </div>
        </div>
        <div class="cell lede">
          <p class="prose">${t("prev.lede")}</p>
          ${opts.subtitleHtml?.() ?? ""}
        </div>
        <div class="cell cell-actions">
          <button class="btn lg" id="go"></button>
          <div class="pair">
            <a class="link plain" href="#" id="sel-all">${t("prev.selectall")}</a><span class="sep">/</span><a class="link plain" href="#" id="sel-none">${t("prev.clear")}</a>
            <span class="sep">·</span>
            <button class="link plain" id="expand-all">${t("prev.expandAll")}</button>
            <span class="sep">·</span>
            <button class="link plain" id="close-preview">${t("flow.cancel")}</button>
          </div>
        </div>
      </section>
      <div class="grid split">
        <section class="mod ledger-mod">
          ${kickHtml(2, t("prev.accounts"), `<span class="aside" id="kick-sum"></span>`)}
          ${rows}
          <div class="tr sum c2">
            <span class="ell" id="foot-count"></span>
            <span class="num foot-size" id="foot-size"></span>
          </div>
        </section>
        <div class="side">
          <section class="mod">
            ${kickHtml(3, t("prev.side.last"))}
            <div class="fig sm">${previous ? figureHtml(relativeDay(daysSince(previous.at))) : "—"}</div>
          </section>
          <section class="mod">
            ${kickHtml(4, t("prev.side.total"))}
            <div class="fig sm">${lifetime > 0 ? figureHtml(humanKb(lifetime)) : "—"}</div>
          </section>
          <section class="mod">
            ${kickHtml(5, rulesTitle)}
            <p class="prose sm">${esc(rulesBody)}</p>
          </section>
        </div>
      </div>
    </div>`;

  body.querySelector("#close-preview")!.addEventListener("click", () => {
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  });

  /** Patch one section row (count, sizes, tri-state) in place. */
  const patchSection = (idx: number) => {
    const section = sections[idx];
    const set = checked[idx];
    const selKb = section.items.reduce(
      (sum, i) => sum + (set.has(i.id) ? (i.size_kb ?? 0) : 0),
      0,
    );
    body.querySelector(`[data-seccount="${idx}"]`)!.textContent =
      ` ${t("prev.selected")} ${set.size}/${section.items.length}`;
    const sizeEl = body.querySelector<HTMLElement>(`[data-secsize="${idx}"]`)!;
    // Partial selections print the full size muted: "1.10GB / 3.21GB".
    sizeEl.innerHTML =
      set.size === section.items.length
        ? esc(humanKb(section.total_kb))
        : `${esc(humanKb(selKb))} <span class="of">/ ${esc(humanKb(section.total_kb))}</span>`;
    const cb = body.querySelector<HTMLInputElement>(`[data-seccheck="${idx}"]`)!;
    cb.checked = set.size === section.items.length && set.size > 0;
    cb.indeterminate = set.size > 0 && set.size < section.items.length;
  };

  /** Patch the subtotal row and the action label in place. */
  const patchFooter = () => {
    let count = 0;
    let kb = 0;
    sections.forEach((section, idx) => {
      const set = checked[idx];
      count += set.size;
      for (const item of section.items) {
        if (set.has(item.id)) kb += item.size_kb ?? 0;
      }
    });
    body.querySelector("#foot-count")!.textContent =
      `${t("prev.subtotal")} · ${t("prev.selected")} ${count} ${t("prev.items")}`;
    body.querySelector("#foot-size")!.textContent = humanKb(kb);
    body.querySelector("#kick-sum")!.textContent =
      `${t("prev.selected")} ${count} ${t("prev.items")} · ${humanKb(kb)}`;
    // The headline figure is what this run will free (the checked items);
    // the full reclaimable total stays underneath as the meter's scale.
    const [selValue, selUnit] = splitUnit(humanKb(kb));
    body.querySelector("#sel-total")!.innerHTML = `${esc(selValue)}<small>${esc(selUnit)}</small>`;
    body.querySelector<HTMLElement>("#sel-meter")!.style.width =
      `${grandKb > 0 ? Math.min(100, (kb / grandKb) * 100) : 0}%`;
    const go = body.querySelector<HTMLButtonElement>("#go")!;
    go.textContent = `${opts.verb} →`;
    go.disabled = count === 0;
  };

  const patchAll = () => {
    sections.forEach((_, idx) => patchSection(idx));
    patchFooter();
  };
  patchAll();

  /** Render the expandable item rows of one section (once, on first expand). */
  const renderItems = (idx: number) => {
    const holder = body.querySelector<HTMLElement>(`[data-items="${idx}"]`)!;
    if (holder.dataset.rendered === "1") {
      syncItemChecks(idx);
      return;
    }
    holder.dataset.rendered = "1";
    const section = sections[idx];
    holder.innerHTML = section.items
      .map((item) => {
        const gated = item.scope === "system" && !opts.helperAvailable;
        const badges = `${opts.itemBadge?.(item, section.title) ?? ""}${
          gated ? `<span class="badge warn">${t("flow.admin")}</span>` : ""
        }`;
        return `<div class="item">
          <input type="checkbox" data-item="${esc(item.id)}" data-sec="${idx}"
            ${checked[idx].has(item.id) ? "checked" : ""} ${gated ? "disabled" : ""} />
          <span class="item-main">
            <b class="iname" title="${esc(displayName(item.path))}">${esc(displayName(item.path))}</b>
            <span class="path" title="${esc(item.path)}">${esc(tildify(item.path, opts.home))}</span>
          </span>
          <span class="item-meta">${
            item.item_count > 1 ? `<span class="num muted">${item.item_count} ${t("prev.items")}</span>` : ""
          }${badges}</span>
          <span class="num isize">${item.size_kb === null ? "?" : humanKb(item.size_kb)}</span>
          <button class="reveal" data-reveal="${esc(item.path)}" title="Finder">↗</button>
        </div>`;
      })
      .join("");

    holder.querySelectorAll<HTMLInputElement>("input[data-item]").forEach((cb) => {
      cb.addEventListener("change", () => {
        const set = checked[Number(cb.dataset.sec)];
        if (cb.checked) set.add(cb.dataset.item!);
        else set.delete(cb.dataset.item!);
        patchSection(Number(cb.dataset.sec));
        patchFooter();
      });
    });
    holder.querySelectorAll<HTMLButtonElement>("button[data-reveal]").forEach((btn) => {
      btn.addEventListener("click", () => void revealInFinder(btn.dataset.reveal!));
    });
  };

  /** Sync rendered item checkboxes to the state after bulk toggles. */
  const syncItemChecks = (idx: number) => {
    body
      .querySelectorAll<HTMLInputElement>(`input[data-item][data-sec="${idx}"]`)
      .forEach((cb) => {
        if (!cb.disabled) cb.checked = checked[idx].has(cb.dataset.item!);
      });
  };

  /** Open (or close) one section's item list. */
  const setExpanded = (idx: number, open: boolean) => {
    const holder = body.querySelector<HTMLElement>(`[data-items="${idx}"]`)!;
    const btn = body.querySelector<HTMLElement>(`[data-chev="${idx}"]`)!;
    holder.hidden = !open;
    btn.classList.toggle("open", open);
    if (open) renderItems(idx);
  };

  // Wire section checkboxes, chevrons, and bulk links.
  body.querySelectorAll<HTMLInputElement>("input[data-seccheck]").forEach((cb) => {
    cb.addEventListener("change", () => {
      const idx = Number(cb.dataset.seccheck);
      const section = sections[idx];
      checked[idx] = cb.checked
        ? new Set(
            section.items
              .filter((i) => !(i.scope === "system" && !opts.helperAvailable))
              .map((i) => i.id),
          )
        : new Set();
      syncItemChecks(idx);
      patchSection(idx);
      patchFooter();
    });
  });
  body.querySelectorAll<HTMLButtonElement>("button[data-chev]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const idx = Number(btn.dataset.chev);
      setExpanded(idx, body.querySelector<HTMLElement>(`[data-items="${idx}"]`)!.hidden);
    });
  });
  body.querySelector("#expand-all")!.addEventListener("click", () => {
    sections.forEach((_, idx) => setExpanded(idx, true));
  });
  body.querySelector("#sel-all")!.addEventListener("click", (ev) => {
    ev.preventDefault();
    sections.forEach((section, idx) => {
      checked[idx] = new Set(
        section.items
          .filter((i) => !(i.scope === "system" && !opts.helperAvailable))
          .map((i) => i.id),
      );
      syncItemChecks(idx);
    });
    patchAll();
  });
  body.querySelector("#sel-none")!.addEventListener("click", (ev) => {
    ev.preventDefault();
    sections.forEach((_, idx) => {
      checked[idx] = new Set();
      syncItemChecks(idx);
    });
    patchAll();
  });

  body.querySelector("#go")!.addEventListener("click", () => {
    // Group the checked ids back by owning plan: each plan executes through
    // its own two-phase funnel, the flow only aggregates the reports.
    const byPlan = new Map<string, string[]>();
    const selected: PlanItem[] = [];
    let count = 0;
    let kb = 0;
    sections.forEach((section, idx) => {
      const set = checked[idx];
      if (set.size === 0) return;
      count += set.size;
      for (const item of section.items) {
        if (set.has(item.id)) {
          kb += item.size_kb ?? 0;
          selected.push(item);
        }
      }
      byPlan.set(planOf[idx], [...(byPlan.get(planOf[idx]) ?? []), ...set]);
    });
    const jobs = [...byPlan.entries()].map(([planId, selection]) => ({ planId, selection }));
    confirmSheet(
      count,
      kb,
      opts.verb,
      () => runExecution(body, jobs, opts),
      opts.confirmNote?.(selected),
    );
  });
}

/** The one confirm sheet, shared by every destructive surface. */
export function confirmSheet(
  count: number,
  totalKb: number,
  verb: string,
  onConfirm: () => void,
  note?: string,
): void {
  const sheet = document.createElement("div");
  sheet.className = "confirm-backdrop";
  sheet.innerHTML = `
    <div class="confirm-sheet" role="dialog" aria-modal="true">
      <h2 class="title">${esc(t("flow.confirm.title", { n: count }))}</h2>
      <p class="note confirm-sub">${esc(t("flow.confirm.sub", { size: humanKb(totalKb) }))}</p>
      ${note ? `<p class="confirm-note">${esc(note)}</p>` : ""}
      <div class="confirm-actions">
        <button class="link" id="confirm-cancel">${t("flow.cancel")}</button>
        <button class="btn" id="confirm-go">${esc(verb)} →</button>
      </div>
    </div>`;
  document.body.appendChild(sheet);
  sheet.querySelector("#confirm-cancel")!.addEventListener("click", () => sheet.remove());
  sheet.querySelector("#confirm-go")!.addEventListener("click", () => {
    sheet.remove();
    onConfirm();
  });
}

/** One executable unit: a stored plan id plus its checked selection. */
export interface ExecJob {
  planId: string;
  selection: string[];
}

/** Stage 4: the shared progress card (same look as the scan), then the
 * reclaimed-space page. Plans execute strictly one after another (each
 * through its own two-phase funnel); the closing page shows the aggregated
 * report and lists only the items that did not go through, with a reason. */
export function runExecution(
  body: HTMLElement,
  jobs: ExecJob[],
  opts: Pick<FlowOptions, "trashOverride" | "execute" | "ledger" | "afterDone" | "home" | "issue">,
): void {
  // Deletion cannot be interrupted midway (each item is one atomic sink
  // call), so the card has no cancel button; the meter is off because
  // progress comes from per-item callbacks, not from a file walk.
  const card = renderScanCard(body, {
    home: opts.home,
    folio: opts.issue,
    stage: t("flow.working"),
    title: t("flow.exec.title"),
    pathLabel: t("flow.exec.current"),
    hint: t("flow.exec.hint"),
    meter: false,
  });
  const total = jobs.reduce((n, j) => n + j.selection.length, 0);
  let done = 0;
  let freedSoFar = 0;
  // Bytes that only moved to the Trash: they count as "freed" in the report
  // but stay on disk until the Trash is emptied, so the done page says so.
  let trashedKb = 0;
  const problems: ExecItem[] = [];

  const paint = () => {
    card.setTile(0, t("flow.exec.progress"), `${done} / ${total}`);
    card.setTile(1, t("flow.exec.freed"), humanKb(freedSoFar));
    card.setProgress(total > 0 ? done / total : null);
  };
  paint();

  // One line that keeps rolling to the item just handled, like the scan's
  // path line — no growing per-item list.
  const onItem = (item: ExecItem) => {
    done += 1;
    if (item.outcome === "trashed") trashedKb += item.size_kb ?? 0;
    if (item.outcome === "trashed" || item.outcome === "removed") freedSoFar += item.size_kb ?? 0;
    if (item.outcome === "failed" || item.outcome === "skipped") problems.push(item);
    card.setLabel(item.path);
    paint();
  };

  (async () => {
    let freedKb = 0;
    let skipped = 0;
    let failed = 0;
    for (const job of jobs) {
      const report: ExecReport =
        (await opts.execute?.(job, onItem)) ??
        (await executePlan(job.planId, job.selection, false, onItem, opts.trashOverride ?? null));
      freedKb += report.total_freed_kb;
      skipped += report.skipped;
      failed += report.failed;
    }
    return { freedKb, skipped, failed };
  })()
    .then(({ freedKb, skipped, failed }) => {
      card.stop();
      // 卸载面只上报"发生过一次卸载"，清理面额外带删除模式；两者都不带
      // 数量和体积（隐私承诺）。失败数只用来分 ok/partial/failed 三档。
      if (surfaceOf(opts) === "apps") {
        track({ kind: "app_uninstalled" });
      } else {
        track({
          kind: "clean_executed",
          mode: opts.trashOverride === true ? "trash" : "permanent",
          result: failed === 0 ? "ok" : done > failed ? "partial" : "failed",
        });
      }
      // The house ledger only ever records what actually came back from the
      // backend, so the hero statlines quote real history.
      recordReclaim(opts.ledger ?? "clean", freedKb);
      const [freedValue, freedUnit] = splitUnit(humanKb(freedKb));
      body.innerHTML = `
        <div class="page flow-done">
          ${folioHtml(opts.issue, t("flow.done"))}
          <section class="databox">
            <div class="cell">
              <div class="cell-k">${t("flow.freed")}</div>
              <div class="fig">${esc(freedValue)}<small>${esc(freedUnit)}</small></div>
            </div>
            <div class="cell lede done-lede">
              <div class="cell-k done-stats">
                <span>${t("flow.skipped")} · ${skipped}</span>
                <span>${t("flow.failed")} · ${failed}</span>
              </div>
            </div>
            <div class="cell cell-actions">
              <button class="btn lg" id="again">${t("clean.again")} →</button>
            </div>
          </section>
          ${problemsHtml(problems, opts.home)}
        </div>`;
      body.querySelector("#again")!.addEventListener("click", () => {
        window.dispatchEvent(new HashChangeEvent("hashchange"));
      });
      opts.afterDone?.(body, { freedKb, trashedKb });
    })
    .catch((e) => {
      card.stop();
      if (surfaceOf(opts) !== "apps") {
        track({
          kind: "clean_executed",
          mode: opts.trashOverride === true ? "trash" : "permanent",
          result: "failed",
        });
      }
      // 执行失败时替换整个视图，避免停留在无出口的"处理中"状态
      body.innerHTML = `
        <div class="page flow-done">
          ${folioHtml(opts.issue, t("flow.failed"))}
          <section class="mod flow-failed">
            ${kickHtml(1, t("flow.failed"))}
            <p class="prose">${esc(String(e?.message ?? e))}</p>
            <div class="act-row"><button class="btn lg" id="again">${t("clean.again")} →</button></div>
          </section>
        </div>`;
      body.querySelector("#again")!.addEventListener("click", () => {
        window.dispatchEvent(new HashChangeEvent("hashchange"));
      });
    });
}

/** The done page's module listing items that were not removed, each with a
 * plain reason (path · size · reason). Empty string when everything went
 * through. */
function problemsHtml(items: ExecItem[], home?: string): string {
  if (items.length === 0) return "";
  const totalKb = items.reduce((n, item) => n + (item.size_kb ?? 0), 0);
  const rows = items
    .map(
      (item) => `<div class="tr skip">
        <span class="ell mono path-cell" title="${esc(item.path)}">${esc(tildify(item.path, home))}</span>
        <span class="num">${item.size_kb === null ? "" : humanKb(item.size_kb)}</span>
        <span class="reason" title="${esc(item.error ?? "")}">${esc(failureReason(item))}</span>
      </div>`,
    )
    .join("");
  return `
    <div class="grid exec-problems">
      <section class="mod span-all">
        ${kickHtml(2, t("flow.problems", { n: items.length }), totalKb > 0 ? `<span class="aside">${esc(humanKb(totalKb))}</span>` : "")}
        <div class="tbl">
          <div class="tr th skip"><span>${t("flow.col.path")}</span><span class="num">${t("flow.col.size")}</span><span>${t("flow.col.reason")}</span></div>
          ${rows}
        </div>
      </section>
    </div>`;
}

/** Turn the backend's stable error code ("io:…", "trash-failed:…",
 * "rejected:…") into a sentence the user can act on. Unknown codes are shown
 * as-is so nothing is hidden. */
export function failureReason(item: ExecItem): string {
  const code = item.error ?? "";
  const head = code.split(":")[0];
  switch (head) {
    case "whitelisted":
      return t("flow.why.whitelisted");
    case "rejected":
      return t("flow.why.rejected");
    case "requires_admin":
      return t("flow.why.admin");
    case "privacy-denied":
      return t("flow.why.privacy");
    case "mutable-parent":
      return t("flow.why.mutableParent");
    case "identity-changed":
      return t("flow.why.changed");
    case "trash-failed":
      return t("flow.why.trash");
    case "io":
      if (/not permitted|permission denied/i.test(code)) return t("flow.why.denied");
      if (/in use|busy/i.test(code)) return t("flow.why.busy");
      return code.slice(3).trim();
    default:
      return code;
  }
}

/** Re-exported for views composing their own headers. */
export type { PlanSection };
