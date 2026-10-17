// Detail tabs (C4–C10): CPU, GPU, Memory, Disk and Network. Each tab stacks
// its summary card on top of the deeper blocks for that metric; CPU and
// Memory end with the application list, which takes the remaining height
// and scrolls on its own.

import { appList } from "./applist";
import { cpuCard, diskCard, gpuCard, networkCard, rateFigure } from "./cards";
import { openDialog } from "./dialog";
import { h, setAttr, setText, show } from "./dom";
import { factList, type FactDef } from "./facts";
import * as fmt from "./fmt";
import { memoryHeader } from "./memory";
import { sparkline } from "./sparkline";
import type { PanelContext, Widget } from "./widget";
import { t } from "../i18n";
import { residentResetNetwork } from "../ipc";
import type { CoreLevel, ResidentReading, Series } from "../types";

/** Stack widgets into one tab body. */
function stack(cls: string, widgets: Widget[]): Widget {
  const el = h("div", { class: `details ${cls}` });
  widgets.forEach((w) => el.append(w.el));
  return {
    el,
    update(r) {
      widgets.forEach((w) => w.update(r));
    },
    resume() {
      widgets.forEach((w) => w.resume?.());
    },
    dispose() {
      widgets.forEach((w) => w.dispose?.());
    },
  };
}

/** Wrap a fact list as a widget. */
function facts(defs: FactDef<ResidentReading>[], layout: "tiles" | "rows" = "tiles"): Widget {
  const list = factList(defs, layout);
  return { el: list.el, update: (r) => list.update(r) };
}

/** Temperature trend card: label + value, 36px trend. Hidden without data. */
function temperatureCard(
  label: string,
  read: (r: ResidentReading) => { now: number | null; history: Series },
): Widget {
  const value = h("strong", { class: "temp-value mono" });
  const spark = sparkline("temperature", t("rp.tempTrend", { name: label }));
  const el = h(
    "section",
    { class: "card temp-card", "aria-label": label },
    h("header", {}, h("span", { class: "sum-title" }, label), value),
    spark.el,
  );
  return {
    el,
    update(r) {
      const { now, history } = read(r);
      // A Mac that never reports this sensor shows no card at all.
      const any = now !== null || history.some((v) => v !== null);
      show(el, any);
      if (!any) return;
      setText(value, fmt.temp(now));
      setAttr(value, "data-tone", now === null ? null : now >= 80 ? "critical" : now >= 60 ? "warning" : null);
      spark.update(history);
    },
  };
}

/** CPU tab (C4): utilisation card, temperature, facts, app list. */
export function cpuTab(ctx: PanelContext): Widget {
  const pct = (v: number | null): { value: string; unit: string } | null =>
    v === null ? null : { value: fmt.pct(v), unit: "%" };
  // One tile per named performance level (Super × 6, Performance × 12 on
  // M5 Pro). The peak tile gives way to them; Intel reports no levels and
  // shows the plain core count instead.
  const hasClasses = (r: ResidentReading) => coreLevels(r).length > 0;
  const level = (i: number): FactDef<ResidentReading> => ({
    key: `level${i}`,
    label: "",
    labelOf: (r) => levelLabel(coreLevels(r)[i]?.name ?? ""),
    read: (r) => {
      const l = coreLevels(r)[i];
      return l ? { value: String(l.count), unit: t("rp.coresUnit") } : null;
    },
  });
  return stack("with-list", [
    cpuCard(ctx, { interactive: false, heading: t("rp.cpuUsage") }, false),
    temperatureCard(t("rp.cpuTemp"), (r) => ({ now: r.cpu.temp_c, history: r.cpu.temp_history })),
    facts([
      level(0),
      level(1),
      level(2),
      {
        key: "cores",
        label: t("rp.cores"),
        read: (r) => (!hasClasses(r) && r.cpu.core_count ? { value: String(r.cpu.core_count) } : null),
      },
      { key: "avg", label: t("rp.avg1m"), read: (r) => withCache(pct(r.cpu.avg_1m), r) },
      {
        key: "peak",
        label: t("rp.peak1m"),
        read: (r) => (hasClasses(r) ? null : withCache(pct(r.cpu.peak_1m), r)),
      },
    ]),
    appList("cpu"),
  ]);
}

/** Named core levels; falls back to the efficiency/performance split when
 * the backend reports no levels (older data), else none. */
function coreLevels(r: ResidentReading): CoreLevel[] {
  if (r.cpu.core_levels?.length) return r.cpu.core_levels;
  if (r.cpu.e_cores !== null && r.cpu.p_cores !== null) {
    return [
      { name: "Efficiency", count: r.cpu.e_cores },
      { name: "Performance", count: r.cpu.p_cores },
    ];
  }
  return [];
}

