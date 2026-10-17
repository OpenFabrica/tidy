// Summary cards (C3): the CPU, GPU, memory, disk and network blocks shown on
// the Overview tab and, non-clickable, at the top of each detail tab. Each
// card keeps a fixed height so numbers changing never shift the layout.

import { h, icon, setAttr, setText, show } from "./dom";
import * as fmt from "./fmt";
import { sparkline } from "./sparkline";
import type { PanelContext, Widget } from "./widget";
import { t } from "../i18n";
import type { PanelTab, Pressure } from "../types";

/** Options shared by every summary card. */
interface CardOptions {
  /** Whole card navigates to its tab (Overview only). */
  interactive: boolean;
  /** Override the label (detail tabs say "CPU 利用率"). */
  heading?: string;
}

/** Card frame: optional full-card nav button, header label + chevron. */
function frame(metric: PanelTab, label: string, ctx: PanelContext, opts: CardOptions) {
  const el = h("section", { class: "card sum", "data-metric": metric, "aria-label": label });
  if (opts.interactive) {
    el.classList.add("nav");
    el.append(
      h("button", {
        type: "button",
        class: "card-nav",
        "aria-label": t("rp.openTab", { name: label }),
        onclick: () => ctx.selectTab(metric),
      }),
    );
  }
  const labelEl = h(
    "span",
    { class: "sum-label" },
    h("span", { class: "sum-title" }, label),
    opts.interactive ? icon("chevronRight", 11, "chev") : null,
  );
  return { el, labelEl };
}

/** Big serif percent figure ("37%"), "—" while unknown. */
function percentFigure() {
  const num = h("span", { class: "num" }, "—");
  const unit = h("small", {}, "%");
  const el = h("strong", { class: "sum-value" }, num, unit);
  return {
    el,
    set(value: number | null | undefined) {
      const text = fmt.pct(value);
      setText(num, text);
      show(unit, text !== "—");
    },
  };
}

/** Thin capacity bar (memory, disk). */
function capacityBar(label: string) {
  const fill = h("i");
  const el = h(
    "div",
    { class: "bar", role: "meter", "aria-label": label, "aria-valuemin": 0, "aria-valuemax": 100 },
    fill,
  );
  return {
    el,
    set(value: number | null | undefined) {
      const v = value === null || value === undefined ? null : Math.max(0, Math.min(100, value));
      fill.style.width = v === null ? "0" : `${v}%`;
      setAttr(el, "aria-valuenow", v === null ? null : String(Math.round(v)));
    },
  };
}

/** Temperature chip; tone follows the default 60/80 °C rules. */
export function tempChip(): { el: HTMLElement; set(c: number | null | undefined): void } {
  const el = h("span", { class: "chip temp" });
  return {
    el,
    set(c) {
      show(el, c !== null && c !== undefined);
      if (c === null || c === undefined) return;
      setText(el, fmt.temp(c));
      setAttr(el, "data-tone", c >= 80 ? "critical" : c >= 60 ? "warning" : null);
    },
  };
}

/** Memory pressure chip: dot + label, tooltip explains what to do. */
export function pressureChip(): { el: HTMLElement; set(p: Pressure, stale: boolean): void } {
  const text = h("span");
  const el = h("span", { class: "chip pressure", tabindex: 0 }, h("i", { class: "dot" }), text);
  return {
    el,
    set(p, stale) {
      const key = stale && p !== "unknown" ? "stale" : p === "unknown" ? "unavailable" : p;
      setAttr(el, "data-pressure", key);
      setText(text, t(`rp.pressure.${key}`));
      setAttr(el, "title", t(`rp.pressure.${key}Hint`));
    },
  };
}

/** CPU summary: percent, sparkline, model + temperature chip. */
export function cpuCard(ctx: PanelContext, opts: CardOptions, withTemp = true): Widget {
  const { el, labelEl } = frame("cpu", opts.heading ?? "CPU", ctx, opts);
  const value = percentFigure();
  const spark = sparkline("usage", t("rp.cpuTrend"));
  const model = h("span", { class: "ell" });
  const chip = tempChip();
  el.append(
    h("header", {}, labelEl, value.el),
    spark.el,
    h("div", { class: "sum-meta" }, model, withTemp ? chip.el : null),
  );
  return {
    el,
    update(r) {
      value.set(r.cpu.usage);
      spark.update(r.cpu.history);
      setText(model, r.cpu.model || t("rp.lastMinute"));
      setAttr(model, "title", r.cpu.model || null);
      chip.set(r.cpu.temp_c);
    },
  };
}

/** GPU summary: percent, sparkline, adapter name + temperature chip. */
export function gpuCard(ctx: PanelContext, opts: CardOptions): Widget {
  const { el, labelEl } = frame("gpu", opts.heading ?? "GPU", ctx, opts);
  const value = percentFigure();
  const spark = sparkline("usage", t("rp.gpuTrend"));
  const name = h("span", { class: "ell" });
  const chip = tempChip();
  el.append(h("header", {}, labelEl, value.el), spark.el, h("div", { class: "sum-meta" }, name, chip.el));
  return {
    el,
    update(r) {
      value.set(r.gpu.usage);
      spark.update(r.gpu.history);
      setText(name, r.gpu.name || t("rp.lastMinute"));
      setAttr(name, "title", r.gpu.name || null);
      chip.set(r.gpu.temp_c);
    },
  };
}

