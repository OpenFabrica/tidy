// Status view (newspaper skin): a six-column metric strip set in giant serif
// numerals — CPU with per-core bars, memory with pressure, GPU, disk, network,
// and battery/fan — then the fan-control panel (manual RPM through the root
// fan daemon, see mole-helper), a double rule, the two top-process ledgers
// and the health rail, and finally the full sortable process table as the
// continuation below the fold. The hardware line is printed in the masthead marginalia.
// Refreshes every 2s while mounted only.

import {
  appIcon,
  fanControlAuto,
  fanControlSet,
  fanControlStart,
  fanControlStatus,
  fanControlStop,
  processDetail,
  revealInFinder,
  signalProcess,
  statusSnapshot,
} from "../ipc";
import { esc, humanBytes, humanUptime, splitUnit } from "../format";
import { t } from "../i18n";
import { formatTemp } from "../prefs";
import { setNavMeta } from "../router";
import type { View } from "../router";
import type {
  FanControlState,
  FanStatus,
  ProcessDetail,
  ProcessInfo,
  StatusSnapshot,
} from "../types";

let timer: number | null = null;

/** Rolling histories for sparklines (persist across renders while mounted). */
const gpuHistory: number[] = [];
const rxHistory: number[] = [];
const txHistory: number[] = [];
const HISTORY = 30;

/** Process table sort state. */
let sortCol: "name" | "pid" | "cpu" | "energy" | "mem" = "cpu";
let sortDesc = true;

/** Last snapshot, kept for instant client-side re-sorts on header click. */
let lastSnap: StatusSnapshot | null = null;

/** Process icon cache: app path → data URL (null = known miss). */
const procIcons = new Map<string, string | null>();

/** Single-flight guard for the icon fetch loop: each 2s render used to start
 * its own loop over the still-uncached paths, so slow fetches stacked into
 * unbounded concurrent backend calls. Only one loop runs; later renders pick
 * up whatever is still missing after it finishes. */
let iconLoopRunning = false;

/** Accumulated CPU time as the energy-impact proxy ("2.4h" / "35m" / "12s"). */
function humanCpuTime(ms: number): string {
  const sec = ms / 1000;
  if (sec >= 3600) return `${(sec / 3600).toFixed(1)}h`;
  if (sec >= 60) return `${Math.round(sec / 60)}m`;
  return `${Math.round(sec)}s`;
}

/** Human run time like "4d 12h". */
function humanRuntime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

/** Headline byte figure: at most one decimal, none from three digits up
 * ("59.3", "120"). humanBytes' two decimals ("59.33") run past the narrow
 * strip columns at headline size. */
function headlineBytes(bytes: number): [string, string] {
  const [value, unit] = splitUnit(humanBytes(bytes));
  const n = Number(value);
  if (!Number.isFinite(n)) return [value, unit];
  return [n >= 100 ? n.toFixed(0) : n.toFixed(1).replace(/\.0$/, ""), unit];
}

/** Push into a rolling buffer. */
function push(buf: number[], v: number): void {
  buf.push(v);
  if (buf.length > HISTORY) buf.shift();
}

/** Health score + the top issue message. */
function health(s: StatusSnapshot): { score: number; message: string } {
  let score = 100;
  const issues: string[] = [];
  const memPct = (s.memory_used_bytes / s.memory_total_bytes) * 100;
  const pressure = s.memory_pressure_percent ?? 0;
  if (s.cpu_usage_percent > 80) {
    score -= 15;
    issues.push("CPU");
  } else if (s.cpu_usage_percent > 60) score -= 5;
  if (pressure > 50 || memPct > 90) {
    score -= 15;
    issues.push(t("st.mem"));
  } else if (memPct > 75) score -= 5;
  const root = s.disks.find((d) => d.mount_point === "/");
  if (root) {
    const freePct = (root.available_bytes / root.total_bytes) * 100;
    if (freePct < 5) {
      score -= 20;
      issues.push(t("st.disk"));
    } else if (freePct < 15) score -= 8;
  }
  const message =
    issues.length === 0 ? t("st.health.good") : `${issues.join(" · ")} ${t("st.health.watch")}`;
  return { score: Math.max(0, score), message };
}