/** Tile label for a core level: known names are localized ("超级核心"),
 * unknown ones keep their raw name. */
function levelLabel(name: string): string {
  const key = `rp.level.${name}`;
  const text = t(key);
  return text === key ? name : text;
}

/** Mark a fact as cached when the reading is stale. */
function withCache<T extends { value: string; unit?: string }>(v: T | null, r: ResidentReading) {
  return v === null ? null : { ...v, cached: r.stale };
}

/** GPU engine activity (Renderer/Tiler) with their one-minute histories. */
function engineCard(): Widget {
  const engines = (["renderer", "tiler"] as const).map((key) => {
    const value = h("strong", { class: "mono" });
    const spark = sparkline("usage", t("rp.engineTrend", { name: t(`rp.${key}`) }), "mini");
    const row = h(
      "div",
      { class: "engine" },
      h("span", { class: "engine-name" }, t(`rp.${key}`)),
      spark.el,
      value,
    );
    return { key, value, spark, row };
  });
  const memValue = h("strong", { class: "mono" });
  const memRow = h("div", { class: "engine mem" }, h("span", { class: "engine-name" }, t("rp.gpuMemory")), memValue);
  const el = h(
    "section",
    { class: "card engines", "aria-label": t("rp.engines") },
    h("header", {}, h("span", { class: "sum-title", title: t("rp.enginesHint"), tabindex: 0 }, t("rp.engines"))),
    ...engines.map((e) => e.row),
    memRow,
  );
  return {
    el,
    update(r) {
      let any = false;
      for (const e of engines) {
        const v = r.gpu[e.key];
        const history: Series = r.gpu[`${e.key}_history`] ?? [];
        const seen = v !== null || history.some((x) => x !== null);
        show(e.row, seen);
        any ||= seen;
        setText(e.value, v === null ? "—" : `${fmt.pct(v)}%`);
        e.spark.update(history);
      }
      show(memRow, r.gpu.memory_used_bytes !== null);
      setText(memValue, fmt.memory(r.gpu.memory_used_bytes));
      show(el, any || r.gpu.memory_used_bytes !== null);
    },
  };
}

/** GPU tab (C5): utilisation, temperature, engine activity, facts. */
export function gpuTab(ctx: PanelContext): Widget {
  return stack("scroll", [
    gpuCard(ctx, { interactive: false, heading: t("rp.gpuUsage") }),
    temperatureCard(t("rp.gpuTemp"), (r) => ({ now: r.gpu.temp_c, history: r.gpu.temp_history })),
    engineCard(),
    facts([
      {
        key: "cores",
        label: t("rp.gpuCores"),
        read: (r) => (r.gpu.core_count ? { value: String(r.gpu.core_count) } : null),
      },
      {
        key: "avg",
        label: t("rp.avg1m"),
        read: (r) => (r.gpu.avg_1m === null ? null : { value: fmt.pct(r.gpu.avg_1m), unit: "%", cached: r.stale }),
      },
      {
        key: "peak",
        label: t("rp.peak1m"),
        read: (r) => (r.gpu.peak_1m === null ? null : { value: fmt.pct(r.gpu.peak_1m), unit: "%", cached: r.stale }),
      },
    ]),
  ]);
}

/** Memory tab (C6): header with release split button, then the app list. */
export function memoryTab(): Widget {
  return stack("with-list", [memoryHeader(), appList("memory")]);
}

/** Disk activity card: read/write rates and totals over a duplex trend. */
function diskActivity(): Widget {
  const read = rateFigure("down", t("rp.read"));
  const write = rateFigure("up", t("rp.write"));
  const readTotal = h("dd", { class: "mono" });
  const writeTotal = h("dd", { class: "mono" });
  const spark = sparkline("duplex", t("rp.diskActivity"));
  /** One column: direction label + live rate, then the since-boot total. */
  const column = (label: string, dir: "down" | "up", fig: HTMLElement, totalLabel: string, total: HTMLElement) =>
    h(
      "dl",
      { class: "io-col" },
      h("div", { class: "io-rate" }, h("dt", { class: dir }, label), h("dd", {}, fig)),
      h("div", { class: "io-total" }, h("dt", {}, totalLabel), total),
    );
  const el = h(
    "section",
    { class: "card activity", "aria-label": t("rp.diskActivity") },
    h(
      "header",
      {},
      h("span", { class: "sum-title", title: t("rp.diskSystemHint"), tabindex: 0 }, t("rp.diskActivity")),
      h("span", { class: "sub" }, `${t("rp.allDisks")} · ${t("rp.lastMinute")}`),
    ),
    h(
      "div",
      { class: "io-cols" },
      column(t("rp.read"), "down", read.el, t("rp.systemRead"), readTotal),
      column(t("rp.write"), "up", write.el, t("rp.systemWritten"), writeTotal),
    ),
    spark.el,
  );
  return {
    el,
    update(r) {
      read.set(r.disk.read_bps);
      write.set(r.disk.write_bps);
      setText(readTotal, fmt.bytes(r.disk.read_total_bytes));
      setText(writeTotal, fmt.bytes(r.disk.write_total_bytes));
      spark.update(r.disk.io_history);
    },
  };
}

