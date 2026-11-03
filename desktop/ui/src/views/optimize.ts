// Optimize view (newspaper skin): a "routine maintenance" column. The idle
// page is the shared front page; running the check opens the numbered task
// grid (tabloid modules that reflow by width) — pick the tasks for this issue, run them
// SEQUENTIALLY, and watch the shared progress card. Admin tasks are skipped
// up front when the privileged helper is unavailable. The right rail prints
// the maintenance log, which comes from this machine's own past runs.

import { appMeta, listOptimizeTasks, runOptimize } from "../ipc";
import { track } from "../telemetry";
import { esc, splitUnit } from "../format";
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
    // The front page markup belongs to frontpage.ts; fall back to the action's
    // row if its lead column is ever renamed, rather than throwing.
    (container.querySelector(".front-lead") ?? start.parentElement)?.append(gate);
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

/** Task list spread: ticker band of the run's figures, the numbered task
 * grid (reflows from three columns to one), and the maintenance-log module. */
function renderChecklist(container: HTMLElement, tasks: OptimizeTask[]): void {
  // Anything not run in the last day is part of this issue by default; a task
  // that already ran today is offered as an opt-in instead.
  const chosen = new Set(
    tasks.filter((task) => !ranRecently(task.id)).map((task) => task.id),
  );
  const last = lastMaintenance();
  const eta = averageTaskMs(tasks.map((task) => task.id));
  const [doneNum, doneUnit] = splitUnit(t("opt.times", { n: maintenanceDoneCount() }));

  const cards = tasks
    .map((task, idx) => {
      const on = chosen.has(task.id);
      const previous = lastMaintenance(task.id);
      const recent = ranRecently(task.id);
      const state = recent && !on
        ? `<span class="state ran">✓ ${t("opt.ranAt", { when: relativeDay(daysSince(previous!.at)) })}</span>`
        : `<button class="state link plain ${on ? "accent" : ""}" data-task="${esc(task.id)}">${
            on ? `✓ ${t("opt.chosen")}` : t("opt.add")
          }</button>`;
      return `<div class="task ${on ? "" : "off"}" data-card="${esc(task.id)}">
        <span class="no">${String(idx + 1).padStart(2, "0")}</span>
        <div class="body">
          <h3 class="t-title">${esc(task.title)}</h3>
          <p class="t-desc">${esc(task.description)}</p>
          ${state}
        </div>
      </div>`;
    })
    .join("");

  container.innerHTML = `
    <div class="page opt-page">
      <div class="folio">
        <span class="issue">${esc(t("opt.kicker"))}</span>
        <span>${esc(t("opt.strapline", { n: tasks.length }))}</span>
      </div>
      <section class="band">
        <div class="ticker four">
          <div class="tick">
            <div class="tick-k">${t("opt.stat.last")}</div>
            <div class="tick-v">${last ? esc(timestamp(last.at)) : "—"}</div>
          </div>
          <div class="tick">
            <div class="tick-k">${t("opt.stat.done")}</div>
            <div class="tick-v">${esc(doneNum)}${doneUnit ? `<small>${esc(doneUnit)}</small>` : ""}</div>
          </div>
          <div class="tick">
            <div class="tick-k">${t("prev.selected")}</div>
            <div class="tick-v"><span id="chosen-n"></span><small>/ ${tasks.length}</small></div>
          </div>
          <div class="tick">
            <div class="tick-k">${t("opt.stat.eta")}</div>
            <div class="tick-v">${eta === null ? "—" : esc(t("opt.minutes", { m: Math.max(1, Math.round(eta / 60_000)) }))}</div>
          </div>
        </div>
      </section>
      <div class="grid split">
        <section class="mod">
          <div class="kick"><span class="code">C1</span><span class="t">${t("opt.listKick")}</span></div>
          <div class="headrow">
            <h1 class="title">${t("opt.listTitle", { n: tasks.length })}</h1>
            <button class="btn" id="go"></button>
          </div>
          <div class="tasks">${cards}</div>
        </section>
        <div class="side">
          <aside class="mod">
            <div class="kick"><span class="code">C2</span><span class="t">${t("opt.log")}</span></div>
            <ul class="kv opt-log" id="log">${logRows()}</ul>
            <p class="prose sm opt-note">${t("opt.note")}</p>
            <div class="act-row"><button class="link" id="back">${t("flow.cancel")}</button></div>
          </aside>
        </div>
      </div>
    </div>`;

  const go = container.querySelector<HTMLButtonElement>("#go")!;
  const chosenN = container.querySelector<HTMLElement>("#chosen-n")!;
  /** Keep the run button and the ticker's chosen count in step with the picks. */
  const syncGo = () => {
    go.textContent = `${t("opt.runSelected", { n: chosen.size })} →`;
    go.disabled = chosen.size === 0;
    chosenN.textContent = String(chosen.size);
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
  // Each nonzero count gets its own ticker cell; ok always prints, skip and
  // fail only when they happened, same as the one-line summary before.
  const parts: { n: number; label: string; tone: string }[] = [
    { n: okCount, label: t("opt.sum.ok"), tone: "" },
  ];
  if (skipCount > 0) parts.push({ n: skipCount, label: t("opt.sum.skip"), tone: "warn" });
  if (failCount > 0) parts.push({ n: failCount, label: t("opt.sum.fail"), tone: "danger" });

  container.innerHTML = `
    <div class="page opt-page">
      <div class="folio">
        <span class="issue">${esc(t("opt.kicker"))}</span>
      </div>
      <section class="opt-done">
        <div class="kick"><span class="code">C1</span><span class="t">${t("opt.done")}</span></div>
        <div class="band opt-sum">
          <div class="ticker">
            ${parts
              .map(
                (p) => `<div class="tick ${p.tone}">
                  <div class="tick-k">${esc(p.label)}</div>
                  <div class="tick-v">${p.n}</div>
                </div>`,
              )
              .join("")}
          </div>
        </div>
        <p class="prose opt-done-sub">${t("opt.doneSub")}</p>
        <div class="act-row" id="done-actions"></div>
      </section>
    </div>`;
  const back = document.createElement("button");
  back.className = "btn";
  back.textContent = `${t("clean.again")} →`;
  back.addEventListener("click", () => {
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  });
  container.querySelector<HTMLElement>("#done-actions")!.append(back);
}

/** Log module rows, newest first: date, task title … ✓ or the raw outcome
 * (an empty ledger prints a dash). */
function logRows(): string {
  const rows = maintenanceLog().slice(-6).reverse();
  if (rows.length === 0) return `<li><span class="l muted">—</span></li>`;
  return rows
    .map((entry) => {
      const d = new Date(entry.at);
      const ok = entry.outcome === "ok" || entry.outcome === "unchanged";
      return `<li>
        <span class="l"><span class="mono muted when">${d.getMonth() + 1}/${d.getDate()}</span>${esc(entry.title)}</span>
        <span class="dots"></span>
        <span class="v ${ok ? "" : "bad"}">${ok ? "✓" : esc(entry.outcome)}</span>
      </li>`;
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
