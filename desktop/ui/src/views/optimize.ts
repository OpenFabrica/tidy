// Optimize view (newspaper skin): a "routine maintenance" column. The idle
// page is a typographic hero; running the check opens the numbered six-item
// grid from the reference — pick the tasks for this issue, run them
// SEQUENTIALLY, and watch the shared progress card. Admin tasks are skipped
// up front when the privileged helper is unavailable. The right rail prints
// the maintenance log, which comes from this machine's own past runs.

import { appMeta, listOptimizeTasks, runOptimize } from "../ipc";
import { track } from "../telemetry";
import { esc } from "../format";
import { renderFrontPage } from "../frontpage";
import { renderScanCard } from "../scancard";
import {
  averageTaskMs,
  daysSince,
  lastMaintenance,
  maintenanceDoneCount,
  maintenanceLog,
  recordMaintenance,
} from "../ledger";
import { relativeDay, t, timestamp } from "../i18n";
import type { View } from "../router";
import type { OptimizeTask, TaskResult } from "../types";

/** Session cache: the task catalog is static and the helper state rarely
 * changes, so re-entering the tab renders instantly instead of re-querying. */
let cached: { tasks: OptimizeTask[]; helper: boolean } | null = null;

/** A task run within this window counts as "already done today" and is left
 * unchecked, matching the reference's "✓ 昨天已运行" state. */
const RECENT_MS = 24 * 3_600_000;

export const optimize: View = {
  async mount(container) {
    if (!cached) {
      const [tasks, meta] = await Promise.all([listOptimizeTasks(), appMeta()]);
      cached = { tasks, helper: meta.helper_available };
    }
    renderIdle(container, cached.tasks, cached.helper);
  },
};

/** Idle front page: the section's standing headline and the check CTA. */
function renderIdle(container: HTMLElement, tasks: OptimizeTask[], helper: boolean): void {
  // Same gating rule the old checklist applied per checkbox: admin tasks are
  // excluded entirely when the privileged helper is unavailable.
  const runnable = tasks.filter((task) => !(task.requires_admin && !helper));
  const last = lastMaintenance();
  const eta = averageTaskMs(runnable.map((task) => task.id));
  const etaLabel = eta === null ? "—" : t("opt.minutes", { m: Math.max(1, Math.round(eta / 60_000)) });

  const start = renderFrontPage(container, {
    kicker: t("opt.kicker"),
    strapline: t("opt.strapline", { n: runnable.length }),
    // The plate numeral is the task count, spelled the way the section
    // headline counts them.
    watermark: countNumeral(runnable.length),
    headlineHtml: t("opt.headline"),
    desk: t("opt.desk"),
    dateline: last ? `${t("opt.stat.last")} · ${timestamp(last.at)}` : t("opt.neverRun"),
    action: t("opt.check"),
    actionNote: eta === null ? t("opt.checkNote") : `${etaLabel}\n${t("opt.checkNote")}`,
    noteBody: t("opt.standfirst"),
    stats: [
      { label: t("opt.stat.last"), value: last ? timestamp(last.at) : "—" },
      { label: t("opt.stat.done"), value: t("opt.times", { n: maintenanceDoneCount() }) },
      { label: t("opt.stat.eta"), value: etaLabel },
    ],
  });
  if (helper === false && runnable.length < tasks.length) {
    const gate = document.createElement("p");
    gate.className = "front-action-note";
    gate.style.maxWidth = "none";
    gate.textContent = t("opt.helper");
    container.querySelector(".front-lead")!.append(gate);
  }

  start.addEventListener("click", () => {
    if (runnable.length === 0) return;
    renderChecklist(container, runnable);
  });
}

/** Spell a small count the way the headline reads it (六 in zh, 6 in en). */
function countNumeral(n: number): string {
  const cjk = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九", "十"];
  return t("opt.numerals") === "cjk" && n <= 10 ? cjk[n] : String(n);
}