/** Disk health card: SMART status chip and the facts the drive reports. */
function diskHealth(): Widget {
  const status = h("span", { class: "chip health", tabindex: 0 });
  const source = h("span", { class: "sub ell" });
  const mountedAt = performance.now();
  const list = factList<ResidentReading>(
    [
      {
        key: "health",
        label: t("rp.health.percent"),
        hint: t("rp.health.wearHint"),
        read: (r) => num(r.disk.health?.health_percent, "%"),
      },
      {
        key: "temp",
        label: t("rp.health.temp"),
        read: (r) => {
          const c = r.disk.health?.temp_c ?? r.disk.temp_c;
          return c === null || c === undefined ? null : { value: fmt.temp(c) };
        },
      },
      { key: "read", label: t("rp.health.read"), read: (r) => size(r.disk.health?.data_read_bytes) },
      { key: "written", label: t("rp.health.written"), read: (r) => size(r.disk.health?.data_written_bytes) },
      {
        key: "hours",
        label: t("rp.health.hours"),
        read: (r) => num(r.disk.health?.power_on_hours, t("rp.health.hoursUnit")),
      },
      { key: "cycles", label: t("rp.health.cycles"), read: (r) => num(r.disk.health?.power_cycles) },
      { key: "spare", label: t("rp.health.spare"), read: (r) => num(r.disk.health?.available_spare_percent, "%") },
      { key: "media", label: t("rp.health.media"), read: (r) => num(r.disk.health?.media_errors) },
      {
        key: "critical",
        label: t("rp.health.critical"),
        read: (r) => {
          const v = r.disk.health?.critical_warning;
          return v === null || v === undefined ? null : { value: v === 0 ? t("rp.health.none") : String(v) };
        },
      },
    ],
    "tiles",
  );
  const el = h(
    "section",
    { class: "card health-card", "aria-label": t("rp.health.title") },
    h("header", {}, h("span", { class: "sum-title" }, t("rp.health.title")), status),
    source,
    list.el,
  );
  return {
    el,
    update(r) {
      const hl = r.disk.health;
      // SMART is read only while this tab is open; give the first read a
      // few seconds before calling it unavailable.
      const key = hl
        ? ["verified", "warning", "failing"].includes(hl.status)
          ? hl.status
          : "unknown"
        : performance.now() - mountedAt < 8000
          ? "loading"
          : "unavailable";
      setAttr(status, "data-status", key);
      setText(status, t(`rp.health.${key}`));
      setAttr(status, "title", t(hl ? "rp.health.smartHint" : "rp.health.unavailableHint"));
      const fs = hl?.file_system;
      setText(source, [r.disk.volume_name || t("rp.systemDisk"), fs].filter(Boolean).join(" · "));
      list.update(r);
    },
  };
}

/** Integer fact helper (null hides the fact). */
function num(v: number | null | undefined, unit?: string) {
  return v === null || v === undefined ? null : { value: fmt.count(v), unit };
}

/** Byte-size fact helper (null hides the fact). */
function size(v: number | null | undefined) {
  if (v === null || v === undefined) return null;
  const [value, unit] = fmt.bytes(v).split(" ");
  return { value, unit };
}

/** Disk tab (C9): summary with the clean button, activity, health. */
export function diskTab(ctx: PanelContext): Widget {
  return stack("scroll", [
    diskCard(ctx, { interactive: false, cleanButton: true }),
    diskActivity(),
    diskHealth(),
  ]);
}

