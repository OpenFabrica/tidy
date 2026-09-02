// Clean view (newspaper skin): a front-page hero — issue kicker, serif
// headline, lede, framed CTA and a monospaced statline drawn from the local
// ledger — then one merged scan (system clean + project build artifacts +
// installer leftovers + docker + Trash contents + orphaned leftovers +
// command-backed tools) → ledger preview → confirm → execute →
// reclaimed-space page. Every extra plan keeps its own backend plan/execute
// funnel; this surface only merges their sections into one flow.

import {
  appMeta,
  cancelTask,
  newTaskId,
  planClean,
  planInstaller,
  planPurge,
  planDocker,
  planTrash,
  planOrphans,
  planTools,
  executeDocker,
  executeTools,
} from "../ipc";
import { confirmSheet, renderFlow, runExecution } from "../flow";
import { renderFrontPage } from "../frontpage";
import { cleanTrashMode, dockerIdleMonths, purgeIdleDays } from "../prefs";
import { esc, humanKb } from "../format";
import { daysSince, lastReclaim, reclaimLog, totalFreedKb } from "../ledger";
import { dateline, relativeDay, t } from "../i18n";
import type { View } from "../router";
import type {
  BackupInfo,
  BlockedCaches,
  DockerImage,
  OrphanInfo,
  PlanSummary,
  ProjectReport,
  ToolItem,
} from "../types";

/** Sections that are shown but left unchecked until the user opts in: the
 * evidence is indirect (orphans), the data is irreplaceable (backups,
 * archives), or the removal is permanent by nature (Trash contents). */
const OPT_IN_SECTIONS = new Set(["trash", "orphans", "xcode_archives", "ios_backups"]);