/** Load label from load average vs core count. */
function loadLabel(s: StatusSnapshot): string {
  const ratio = s.load_avg_1m / Math.max(1, s.cpu_count);
  if (ratio < 0.5) return t("st.lowload");
  if (ratio < 0.9) return t("st.midload");
  return t("st.highload");
}

/** Per-core bar chart (inline flex divs). */
function coreBars(cores: number[]): string {
  const bars = cores
    .map((c) => {
      const h = Math.max(3, Math.round((c / 100) * 44));
      return `<div class="core-bar" style="height:${h}px"></div>`;
    })
    .join("");
  return `<div class="core-bars">${bars}</div>`;
}

/** SVG polyline sparkline for a rolling buffer (0-100 scale or auto max). */
function sparkline(buf: number[], color: string, autoScale = false): string {
  if (buf.length < 2) return `<svg class="spark" viewBox="0 0 100 30"></svg>`;
  const max = autoScale ? Math.max(1, ...buf) : 100;
  const points = buf
    .map((v, i) => {
      const x = (i / (HISTORY - 1)) * 100;
      const y = 28 - (Math.min(v, max) / max) * 26;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return `<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none">
    <polyline points="${points}" fill="none" stroke="${color}" stroke-width="1.5" /></svg>`;
}

/** Dual-line network chart (rx inked area, tx rust line), auto-scaled together. */
function netChart(): string {
  if (rxHistory.length < 2) return `<svg class="spark" viewBox="0 0 100 30"></svg>`;
  const max = Math.max(1, ...rxHistory, ...txHistory);
  const line = (buf: number[], color: string, fill: boolean) => {
    const pts = buf
      .map((v, i) => `${((i / (HISTORY - 1)) * 100).toFixed(1)},${(28 - (v / max) * 26).toFixed(1)}`)
      .join(" ");
    return fill
      ? `<polygon points="0,28 ${pts} 100,28" fill="${color}22" stroke="none" />
         <polyline points="${pts}" fill="none" stroke="${color}" stroke-width="1.5" />`
      : `<polyline points="${pts}" fill="none" stroke="${color}" stroke-width="1.5" />`;
  };
  return `<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none">
    ${line(rxHistory, "#26241f", true)}${line(txHistory, "#a4432c", false)}</svg>`;
}

/** One column of the metric strip: a giant serif reading, a monospaced
 * caption, and an optional chart. `hot` turns the whole column rust. */
function metric(value: string, unit: string, label: string, chart: string, hot = false): string {
  return `<div class="metric ${hot ? "hot" : ""}">
    <div class="figure md">${value}<span class="unit">${unit}</span></div>
    <span class="metric-label">${label}</span>
    ${chart}
  </div>`;
}

/** Skeleton strip: the instant first paint before any snapshot exists, so
 * opening the tab never shows a blank screen while sampling runs. */
function renderSkeleton(container: HTMLElement): void {
  const col = `<div class="metric">
    <div class="skel-line" style="width:60%;height:40px"></div>
    <div class="skel-line" style="width:80%;margin-top:10px"></div>
  </div>`;
  container.innerHTML = `
    <div class="content-narrow">
      <div class="metric-strip">${col.repeat(6)}</div>
      <div class="rule-double"></div>
      <div style="padding-top:20px">
        <div class="skel-line" style="width:30%"></div>
        <div class="skel-line" style="width:90%;margin-top:12px"></div>
        <div class="skel-line" style="width:85%;margin-top:8px"></div>
      </div>
    </div>`;
}

/** Fetch one snapshot and re-render. */
async function refresh(container: HTMLElement): Promise<void> {
  let s: StatusSnapshot;
  try {
    s = await statusSnapshot();
  } catch (e) {
    // Only a first load with nothing on screen shows the error; a transient
    // IPC failure must not blank an already-rendered dashboard.
    if (!lastSnap) container.innerHTML = `<div class="placeholder">${esc(String(e))}</div>`;
    return;
  }
  lastSnap = s;
  push(gpuHistory, s.gpu.utilization_percent ?? 0);
  push(rxHistory, s.network.rx_rate_bps);
  push(txHistory, s.network.tx_rate_bps);
  renderSnapshot(container, s);
}

/** Render one snapshot (pure DOM, no fetching — also used for the instant
 * repaint from `lastSnap` when the tab is re-entered). */
function renderSnapshot(container: HTMLElement, s: StatusSnapshot): void {
  const fans = s.fans ?? [];
  const { score, message } = health(s);
  const memPct = (s.memory_used_bytes / s.memory_total_bytes) * 100;
  const pressure = s.memory_pressure_percent ?? 0;
  const root = s.disks.find((d) => d.mount_point === "/") ?? s.disks[0];
  const bootDate = new Date(Date.now() - s.uptime_seconds * 1000);
  const usedPct = root ? ((root.total_bytes - root.available_bytes) / root.total_bytes) * 100 : 0;

  // The hardware line belongs in the masthead marginalia, exactly where the
  // reference prints it.
  setNavMeta(
    `${esc(s.hardware.chip.replace("Apple ", ""))} · ${s.hardware.memory_gb} GB · macOS ${esc(
      s.hardware.os_version,
    )}`,
  );

  const memHot = pressure > 50 || memPct > 90;
  const cpuHot = s.cpu_usage_percent > 80;
  // Die temperatures are the hottest SMC core sensor; null inside VMs or on
  // Intel Macs, in which case the label simply omits it rather than guess.
  // The temperature goes right after the name: the label is a single
  // ellipsized line, so anything at the tail is what gets cut in narrow
  // columns.
  const cpuTemp = s.temperatures?.cpu_c ?? null;
  const gpuTemp = s.temperatures?.gpu_c ?? null;
  const tempTag = (c: number | null) => (c !== null ? ` ${formatTemp(c)}` : "");

  const strip = [
    metric(
      s.cpu_usage_percent.toFixed(0),
      "%",
      `${t("st.cpu")}${tempTag(cpuTemp)} · ${s.cpu_count} ${t("st.cores")}`,
      coreBars(s.per_core_percent),
      cpuHot,
    ),
    metric(
      memPct.toFixed(0),
      "%",
      s.memory_pressure_percent !== null
        ? `${t("st.mem")} · ${t("st.pressure")} ${pressure}%`
        : `${t("st.mem")} · ${humanBytes(s.memory_total_bytes)}`,
      `<div class="meter ${memHot ? "accent" : ""}"><div style="width:${Math.min(100, memPct)}%"></div></div>`,
      memHot,
    ),
    metric(
      String(s.gpu.utilization_percent ?? 0),
      "%",
      `${t("st.gpu")}${tempTag(gpuTemp)} · ${s.gpu.core_count ?? "—"} ${t("st.gpucores")} · ${loadLabel(s)}`,
      sparkline(gpuHistory, "var(--ink)"),
    ),
    root
      ? metric(
          ...headlineBytes(root.available_bytes),
          `${t("st.disk")} · ${t("st.used")} ${usedPct.toFixed(0)}%`,
          `<div class="meter"><div style="width:${usedPct.toFixed(0)}%"></div></div>`,
        )
      : "",
    metric(
      headlineBytes(s.network.rx_rate_bps)[0],
      `${headlineBytes(s.network.rx_rate_bps)[1]}/s`,
      `${t("st.net")}${s.network.interface ? ` · ${esc(s.network.interface)}` : ""} · ↑${humanBytes(s.network.tx_rate_bps)}`,
      netChart(),
    ),
    batteryMetric(s, fans),
  ].join("");

  const procColumn = (label: string, rows: string) =>
    `<div class="col-main">
      <span class="sec-label">${label}</span>
      <div class="ledger tight">${rows}</div>
    </div>`;

  const byCpu = [...s.top_processes].sort((a, b) => b.cpu_percent - a.cpu_percent).slice(0, 5);
  const byMem = [...s.top_processes].sort((a, b) => b.memory_bytes - a.memory_bytes).slice(0, 5);
  const ledRow = (name: string, value: string, hot: boolean) =>
    `<div class="led ${hot ? "flag" : ""}">
      <span class="name">${esc(name)}</span>
      <span class="dots"></span>
      <span class="amt">${esc(value)}</span>
    </div>`;

  // The strip and everything below the fan panel are re-painted as two
  // separate regions; the #fan-panel node in between is never touched, so a
  // slider mid-drag keeps its pointer capture and its event listeners.
  const topHtml = `<div class="metric-strip">${strip}</div>`;
  const restHtml = `
      <div class="rule-double"></div>
      <div class="cols" style="padding-top:20px">
        ${procColumn(
          t("st.proc.byCpu"),
          byCpu
            .map((p) => ledRow(p.name, p.cpu_percent.toFixed(1), p.cpu_percent > 100))
            .join(""),
        )}
        <div class="col-main col-rule">
          <span class="sec-label">${t("st.proc.byMem")}</span>
          <div class="ledger tight">
            ${byMem.map((p) => ledRow(p.name, humanBytes(p.memory_bytes), false)).join("")}
          </div>
        </div>
        <div class="col-side">
          <span class="sec-label">${t("st.health")}</span>
          <div class="figure" style="font-size:44px">${score}<small style="font-size:16px;color:var(--ink-faint)"> / ${
            score >= 85 ? t("st.good") : t("st.fair")
          }</small></div>
          <span class="health-note">${esc(message)}</span>
          <span class="mono muted" style="font-size:11.5px">
            ${t("st.uptime")} ${humanUptime(s.uptime_seconds)} · ${t("st.since")} ${bootDate.getMonth() + 1}/${bootDate.getDate()}
          </span>
        </div>
      </div>
      <div class="rule" style="margin-top:26px;padding-top:16px">
        <span class="sec-label">${t("st.proc.all", { n: s.top_processes.length })}</span>
        <div id="proc-panel" style="margin-top:10px">${procTable(s)}</div>
      </div>
      <div class="muted mono" style="text-align:center;font-size:11px;padding:14px 0 4px">
        ${esc(s.platform)} · ${esc(s.host)}
      </div>`;

  const top = container.querySelector<HTMLElement>("#snap-top");
  const rest = container.querySelector<HTMLElement>("#snap-rest");
  if (top && rest) {
    top.innerHTML = topHtml;
    rest.innerHTML = restHtml;
  } else {
    container.innerHTML = `
    <div class="content-narrow">
      <div id="snap-top">${topHtml}</div>
      <div id="fan-panel" class="fan-panel hidden"></div>
      <div id="snap-rest">${restHtml}</div>
    </div>`;
    renderFanPanel();
  }

  wireProcTable(container);
}

/** Sixth strip column: battery when present, otherwise fan RPM — whichever
 * the hardware actually reports, never an empty slot. */
function batteryMetric(s: StatusSnapshot, fans: FanStatus[]): string {
  const fanLine =
    fans.length > 0
      ? `${t("st.fan")} ${Math.round(Math.max(...fans.map((f) => f.actual_rpm)))} RPM ×${fans.length}`
      : t("st.fan.none");
  if (s.battery) {
    // Charge percent leads; battery temperature is deliberately not shown
    // (it barely moves and reads as if it were the chip temperature).
    return `<div class="metric" style="flex:0.8">
      <div class="figure md">${s.battery.percent}<span class="unit">%</span></div>
      <span class="metric-label">${t("st.battery")}${
        s.battery.charging ? " ⚡" : ""
      } · ${s.battery.cycle_count ?? "—"} ${t("st.cycles")}</span>
      <span class="metric-label">${fanLine}</span>
    </div>`;
  }
  return `<div class="metric" style="flex:0.8">
    <div class="figure md">${
      fans.length > 0 ? Math.round(Math.max(...fans.map((f) => f.actual_rpm))) : "—"
    }<span class="unit">RPM</span></div>
    <span class="metric-label">${t("st.fan")} · ${t("st.fan.macos")}</span>
  </div>`;
}


/** Full process table markup (thead + tbody) from a snapshot. */
function procTable(s: StatusSnapshot): string {
  return `<table class="proc-table">
    <thead><tr>
      <th data-col="name" class="sortable">${t("st.name")} (${s.top_processes.length}) ${arrow("name")}</th>
      <th data-col="pid" class="num sortable">PID ${arrow("pid")}</th>
      <th data-col="cpu" class="num sortable">CPU ${arrow("cpu")}</th>
      <th data-col="energy" class="num sortable">${t("st.energy")} ${arrow("energy")}</th>
      <th data-col="mem" class="num sortable">${t("st.mem")} ${arrow("mem")}</th>
    </tr></thead>
    <tbody>${procRows(s.top_processes)}</tbody>
  </table>`;
}

/** Wire header sorting (instant, from the cached snapshot), row clicks, and
 * lazy icon fill. Called after every table (re)render. */
function wireProcTable(container: HTMLElement): void {
  const panel = container.querySelector<HTMLElement>("#proc-panel");
  if (!panel) return;

  panel.querySelectorAll<HTMLElement>("th.sortable").forEach((th) => {
    th.addEventListener("click", () => {
      const col = th.dataset.col as typeof sortCol;
      if (sortCol === col) sortDesc = !sortDesc;
      else {
        sortCol = col;
        sortDesc = col !== "name";
      }
      // Instant client-side resort — no snapshot refetch, no latency.
      if (lastSnap) {
        panel.innerHTML = procTable(lastSnap);
        wireProcTable(container);
      }
    });
  });

  panel.querySelectorAll<HTMLElement>("tr.proc-row").forEach((row) => {
    row.addEventListener("click", () => void openProcessModal(Number(row.dataset.pid)));
  });

  // Lazy icon fill for uncached app processes — sequential and single-flight
  // so overlapping renders never multiply concurrent backend fetches.
  const pending = Array.from(
    panel.querySelectorAll<HTMLElement>("[data-prociconpath]"),
  );
  const uniquePaths = [...new Set(pending.map((el) => el.dataset.prociconpath!))].filter(
    (path) => !procIcons.has(path),
  );
  if (iconLoopRunning || uniquePaths.length === 0) return;
  iconLoopRunning = true;
  void (async () => {
    try {
      for (const path of uniquePaths) {
        try {
          procIcons.set(path, await appIcon(path));
        } catch {
          procIcons.set(path, null);
        }
      }
    } finally {
      iconLoopRunning = false;
    }
    // Applied on the next 2s refresh; also patch in place for snappiness.
    pending.forEach((el) => {
      const icon = procIcons.get(el.dataset.prociconpath!);
      if (icon && el.isConnected) {
        const img = document.createElement("img");
        img.className = "proc-icon";
        img.src = icon;
        el.replaceWith(img);
      }
    });
  })();
}


/** The process detail modal (reference design image: OrbStack Helper). */
async function openProcessModal(pid: number): Promise<void> {
  const backdrop = document.createElement("div");
  backdrop.className = "confirm-backdrop";
  backdrop.innerHTML = `<div class="proc-sheet"><div class="placeholder">…</div></div>`;
  document.body.appendChild(backdrop);
  backdrop.addEventListener("click", (ev) => {
    if (ev.target === backdrop) backdrop.remove();
  });

  const d: ProcessDetail | null = await processDetail(pid).catch(() => null);
  const sheet = backdrop.querySelector<HTMLElement>(".proc-sheet")!;
  if (!d) {
    sheet.innerHTML = `<div class="placeholder">${t("proc.killfail")}</div>`;
    return;
  }

  const icon = d.app_path ? procIcons.get(d.app_path) : null;
  const chain = [...d.parent_chain, [d.pid, d.name] as [number, string]]
    .map(
      ([cpid, cname], i, arr) =>
        `<span class="${i === arr.length - 1 ? "" : "muted"}">${esc(cname)} <span class="muted">${cpid}</span></span>`,
    )
    .join(" › ");
  const row = (label: string, value: string) =>
    value
      ? `<div class="proc-kv"><span class="k muted">${label}</span><span class="v">${value}</span></div>`
      : "";

  sheet.innerHTML = `
    <div class="proc-head">
      ${icon ? `<img class="proc-icon big" src="${icon}" alt="" />` : `<span class="proc-icon big fallback">⚙︎</span>`}
      <h2>${esc(d.name)}</h2>
      <button class="chev" id="pm-close">✕</button>
    </div>
    <div class="muted" style="font-family:var(--font-mono);font-size:12px;margin-bottom:12px">
      PID ${d.pid} · CPU ${d.cpu_percent.toFixed(1)}% · MEM ${humanBytes(d.memory_bytes)}
      ${d.user ? ` · ${esc(d.user)}` : ""}${d.app_path ? ` · ${t("proc.from")} ${esc(d.app_path.split("/").pop()?.replace(".app", "") ?? "")}` : ""}
    </div>
    <div class="muted" style="font-family:var(--font-mono);font-size:12px;margin-bottom:14px">${chain}</div>
    ${row(t("proc.threads"), d.threads !== null ? String(d.threads) : "")}
    ${row(t("proc.openfiles"), d.open_files !== null ? String(d.open_files) : "")}
    ${row(t("proc.diskio"), `${humanBytes(d.disk_read_bytes)} R · ${humanBytes(d.disk_written_bytes)} W`)}
    ${row(
      t("proc.ports"),
      d.listen_ports.length
        ? `${d.listen_ports.map(esc).join(", ")} <span class="badge warn">${t("proc.ports.warn")}</span>`
        : "",
    )}
    ${row(t("proc.children"), String(d.children))}
    ${row(t("proc.user"), d.user ? esc(d.user) : "")}
    ${row(t("proc.runtime"), humanRuntime(d.run_time_seconds))}
    ${row(t("proc.cwd"), d.cwd ? esc(d.cwd) : "")}
    ${row(t("proc.exe"), d.exe ? esc(d.exe) : "")}
    ${d.cmd.length ? `<div class="proc-kv"><span class="k muted">${t("proc.cmd")}</span></div><div class="proc-cmd">${esc(d.cmd.join(" "))}</div>` : ""}
    <div class="confirm-actions">
      <button id="pm-copy">${t("proc.copy")}</button>
      ${d.exe ? `<button id="pm-reveal">${t("proc.reveal")}</button>` : ""}
      <button id="pm-term">${t("proc.term")}</button>
      <button id="pm-kill" class="danger">${t("proc.kill")}</button>
    </div>`;

  sheet.querySelector("#pm-close")!.addEventListener("click", () => backdrop.remove());
  sheet.querySelector("#pm-copy")!.addEventListener("click", async (ev) => {
    const summary = `${d.name} (pid ${d.pid}) cpu ${d.cpu_percent.toFixed(1)}% mem ${humanBytes(d.memory_bytes)}\n${d.exe ?? ""}\n${d.cmd.join(" ")}`;
    await navigator.clipboard.writeText(summary);
    (ev.target as HTMLElement).textContent = t("proc.copied");
  });
  sheet.querySelector("#pm-reveal")?.addEventListener("click", () => {
    if (d.exe) void revealInFinder(d.exe);
  });
  const kill = (force: boolean) => async (ev: Event) => {
    try {
      await signalProcess(d.pid, force);
      backdrop.remove();
    } catch {
      (ev.target as HTMLElement).textContent = t("proc.killfail");
    }
  };
  sheet.querySelector("#pm-term")!.addEventListener("click", kill(false));
  sheet.querySelector("#pm-kill")!.addEventListener("click", kill(true));
}

/** Sort indicator for a column header. */
function arrow(col: string): string {
  if (col !== sortCol) return "⇅";
  return `<svg class="sort-chev ${sortDesc ? "" : "ascending"}" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4"/></svg>`;
}

/** Sorted process rows with CPU bars and hot flames. */
function procRows(procs: ProcessInfo[]): string {
  const sorted = [...procs].sort((a, b) => {
    const cmp =
      sortCol === "name"
        ? a.name.localeCompare(b.name)
        : sortCol === "pid"
          ? a.pid - b.pid
          : sortCol === "cpu"
            ? a.cpu_percent - b.cpu_percent
            : sortCol === "energy"
              ? a.cpu_time_ms - b.cpu_time_ms
              : a.memory_bytes - b.memory_bytes;
    return sortDesc ? -cmp : cmp;
  });
  const maxCpu = Math.max(1, ...sorted.map((p) => p.cpu_percent));
  return sorted
    .map((p) => {
      const hot = p.cpu_percent > 100;
      const width = Math.max(2, Math.round((p.cpu_percent / maxCpu) * 60));
      const cachedIcon = p.app_path ? procIcons.get(p.app_path) : null;
      const iconHtml = cachedIcon
        ? `<img class="proc-icon" src="${cachedIcon}" alt="" />`
        : `<span class="proc-icon fallback" ${p.app_path ? `data-prociconpath="${esc(p.app_path)}"` : ""}>⚙︎</span>`;
      return `<tr class="proc-row" data-pid="${p.pid}">
        <td><span class="proc-dot ${hot ? "hot" : ""}"></span>${iconHtml} ${esc(p.name)} ${hot ? "🔥" : ""}</td>
        <td class="muted num">${p.pid}</td>
        <td class="num"><span class="cpu-bar ${hot ? "hot" : ""}" style="width:${width}px"></span>
          <span style="color:${hot ? "var(--danger)" : "inherit"}">${p.cpu_percent.toFixed(1)}</span></td>
        <td class="muted num">${humanCpuTime(p.cpu_time_ms)}</td>
        <td class="num">${humanBytes(p.memory_bytes)}</td>
      </tr>`;
    })
    .join("");
}

// ---- fan control panel ----
// The panel lives outside the 2s snapshot re-render: `renderSnapshot` keeps
// the existing #fan-panel node, so a slider mid-drag is never yanked away.
// State is refreshed on its own slower cadence and right after each action.

let fanState: FanControlState | null = null;
let fanError: string | null = null;
let fanBusy = false;
let fanDragging = false;
/** A render requested mid-drag; replayed on pointer release. */
let fanRenderPending = false;
let fanTimer: number | null = null;

/** Panel markup for the current fan state; empty on fanless Macs. */
function fanPanelMarkup(): string {
  const f = fanState;
  if (!f || f.fan_count === 0) return "";
  const min = Math.round(f.min_rpm ?? 0);
  const max = Math.round(f.max_rpm ?? 0);
  const target = Math.round(f.target_rpm ?? f.min_rpm ?? 0);
  const err = fanError ? `<span class="fan-err">${esc(fanError)}</span>` : "";
  if (!f.running) {
    return `<span class="sec-label">${t("st.fan.control")}</span>
      <div class="fan-row">
        <span class="mono muted">${t("st.fan.auto")}</span>
        <button id="fan-start" ${fanBusy ? "disabled" : ""}>${t("st.fan.enable")}</button>
        <span class="muted fan-desc">${t("st.fan.enable.desc")}</span>${err}
      </div>`;
  }
  // Running: mode toggle + slider (slider only active in manual mode).
  return `<span class="sec-label">${t("st.fan.control")}</span>
    <div class="fan-row">
      <button class="toggle ${f.forced ? "on" : ""}" id="fan-mode" ${fanBusy ? "disabled" : ""}></button>
      <span class="mono">${f.forced ? t("st.fan.manual") : t("st.fan.auto")}</span>
      <input type="range" id="fan-rpm" min="${min}" max="${max}" step="50" value="${target}"
        ${f.forced && !fanBusy ? "" : "disabled"} />
      <span class="mono fan-rpm-label" id="fan-rpm-label">${target} RPM</span>
      <span class="muted mono fan-desc">${t("st.fan.range")} ${min}–${max}</span>
      <button id="fan-stop" ${fanBusy ? "disabled" : ""}>${t("st.fan.back")}</button>${err}
    </div>`;
}

/** Run one fan action, then re-render from the returned state. */
async function fanAction(action: () => Promise<FanControlState>): Promise<void> {
  if (fanBusy) return;
  fanBusy = true;
  fanError = null;
  renderFanPanel();
  try {
    fanState = await action();
  } catch (e) {
    const code = (e as { code?: string } | null)?.code;
    // The user closing the password dialog is not an error worth showing.
    if (code !== "cancelled") fanError = `${t("st.fan.err")}: ${String((e as { message?: string })?.message ?? e)}`;
  } finally {
    fanBusy = false;
    renderFanPanel();
  }
}

/** Paint the panel into its node and wire its controls. */
function renderFanPanel(): void {
  const panel = document.getElementById("fan-panel");
  if (!panel) return;
  if (fanDragging) {
    fanRenderPending = true;
    return;
  }
  fanRenderPending = false;
  panel.innerHTML = fanPanelMarkup();
  panel.classList.toggle("hidden", panel.innerHTML === "");
  panel.querySelector<HTMLButtonElement>("#fan-start")?.addEventListener("click", () => {
    void fanAction(fanControlStart);
  });
  panel.querySelector<HTMLButtonElement>("#fan-stop")?.addEventListener("click", () => {
    void fanAction(fanControlStop);
  });
  panel.querySelector<HTMLButtonElement>("#fan-mode")?.addEventListener("click", () => {
    const slider = panel.querySelector<HTMLInputElement>("#fan-rpm");
    const rpm = Number(slider?.value ?? fanState?.min_rpm ?? 0);
    void fanAction(fanState?.forced ? fanControlAuto : () => fanControlSet(rpm));
  });
  const slider = panel.querySelector<HTMLInputElement>("#fan-rpm");
  const label = panel.querySelector<HTMLElement>("#fan-rpm-label");
  if (!slider) return;
  // The drag flag follows the pointer, not `input`/`change`: a `change` that
  // never fires (drag interrupted) would otherwise freeze the panel forever.
  const release = () => {
    if (!fanDragging) return;
    fanDragging = false;
    if (fanRenderPending) renderFanPanel();
  };
  slider.addEventListener("pointerdown", () => {
    fanDragging = true;
  });
  slider.addEventListener("pointerup", release);
  slider.addEventListener("pointercancel", release);
  slider.addEventListener("lostpointercapture", release);
  slider.addEventListener("input", () => {
    if (label) label.textContent = `${slider.value} RPM`;
  });
  // Apply on release only: one SMC write per gesture, not per pixel.
  slider.addEventListener("change", () => {
    release();
    void fanAction(() => fanControlSet(Number(slider.value)));
  });
}

/** Poll the daemon/SMC state (slower than the snapshot; it rarely changes). */
async function refreshFan(): Promise<void> {
  try {
    fanState = await fanControlStatus();
  } catch {
    fanState = null;
  }
  renderFanPanel();
}

/** Re-render immediately when the window becomes visible again. */
let onVisible: (() => void) | null = null;

export const dashboard: View = {
  mount(container) {
    // Instant first paint: the last snapshot when returning to the tab, a
    // skeleton grid on the very first open — never a blank screen while the
    // snapshot samples.
    if (lastSnap) renderSnapshot(container, lastSnap);
    else renderSkeleton(container);
    void refresh(container);
    // Skip refreshes while the window is hidden (close-to-tray keeps the view
    // mounted): polling an invisible dashboard is pure CPU waste.
    timer = window.setInterval(() => {
      if (!document.hidden) void refresh(container);
    }, 2000);
    void refreshFan();
    fanTimer = window.setInterval(() => {
      if (!document.hidden && !fanBusy) void refreshFan();
    }, 6000);
    onVisible = () => {
      if (!document.hidden) void refresh(container);
    };
    document.addEventListener("visibilitychange", onVisible);
  },
  unmount() {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
    if (fanTimer !== null) {
      clearInterval(fanTimer);
      fanTimer = null;
    }
    if (onVisible !== null) {
      document.removeEventListener("visibilitychange", onVisible);
      onVisible = null;
    }
  },
};
