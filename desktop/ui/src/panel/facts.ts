// Fact tiles and fact rows (the "1 分钟平均 · 37%" boxes and the interface
// table). Facts whose value is unknown are hidden rather than shown as dashes
// when the hardware simply does not report them.

import { h, setAttr, setText, show } from "./dom";
import { t } from "../i18n";

/** One fact's current value; null hides it. */
export interface FactValue {
  value: string;
  unit?: string;
  /** Value comes from an older sample ("缓存" tag). */
  cached?: boolean;
}

/** A fact definition: label + how to read it. */
export interface FactDef<R> {
  key: string;
  label: string;
  /** Label that depends on the reading (e.g. a named core level). */
  labelOf?(r: R): string;
  hint?: string;
  /** Show a copy button instead of a plain value. */
  copy?: boolean;
  read(r: R): FactValue | null;
}

/** A grid (tiles) or rows (label ··· value) of facts. */
export function factList<R>(defs: FactDef<R>[], layout: "tiles" | "rows" = "tiles") {
  const el = h("dl", { class: `facts ${layout}` });
  const items = defs.map((def) => {
    const cached = h("span", { class: "cached" }, t("rp.cached"));
    const label = h("span", { class: "fact-label", title: def.hint ?? null, tabindex: def.hint ? 0 : null });
    label.textContent = def.label;
    const num = h("span", { class: "fact-num" });
    const unit = h("small");
    const dd = h("dd", {}, num, unit);
    let copyBtn: HTMLButtonElement | null = null;
    let copyValue = "";
    if (def.copy) {
      copyBtn = h("button", {
        type: "button",
        class: "copy",
        onclick: () => {
          void navigator.clipboard
            ?.writeText(copyValue)
            .then(() => {
              copyBtn!.dataset.copied = "1";
              setAttr(copyBtn!, "aria-label", t("rp.copied"));
              window.setTimeout(() => {
                delete copyBtn!.dataset.copied;
                setAttr(copyBtn!, "aria-label", t("rp.copyNamed", { name: def.label }));
              }, 1500);
            })
            .catch(() => {});
        },
        "aria-label": t("rp.copyNamed", { name: def.label }),
        title: t("rp.copy"),
      });
      dd.replaceChildren(copyBtn);
    }
    const row = h("div", { class: "fact", "data-key": def.key }, h("dt", {}, label, cached), dd);
    el.append(row);
    return { def, row, label, num, unit, cached, copyBtn, setCopy: (v: string) => (copyValue = v) };
  });
  return {
    el,
    update(r: R) {
      let visible = 0;
      for (const item of items) {
        const v = item.def.read(r);
        show(item.row, v !== null);
        if (!v) continue;
        visible += 1;
        show(item.cached, !!v.cached);
        if (item.def.labelOf) setText(item.label, item.def.labelOf(r));
        if (item.copyBtn) {
          setText(item.copyBtn, v.value);
          item.setCopy(v.value);
        } else {
          setText(item.num, v.value);
          setText(item.unit, v.unit ?? "");
        }
      }
      show(el, visible > 0);
      el.dataset.count = String(visible);
    },
  };
}
