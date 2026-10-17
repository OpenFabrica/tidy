// Number formatting for the tray panel: percentages, rates, capacities and
// temperatures. Memory is shown in binary units (a 16 GiB Mac reads "16 GB",
// like Activity Monitor); disk capacity and transfer rates in decimal units
// (like Finder and the network panes).

import { formatTemp } from "../prefs";

/** A split figure: numeral and unit set at different sizes. */
export interface Figure {
  value: string;
  unit: string;
}

/** Rounded percent numeral, "—" when unknown. */
export function pct(value: number | null | undefined): string {
  return value === null || value === undefined || !Number.isFinite(value)
    ? "—"
    : Math.round(Math.max(0, value)).toString();
}

/** Scale a value through a unit ladder. */
function ladder(value: number, base: number, units: string[]): Figure {
  let v = Math.max(0, value);
  let i = 0;
  while (v >= base && i < units.length - 1) {
    v /= base;
    i += 1;
  }
  const digits = i === 0 || v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return { value: v.toFixed(digits), unit: units[i] };
}

/** Transfer rate, decimal: "1.24 MB/s". */
export function rate(bps: number | null | undefined): Figure {
  if (bps === null || bps === undefined) return { value: "—", unit: "" };
  return ladder(bps, 1000, ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"]);
}

/** Decimal byte size (disk, traffic totals). */
export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  const f = ladder(n, 1000, ["B", "KB", "MB", "GB", "TB", "PB"]);
  return `${f.value} ${f.unit}`;
}

/** Binary byte size for memory, labelled GB/MB like Activity Monitor. */
export function memory(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  const f = ladder(n, 1024, ["B", "KB", "MB", "GB", "TB"]);
  // Whole numbers read cleaner for installed RAM ("16 GB", not "16.00 GB").
  return `${f.value.replace(/\.0+$/, "")} ${f.unit}`;
}

/** Temperature in the user's unit (shared main-window pref), "—" when unknown. */
export function temp(celsius: number | null | undefined): string {
  return celsius === null || celsius === undefined ? "—" : formatTemp(celsius);
}

/** Group digits for counters (power cycles, hours). */
export function count(n: number | null | undefined): string {
  return n === null || n === undefined ? "—" : Math.round(n).toLocaleString();
}