/** Traffic card: cumulative totals with session values and a reset. */
function trafficCard(): Widget {
  const rx = h("dd", { class: "mono" });
  const tx = h("dd", { class: "mono" });
  const rxSession = h("small", { title: t("rp.net.sessionHint"), tabindex: 0 });
  const txSession = h("small", { title: t("rp.net.sessionHint"), tabindex: 0 });
  let iface = "";
  const reset = h(
    "button",
    { type: "button", class: "btn small", title: t("rp.net.resetHint") },
    t("rp.net.reset"),
  );
  reset.addEventListener("click", () => confirmReset(reset, iface || t("rp.automatic")));
  const el = h(
    "section",
    { class: "card traffic", "aria-label": t("rp.net.traffic") },
    h("header", {}, h("span", { class: "sum-title" }, t("rp.net.traffic")), reset),
    h(
      "dl",
      { class: "traffic-facts" },
      h("div", {}, h("dt", {}, h("b", { class: "down" }, "↓ "), t("rp.net.totalRx")), rx, rxSession),
      h("div", {}, h("dt", {}, h("b", { class: "up" }, "↑ "), t("rp.net.totalTx")), tx, txSession),
    ),
  );
  return {
    el,
    update(r) {
      const n = r.network;
      iface = n.details?.interface ?? n.interface ?? "";
      setText(rx, fmt.bytes(n.total_rx_bytes));
      setText(tx, fmt.bytes(n.total_tx_bytes));
      setText(rxSession, t("rp.net.session", { value: fmt.bytes(n.session_rx_bytes) }));
      setText(txSession, t("rp.net.session", { value: fmt.bytes(n.session_tx_bytes) }));
    },
  };
}

/** "Reset traffic totals?" confirmation (C10). */
function confirmReset(trigger: HTMLButtonElement, name: string): void {
  const error = h("p", { class: "alert", role: "alert" }, t("rp.net.resetFailed"));
  error.hidden = true;
  const cancel = h("button", { type: "button", class: "btn" }, t("rp.cancel"));
  const ok = h("button", { type: "button", class: "btn danger" }, t("rp.net.reset"));
  const dlg = openDialog({
    title: t("rp.net.resetTitle"),
    description: t("rp.net.resetDesc", { name }),
    body: error,
    footer: h("div", { class: "dialog-actions" }, cancel, ok),
    onClose: () => trigger.isConnected && trigger.focus(),
  });
  cancel.addEventListener("click", () => dlg.close());
  ok.addEventListener("click", async () => {
    dlg.setBusy(true);
    ok.disabled = cancel.disabled = true;
    try {
      await residentResetNetwork();
      dlg.setBusy(false);
      dlg.close();
    } catch {
      dlg.setBusy(false);
      ok.disabled = cancel.disabled = false;
      error.hidden = false;
    }
  });
}

/** Interface card: name, status, addresses (copyable), Wi-Fi details. */
function interfaceCard(): Widget {
  const status = h("span", { class: "chip" });
  const rows = factList<ResidentReading>(
    [
      { key: "iface", label: t("rp.net.interface"), read: (r) => text(r.network.details?.interface ?? r.network.interface) },
      { key: "mac", label: t("rp.net.mac"), copy: true, read: (r) => text(r.network.details?.mac) },
      { key: "ipv4", label: "IPv4", copy: true, read: (r) => text(r.network.details?.ipv4.join(", ")) },
      { key: "ipv6", label: "IPv6", copy: true, read: (r) => text(r.network.details?.ipv6[0]) },
      { key: "std", label: t("rp.net.standard"), read: (r) => text(r.network.details?.wifi_standard) },
      { key: "chan", label: t("rp.net.channel"), read: (r) => text(r.network.details?.wifi_channel) },
    ],
    "rows",
  );
  const loading = h("p", { class: "muted-note", role: "status" }, t("rp.loading"));
  const el = h(
    "section",
    { class: "card iface", "aria-label": t("rp.net.interfaceTitle") },
    h("header", {}, h("span", { class: "sum-title" }, t("rp.net.interfaceTitle")), status),
    loading,
    rows.el,
  );
  return {
    el,
    update(r) {
      const d = r.network.details;
      show(loading, !d);
      show(status, !!d);
      if (d) {
        setText(status, t(d.connected ? "rp.net.connected" : "rp.net.disconnected"));
        setAttr(status, "data-status", d.connected ? "ok" : "off");
      }
      rows.update(r);
      if (!d) show(rows.el, false);
    },
  };
}

/** Non-empty text fact, else hidden. */
function text(v: string | null | undefined) {
  return v ? { value: v } : null;
}

/** Network tab (C10): summary, traffic totals, interface details. */
export function networkTab(ctx: PanelContext): Widget {
  return stack("scroll", [
    networkCard(ctx, { interactive: false }),
    trafficCard(),
    interfaceCard(),
  ]);
}