export const clean: View = {
  mount(container) {
    const previous = lastReclaim("clean");
    // The "issue number" is simply how many clean runs this machine has seen,
    // so the masthead conceit stays factual instead of decorative.
    const issue = reclaimLog().filter((r) => r.kind === "clean").length + 1;
    const total = totalFreedKb();

    const scanButton = renderFrontPage(container, {
      kicker: t("clean.issue", { n: issue }),
      strapline: t("clean.strapline"),
      // The plate number is the issue number, so the numeral on the page and
      // the one in the kicker are always the same fact.
      watermark: String(issue),
      headlineHtml: t("clean.headline"),
      desk: t("clean.desk"),
      dateline: dateline(),
      action: t("clean.scan"),
      actionNote: t("clean.scanNote"),
      noteBody: t("clean.standfirst"),
      stats: [
        {
          label: t("clean.stat.last"),
          value: previous ? relativeDay(daysSince(previous.at)) : "—",
        },
        {
          label: t("clean.stat.reclaimed"),
          value: previous ? humanKb(previous.freedKb) : "—",
        },
        { label: t("clean.stat.total"), value: total > 0 ? humanKb(total) : "—" },
      ],
    });


    scanButton.addEventListener("click", async () => {
      const meta = await appMeta();
      let blocked: BlockedCaches | null = null;
      let projects: ProjectReport[] = [];
      let images: DockerImage[] = [];
      let backups: BackupInfo[] = [];
      let orphans: OrphanInfo[] = [];
      let tools: ToolItem[] = [];
      let dockerPlanId = "";
      let toolsPlanId = "";
      const idleDays = purgeIdleDays();
      const idleMonths = dockerIdleMonths();
      // Every secondary scan runs under its own task id so cancel reaches
      // all backend scans, not only the clean one.
      const purgeTask = newTaskId();
      const installerTask = newTaskId();
      const dockerTask = newTaskId();
      const trashTask = newTaskId();
      const orphansTask = newTaskId();
      const toolsTask = newTaskId();
      /** Paths listed by the Trash plan (to tell them apart in the confirm note). */
      const trashPaths = new Set<string>();
      renderFlow(container, newTaskId(), {
        title: t("prev.header"),
        verb: t("clean.go"),
        helperAvailable: meta.helper_available,
        home: meta.home,
        ledger: "clean",
        trashOverride: cleanTrashMode(),
        onCancel: () => {
          for (const id of [purgeTask, installerTask, dockerTask, trashTask, orphansTask, toolsTask])
            void cancelTask(id);
        },
        subtitleHtml: () => {
          const parts: string[] = [];
          if (blocked && blocked.count > 0) {
            const hint = t("prev.blocked", {
              apps: blocked.owners.join("、"),
              size: humanKb(blocked.total_kb),
              n: blocked.count,
            });
            parts.push(`<p class="lede" style="margin-top:6px">${hint}</p>`);
          }
          // Project git blockers stay report-only badges, never a verdict.
          const flagged = projects.filter((p) => p.blockers.length > 0);
          if (flagged.length > 0) {
            parts.push(
              `<div class="note-list">${flagged
                .map(
                  (p) =>
                    `<div class="badge warn" title="${esc(p.root)}">${esc(shorten(p.root))}: ${esc(
                      p.blockers.join(", "),
                    )}</div>`,
                )
                .join(" ")}</div>`,
            );
          }
          return parts.join("");
        },
        // Default checkbox state comes from the user's thresholds (Settings):
        // project deps idle ≥ N days, Docker images unused ≥ N months (no
        // container uses them; docker has no last-used stamp, so age is
        // creation age). Dangling images and unused build cache are always
        // checked; in-use images never are. Opt-in sections and Time Machine
        // snapshots start unchecked regardless of size.
        defaultUnchecked: (item, section) => {
          if (OPT_IN_SECTIONS.has(section)) return true;
          if (section === "projects") {
            const p = projectOf(projects, item.path);
            return p !== undefined && (p.idle_days === null || p.idle_days < idleDays);
          }
          if (section === "docker") {
            const img = imageOf(images, item.id);
            if (!img) return false;
            if (img.containers > 0) return true;
            if (img.dangling) return false;
            return img.age_days === null || img.age_days < idleMonths * 30;
          }
          if (section === "tools") return toolOf(tools, item.id)?.target.kind === "snapshot";
          return false;
        },
        itemBadge: (item, section) => {
          if (section === "projects") {
            const p = projectOf(projects, item.path);
            if (!p || p.idle_days === null) return "";
            return p.idle_days < idleDays
              ? `<span class="badge warn">${esc(t("prev.proj.recent", { d: p.idle_days }))}</span>`
              : `<span class="badge ok">${esc(t("prev.proj.idle", { d: p.idle_days }))}</span>`;
          }
          if (section === "docker") {
            const img = imageOf(images, item.id);
            if (!img) return "";
            if (img.containers > 0)
              return `<span class="badge warn">${esc(t("prev.docker.inuse", { n: img.containers }))}</span>`;
            if (img.dangling) return `<span class="badge ok">${esc(t("prev.docker.dangling"))}</span>`;
            if (img.age_days === null) return "";
            return `<span class="badge ${img.age_days >= idleMonths * 30 ? "ok" : "warn"}">${esc(
              t("prev.docker.age", { d: img.age_days }),
            )}</span>`;
          }
          if (section === "ios_backups") {
            const b = backups.find((x) => x.path === item.path);
            if (!b) return "";
            const who = [b.device, b.product].filter((s) => s.length > 0).join(" · ");
            const when =
              b.last_backup_days === null ? "" : t("prev.backup.age", { d: b.last_backup_days });
            return `<span class="badge warn">${esc([who, when].filter((s) => s.length > 0).join(" · "))}</span>`;
          }
          if (section === "orphans") {
            const o = orphans.find((x) => x.path === item.path);
            if (!o) return "";
            const idle = o.idle_days === null ? "" : ` · ${t("prev.proj.idle", { d: o.idle_days })}`;
            return `<span class="badge ok">${esc(o.bundle_id + idle)}</span>`;
          }
          if (section === "tools") {
            const tool = toolOf(tools, item.id);
            if (!tool) return "";
            const kind = t(`prev.tool.${tool.target.kind}`);
            const text = tool.detail ? `${kind} · ${tool.detail}` : kind;
            return `<span class="badge ${tool.target.kind === "snapshot" ? "warn" : "ok"}">${esc(text)}</span>`;
          }
          return "";
        },
        confirmNote: (selectedItems) => {
          const roots = new Set<string>();
          let kb = 0;
          let recent = 0;
          let dockerCount = 0;
          let trashCount = 0;
          let orphanCount = 0;
          let backupCount = 0;
          let snapshotCount = 0;
          for (const item of selectedItems) {
            if (imageOf(images, item.id) || item.id === "build-cache") {
              dockerCount += 1;
              continue;
            }
            const tool = toolOf(tools, item.id);
            if (tool) {
              if (tool.target.kind === "snapshot") snapshotCount += 1;
              continue;
            }
            if (orphans.some((o) => o.path === item.path)) {
              orphanCount += 1;
              continue;
            }
            if (backups.some((b) => b.path === item.path)) {
              backupCount += 1;
              continue;
            }
            if (trashPaths.has(item.path)) {
              trashCount += 1;
              continue;
            }
            const p = projectOf(projects, item.path);
            if (!p) continue;
            kb += item.size_kb ?? 0;
            if (!roots.has(p.root) && (p.idle_days === null || p.idle_days < idleDays)) recent += 1;
            roots.add(p.root);
          }
          const notes: string[] = [];
          if (roots.size > 0)
            notes.push(
              t("flow.confirm.projects", { n: roots.size, size: humanKb(kb), m: recent, days: idleDays }),
            );
          if (dockerCount > 0) notes.push(t("flow.confirm.docker", { n: dockerCount }));
          if (trashCount > 0) notes.push(t("flow.confirm.trash", { n: trashCount }));
          if (backupCount > 0) notes.push(t("flow.confirm.backups", { n: backupCount }));
          if (snapshotCount > 0) notes.push(t("flow.confirm.snapshots", { n: snapshotCount }));
          if (orphanCount > 0) notes.push(t("flow.confirm.orphans", { n: orphanCount }));
          return notes.join(" ");
        },
        execute: (job, onItem) => {
          if (job.planId === dockerPlanId) return executeDocker(job.planId, job.selection, onItem);
          if (job.planId === toolsPlanId) return executeTools(job.planId, job.selection, onItem);
          return undefined;
        },
        afterDone: (body, result) => renderTrashPending(body, result.trashedKb),
        plan: async (taskId, onLabel) => {
          // All scans run in parallel. The clean scan is the primary: its
          // failure (including cancel) aborts the flow; any secondary failure
          // (docker not running is the common case) degrades to a preview
          // without that section.
          const [c, p, i, d, tr, o, tl] = await Promise.allSettled([
            planClean(taskId, (e) => onLabel(`${e.label} (${e.count})`)),
            planPurge(purgeTask, (e) => onLabel(shorten(e.label))),
            planInstaller(installerTask),
            planDocker(dockerTask),
            planTrash(trashTask),
            planOrphans(orphansTask),
            planTools(toolsTask),
          ]);
          if (c.status === "rejected") throw c.reason;
          blocked = c.value.blocked;
          backups = c.value.backups;
          const summaries: PlanSummary[] = [c.value.summary];
          if (p.status === "fulfilled") {
            projects = p.value.projects;
            summaries.push(p.value.summary);
          }
          if (i.status === "fulfilled") summaries.push(i.value);
          if (d.status === "fulfilled" && d.value.summary.count > 0) {
            images = d.value.images;
            dockerPlanId = d.value.summary.plan_id;
            summaries.push(d.value.summary);
          }
          if (tr.status === "fulfilled" && tr.value.count > 0) {
            for (const s of tr.value.sections) for (const it of s.items) trashPaths.add(it.path);
            summaries.push(tr.value);
          }
          if (o.status === "fulfilled" && o.value.summary.count > 0) {
            orphans = o.value.orphans;
            summaries.push(o.value.summary);
          }
          if (tl.status === "fulfilled" && tl.value.summary.count > 0) {
            tools = tl.value.items;
            toolsPlanId = tl.value.summary.plan_id;
            summaries.push(tl.value.summary);
          }
          return summaries;
        },
      });
    });
  },
};