/** Memory summary: percent, bar, used / total + pressure chip. */
export function memoryCard(ctx: PanelContext, opts: CardOptions): Widget {
  const { el, labelEl } = frame("memory", t("rp.tab.memory"), ctx, opts);
  const value = percentFigure();
  const bar = capacityBar(t("rp.memoryUsage"));
  const used = h("span", { class: "ell mono" });
  const chip = pressureChip();
  el.append(h("header", {}, labelEl, value.el), bar.el, h("div", { class: "sum-meta" }, used, chip.el));
  // The chip sits above the card's nav button so its tooltip stays reachable.
  chip.el.classList.add("lift");
  return {
    el,
    update(r) {
      const m = r.memory;
      value.set(m.usage_percent);
      bar.set(m.usage_percent);
      setText(used, m.total_bytes ? `${fmt.memory(m.used_bytes)} / ${fmt.memory(m.total_bytes)}` : "—");
      chip.set(m.pressure, r.stale && m.usage_percent === null);
    },
  };
}

/** Disk summary: volume, used percent, bar, available / total + clean action. */
export function diskCard(ctx: PanelContext, opts: CardOptions & { cleanButton?: boolean }): Widget {
  const { el, labelEl } = frame("disk", t("rp.tab.disk"), ctx, opts);
  const source = h("span", { class: "sum-source" });
  labelEl.append(source);
  const value = percentFigure();
  const bar = capacityBar(t("rp.tab.disk"));
  const avail = h("span", { class: "ell mono", title: t("rp.diskCapacityHint"), tabindex: 0 });
  const clean = opts.cleanButton
    ? h(
        "button",
        { type: "button", class: "btn primary small lift", onclick: () => ctx.openMain("clean") },
        icon("broom", 12),
        t("rp.clean"),
      )
    : h(
        "button",
        { type: "button", class: "link lift", onclick: () => ctx.openMain("clean") },
        t("rp.clean"),
        h("span", { "aria-hidden": "true" }, " ›"),
      );
  el.append(h("header", {}, labelEl, value.el), bar.el, h("div", { class: "sum-meta" }, avail, clean));
  return {
    el,
    update(r) {
      const d = r.disk;
      setText(source, d.volume_name || t("rp.systemDisk"));
      value.set(d.used_percent);
      bar.set(d.used_percent);
      setText(
        avail,
        d.total_bytes
          ? `${t("rp.available")} ${fmt.bytes(d.available_bytes)} / ${fmt.bytes(d.total_bytes)}`
          : "—",
      );
    },
  };
}

/** A "↑ 1.2 MB/s" rate figure in a fixed grid so digits never jiggle. */
export function rateFigure(direction: "up" | "down", label: string) {
  const num = h("strong");
  const unit = h("small");
  const el = h(
    "span",
    { class: `rate ${direction}`, "aria-label": label },
    h("b", { "aria-hidden": "true" }, direction === "up" ? "↑" : "↓"),
    num,
    unit,
  );
  return {
    el,
    set(bps: number | null | undefined) {
      const f = fmt.rate(bps);
      setText(num, f.value);
      setText(unit, f.unit);
    },
  };
}

/** Network summary: upload/download rates, duplex sparkline, interface. */
export function networkCard(ctx: PanelContext, opts: CardOptions): Widget {
  const { el, labelEl } = frame("network", t("rp.tab.network"), ctx, opts);
  const up = rateFigure("up", t("rp.upload"));
  const down = rateFigure("down", t("rp.download"));
  const spark = sparkline("duplex", t("rp.netTrend"));
  const iface = h("span", { class: "ell", title: t("rp.networkScope") });
  el.append(
    h("header", {}, labelEl, h("div", { class: "rates" }, up.el, down.el)),
    spark.el,
    h("div", { class: "sum-meta" }, iface, h("span", {}, t("rp.lastMinute"))),
  );
  return {
    el,
    update(r) {
      up.set(r.network.tx_bps);
      down.set(r.network.rx_bps);
      spark.update(r.network.history);
      setText(iface, r.network.interface ?? t("rp.automatic"));
    },
  };
}

/** Overview tab (C3): CPU+GPU side by side, then memory, disk, network. */
export function overviewTab(ctx: PanelContext): Widget {
  const opts = { interactive: true };
  const cards = [
    cpuCard(ctx, opts),
    gpuCard(ctx, opts),
    memoryCard(ctx, opts),
    diskCard(ctx, opts),
    networkCard(ctx, opts),
  ];
  const el = h("div", { class: "overview" });
  cards.forEach((c) => el.append(c.el));
  return {
    el,
    update(r) {
      cards.forEach((c) => c.update(r));
    },
  };
}
