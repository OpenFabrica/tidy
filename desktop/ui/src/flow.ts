// The shared destructive-flow skeleton, set as a newspaper spread:
// scanning → three-column ledger preview (总额 · 账目 · 说明) → confirm sheet
// → live execution feed → reclaimed-space page. One confirmation pattern for
// every destructive surface.
// Performance contract: checkbox toggles patch section rows and the subtotal
// in place — the ledger never re-renders wholesale.

import { cancelTask, executePlan, revealInFinder } from "./ipc";
import { esc, figureClass, humanKb, splitUnit } from "./format";
import { daysSince, lastReclaim, recordReclaim, totalFreedKb } from "./ledger";
import { t } from "./i18n";
import { stopwatch, track } from "./telemetry";
import type { ExecItem, ExecReport, PlanItem, PlanSection, PlanSummary } from "./types";

export interface FlowOptions {
  title: string;
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
    <div class="hero" id="flow-hero">
      <span class="kicker">${t("flow.scanning")}</span>
      <div class="big">${esc(opts.title)}</div>
      <p class="sub mono" id="scan-label"></p>
      <button class="link-quiet" id="cancel-scan">${t("flow.cancel")}</button>
    </div>
    <div class="content-narrow" id="flow-body"></div>`;

  const body = container.querySelector<HTMLElement>("#flow-body")!;
  const hero = container.querySelector<HTMLElement>("#flow-hero")!;
  container.querySelector("#cancel-scan")!.addEventListener("click", () => {
    void cancelTask(taskId);
    opts.onCancel?.();
  });

  // 扫描与执行的埋点都收在这里：clean / apps / analyze 三个界面共用这套骨架，
  // 埋在各自的 view 里迟早会漏掉一个分支，也会各写各的口径。
  const scan = stopwatch();
  opts
    .plan(taskId, (label) => {
      const el = container.querySelector<HTMLElement>("#scan-label");
      if (el) el.textContent = label;
    })
    .then((result) => {
      track({ kind: "scan_completed", scan: surfaceOf(opts), duration_ms: scan() });
      hero.remove();
      renderPreview(body, Array.isArray(result) ? result : [result], opts);
    })
    .catch((e) => {
      hero.innerHTML = `<div class="placeholder">${esc(String(e?.message ?? e))}</div>`;
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

/** Stage 2: the ledger spread — headline total, account rows, house rules. */
function renderPreview(body: HTMLElement, summaries: PlanSummary[], opts: FlowOptions): void {
  if (summaries.reduce((n, s) => n + s.count, 0) === 0) {
    body.innerHTML = `
      <div class="hero">
        <span class="kicker">${esc(opts.title)}</span>
        <div class="big">${t("flow.nothing")}</div>
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
        <div class="card-row">
          <input type="checkbox" class="sec-check" data-seccheck="${idx}" />
          <div class="sec-text">
            <span class="sec-title">${esc(meta.name)}<span class="muted" data-seccount="${idx}"></span></span>
            ${meta.desc ? `<span class="sec-desc">${esc(meta.desc)}</span>` : ""}
          </div>
          <div class="spacer"></div>
          <div class="sec-size" data-secsize="${idx}"></div>
          <button class="chev" data-chev="${idx}" title="${t("prev.expand")}">${CHEVRON_SVG}</button>
        </div>
        <div class="sec-items" data-items="${idx}" hidden></div>
      </div>`;
    })
    .join("");

  const previous = lastReclaim(opts.ledger ?? "clean");
  const lifetime = totalFreedKb();
  const [grandValue, grandUnit] = splitUnit(humanKb(grandKb));

  body.innerHTML = `
    <div class="cols" style="padding-top:36px">
      <div class="col-main" style="flex:0 0 330px">
        <span class="kicker">${t("prev.available")}</span>
        <div class="figure ${figureClass(grandValue)}" id="grand-total">${grandValue}<span class="unit">${grandUnit}</span></div>
        <p class="lede">${t("prev.lede")}</p>
        ${opts.subtitleHtml?.() ?? ""}
        <button class="link-cta" id="go"></button>
        <button class="link-quiet" id="expand-all">${t("prev.expandAll")}</button>
        <button class="link-quiet" id="close-preview">${t("flow.cancel")}</button>
      </div>
      <div class="col-main col-rule">
        <div class="led-head">
          <span class="title">${t("prev.accounts")}</span>
          <span class="aside">
            <a href="#" id="sel-all">${t("prev.selectall")}</a> /
            <a href="#" id="sel-none">${t("prev.clear")}</a>
          </span>
        </div>
        ${rows}
        <div class="led total">
          <span class="label" id="foot-count"></span>
          <span class="dots"></span>
          <span class="amt" id="foot-size"></span>
        </div>
      </div>
      <div class="col-side">
        <div>
          <div class="sec-label">${t("prev.side.last")}</div>
          <div class="figure sm">${previous ? t("clean.stat.daysAgo", { d: daysSince(previous.at) }) : "—"}</div>
        </div>
        <div>
          <div class="sec-label">${t("prev.side.total")}</div>
          <div class="figure sm">${lifetime > 0 ? humanKb(lifetime) : "—"}</div>
        </div>
        <div class="rule-soft" style="padding-top:16px;font-size:12px;line-height:1.8;color:var(--ink-faint);text-wrap:pretty">
          ${opts.trashOverride ? t("prev.side.rulesTrash") : t("prev.side.rules")}
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
    sizeEl.textContent =
      set.size === section.items.length
        ? humanKb(section.total_kb)
        : `${humanKb(selKb)} / ${humanKb(section.total_kb)}`;
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
        return `<div class="item-row">
          <input type="checkbox" data-item="${esc(item.id)}" data-sec="${idx}"
            ${checked[idx].has(item.id) ? "checked" : ""} ${gated ? "disabled" : ""} />
          <div class="item-text">
            <span class="iname">${esc(displayName(item.path))}</span>
            <span class="ipath" title="${esc(item.path)}">${esc(tildify(item.path, opts.home))}</span>
          </div>
          <div class="spacer"></div>
          ${item.item_count > 1 ? `<span class="icount muted">${item.item_count} ${t("prev.items")}</span>` : ""}
          <span class="isize">${item.size_kb === null ? "?" : humanKb(item.size_kb)}</span>
          ${opts.itemBadge?.(item, section.title) ?? ""}
          ${gated ? `<span class="badge warn">${t("flow.admin")}</span>` : ""}
          <button class="chev reveal" data-reveal="${esc(item.path)}" title="Finder">↗</button>
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
    <div class="confirm-sheet">
      <h2>${esc(t("flow.confirm.title", { n: count }))}</h2>
      <p class="lede">${esc(t("flow.confirm.sub", { size: humanKb(totalKb) }))}</p>
      ${note ? `<p class="confirm-note">${esc(note)}</p>` : ""}
      <div class="confirm-actions">
        <button id="confirm-cancel">${t("flow.cancel")}</button>
        <button id="confirm-go" class="danger">${esc(verb)}</button>
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

/** Stage 4: live execution feed, then the reclaimed-space page. Plans execute
 * strictly one after another (each through its own two-phase funnel); the
 * closing page shows the aggregated report. */
export function runExecution(
  body: HTMLElement,
  jobs: ExecJob[],
  opts: Pick<FlowOptions, "trashOverride" | "execute" | "ledger" | "afterDone">,
): void {
  body.innerHTML = `
    <div style="padding-top:30px">
      <div class="led-head">
        <span class="title">${t("flow.working")}</span>
        <span class="aside" id="exec-line"></span>
      </div>
      <table><tbody id="exec-feed"></tbody></table>
    </div>`;
  const feed = body.querySelector<HTMLElement>("#exec-feed")!;
  const line = body.querySelector<HTMLElement>("#exec-line")!;
  const total = jobs.reduce((n, j) => n + j.selection.length, 0);
  let done = 0;
  // Bytes that only moved to the Trash: they count as "freed" in the report
  // but stay on disk until the Trash is emptied, so the done page says so.
  let trashedKb = 0;

  const onItem = (item: ExecItem) => {
    done += 1;
    if (item.outcome === "trashed") trashedKb += item.size_kb ?? 0;
    line.textContent = `${done}/${total}`;
    const row = document.createElement("tr");
    row.innerHTML = `
      <td style="width:96px"><span class="badge ${badgeFor(item.outcome)}">${esc(item.outcome)}</span></td>
      <td title="${esc(item.path)}">${esc(item.path)}</td>
      <td class="num">${item.size_kb === null ? "" : humanKb(item.size_kb)}</td>
      <td class="muted">${esc(item.error ?? "")}</td>`;
    feed.prepend(row);
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
      body.innerHTML = `
        <div class="hero">
          <span class="kicker">${t("flow.done")}</span>
          <div class="figure ${figureClass(splitUnit(humanKb(freedKb))[0])}">${
            splitUnit(humanKb(freedKb))[0]
          }<span class="unit">${splitUnit(humanKb(freedKb))[1]}</span></div>
          <p class="sub">${t("flow.freed")}</p>
          <div class="statline">
            <span>${t("flow.skipped")} · ${skipped}</span>
            <span>${t("flow.failed")} · ${failed}</span>
          </div>
          <button class="frame-cta" id="again">${t("clean.again")} →</button>
        </div>`;
      body.querySelector("#again")!.addEventListener("click", () => {
        window.dispatchEvent(new HashChangeEvent("hashchange"));
      });
      opts.afterDone?.(body, { freedKb, trashedKb });
    })
    .catch((e) => {
      if (surfaceOf(opts) !== "apps") {
        track({
          kind: "clean_executed",
          mode: opts.trashOverride === true ? "trash" : "permanent",
          result: "failed",
        });
      }
      line.textContent = String(e?.message ?? e);
    });
}

/** Badge color for an execution outcome. */
export function badgeFor(outcome: string): string {
  if (outcome === "trashed" || outcome === "removed" || outcome === "dry-run") return "ok";
  if (outcome === "failed") return "danger";
  return "warn";
}

/** Re-exported for views composing their own headers. */
export type { PlanSection };