/**
 * After a Trash-mode run nothing is actually freed until the Trash is emptied.
 * Say so on the done page and offer a one-click empty that goes through the
 * same plan → confirm → execute funnel as everything else (permanent by
 * construction: the backend never Trash-routes the trash plan).
 */
function renderTrashPending(body: HTMLElement, trashedKb: number): void {
  if (trashedKb <= 0) return;
  const hero = body.querySelector<HTMLElement>(".hero");
  if (!hero) return;
  const note = document.createElement("div");
  note.className = "trash-pending";
  note.innerHTML = `
    <p class="sub">${esc(t("flow.trashPending", { size: humanKb(trashedKb) }))}</p>
    <button class="link-cta" id="empty-trash">${esc(t("flow.emptyTrash"))} →</button>`;
  hero.appendChild(note);
  const button = note.querySelector<HTMLButtonElement>("#empty-trash")!;
  button.addEventListener("click", async () => {
    button.disabled = true;
    try {
      const plan = await planTrash(newTaskId());
      if (plan.count === 0) {
        note.innerHTML = `<p class="sub">${esc(t("flow.trashEmpty"))}</p>`;
        return;
      }
      const selection = plan.sections.flatMap((s) => s.items.map((i) => i.id));
      confirmSheet(
        plan.count,
        plan.total_kb,
        t("flow.emptyTrash"),
        () => runExecution(body, [{ planId: plan.plan_id, selection }], { ledger: "clean" }),
        t("flow.confirm.trash", { n: plan.count }),
      );
    } finally {
      button.disabled = false;
    }
  });
}

/** Find the reported project that owns an artifact path (longest root wins
 * so nested packages resolve to the inner project). */
function projectOf(projects: ProjectReport[], path: string): ProjectReport | undefined {
  let best: ProjectReport | undefined;
  for (const p of projects) {
    if (path.startsWith(p.root + "/") && (!best || p.root.length > best.root.length)) best = p;
  }
  return best;
}

/** Docker image row for a plan item id (ids are the full sha256 ids). */
function imageOf(images: DockerImage[], id: string): DockerImage | undefined {
  return images.find((img) => img.id === id);
}

/** Tool row for a plan item id ("snap:…", "sim:…", "brew:cleanup"). */
function toolOf(tools: ToolItem[], id: string): ToolItem | undefined {
  return tools.find((tool) => tool.id === id);
}

/** Shorten a project path for progress/badge display. */
function shorten(path: string): string {
  return path.split("/").slice(-2).join("/");
}