/** The six-item grid: numbered tasks in two columns plus the log rail. */
function renderChecklist(container: HTMLElement, tasks: OptimizeTask[]): void {
  // Anything not run in the last day is part of this issue by default; a task
  // that already ran today is offered as an opt-in instead.
  const chosen = new Set(
    tasks.filter((task) => !ranRecently(task.id)).map((task) => task.id),
  );

  const cards = tasks
    .map((task, idx) => {
      const on = chosen.has(task.id);
      const previous = lastMaintenance(task.id);
      const recent = ranRecently(task.id);
      const state = recent && !on
        ? `<span class="t-state">✓ ${t("opt.ranAt", { when: relativeDay(daysSince(previous!.at)) })}</span>`
        : `<button class="link-quiet ${on ? "accent" : ""}" data-task="${esc(task.id)}">${
            on ? `✓ ${t("opt.chosen")}` : t("opt.add")
          }</button>`;
      return `<div class="task ${on ? "" : "off"}" data-card="${esc(task.id)}">
        <span class="no">${String(idx + 1).padStart(2, "0")}</span>
        <div class="body">
          <div class="t-title">${esc(task.title)}</div>
          <div class="t-desc">${esc(task.description)}</div>
          <div class="t-action">${state}</div>
        </div>
      </div>`;
    })
    .join("");

  container.innerHTML = `
    <div class="cols" style="padding-top:32px">
      <div class="col-main">
        <div class="led-head" style="margin-bottom:16px">
          <span class="title" style="font-size:26px">${t("opt.listTitle", { n: tasks.length })}</span>
          <button class="link-cta sm" id="go"></button>
        </div>
        <div class="task-grid">${cards}</div>
      </div>
      <div class="col-side">
        <span class="sec-label">${t("opt.log")}</span>
        <div class="ledger tight" id="log">${logRows()}</div>
        <div class="rule-soft" style="padding-top:14px;font-size:12px;line-height:1.8;color:var(--ink-faint);text-wrap:pretty">
          ${t("opt.note")}
        </div>
        <button class="link-quiet" id="back">${t("flow.cancel")}</button>
      </div>
    </div>`;

  const go = container.querySelector<HTMLButtonElement>("#go")!;
  const syncGo = () => {
    go.textContent = `${t("opt.runSelected", { n: chosen.size })} →`;
    go.disabled = chosen.size === 0;
  };
  syncGo();

  container.querySelectorAll<HTMLButtonElement>("[data-task]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = btn.dataset.task!;
      const on = !chosen.has(id);
      if (on) chosen.add(id);
      else chosen.delete(id);
      btn.classList.toggle("accent", on);
      btn.textContent = on ? `✓ ${t("opt.chosen")}` : t("opt.add");
      container
        .querySelector<HTMLElement>(`[data-card="${CSS.escape(id)}"]`)!
        .classList.toggle("off", !on);
      syncGo();
    });
  });

  container.querySelector("#back")!.addEventListener("click", () => {
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  });
  go.addEventListener("click", () => {
    void renderRunning(container, tasks.filter((task) => chosen.has(task.id)));
  });
}

/** Running page: the shared progress card (same as Clean's scan) driven by
 * the sequential task loop — current task on the path line, a determinate
 * bar, and done/succeeded counts in the tiles. Tasks are not cancellable
 * mid-run, so the card has no cancel button. */
async function renderRunning(container: HTMLElement, tasks: OptimizeTask[]): Promise<void> {
  const card = renderScanCard(container, {
    icon: "gauge",
    stage: t("scan.opt.stage"),
    title: t("scan.opt.title"),
    pathLabel: t("scan.opt.current"),
    hint: t("scan.opt.hint"),
    meter: false,
  });
  const paintCounts = (done: number, ok: number) => {
    card.setTile(0, t("scan.opt.progress"), `${done} / ${tasks.length}`);
    card.setTile(1, t("scan.opt.ok"), String(ok));
    card.setProgress(tasks.length > 0 ? done / tasks.length : null);
  };
  paintCounts(0, 0);

  const results: TaskResult[] = [];
  for (let i = 0; i < tasks.length; i++) {
    const task = tasks[i];
    card.setLabel(`${task.title} · ${i + 1}/${tasks.length}`);
    // One task per IPC call keeps the card honest about what is running.
    const startedAt = Date.now();
    track({ kind: "optimize_run", action: task.id });
    const [result] = await runOptimize([task.id]);
    const settled = result ?? { id: task.id, outcome: "failed", output: "" };
    results.push(settled);
    recordMaintenance(task.id, task.title, settled.outcome, Date.now() - startedAt);
    paintCounts(results.length, results.filter((r) => isOk(r)).length);
  }
  card.stop();

  // Compact summary: real execution errors are "failed"; every refusal or
  // skip (admin gate, apps running, probe unknown, unavailable…) counts as
  // skipped so the closing line stays one glance long.
  const okCount = results.filter((r) => isOk(r)).length;
  const failCount = results.filter((r) => r.outcome === "failed").length;
  const skipCount = results.length - okCount - failCount;
  const parts = [`${okCount} ${t("opt.sum.ok")}`];
  if (skipCount > 0) parts.push(`${skipCount} ${t("opt.sum.skip")}`);
  if (failCount > 0) parts.push(`${failCount} ${t("opt.sum.fail")}`);

  container.innerHTML = `
    <div class="hero">
      <span class="kicker">${t("opt.done")}</span>
      <div class="big">${esc(parts.join(" · "))}</div>
      <p class="sub">${t("opt.doneSub")}</p>
    </div>`;
  const hero = container.querySelector<HTMLElement>(".hero")!;
  const back = document.createElement("button");
  back.className = "frame-cta";
  back.textContent = `${t("clean.again")} →`;
  back.addEventListener("click", () => {
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  });
  hero.append(back);
}

/** Right-rail log rows, newest first (empty ledger prints a dash). */
function logRows(): string {
  const rows = maintenanceLog().slice(-6).reverse();
  if (rows.length === 0) return `<div class="led"><span class="muted">—</span></div>`;
  return rows
    .map((entry) => {
      const d = new Date(entry.at);
      return `<div class="led">
        <span class="muted">${d.getMonth() + 1}/${d.getDate()}</span>
        <span>${esc(entry.title)}</span>
        <span class="dots"></span>
        <span class="amt dim">${entry.outcome === "ok" || entry.outcome === "unchanged" ? "✓" : esc(entry.outcome)}</span>
      </div>`;
    })
    .join("");
}

/** True when the task ran inside the recency window. */
function ranRecently(id: string): boolean {
  const previous = lastMaintenance(id);
  return previous !== null && Date.now() - previous.at < RECENT_MS;
}

/** A run that left the system in the wanted state (changed or already so). */
function isOk(result: TaskResult): boolean {
  return result.outcome === "ok" || result.outcome === "unchanged";
}
