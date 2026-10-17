// Local preferences (persisted in localStorage). The menu-bar switch is not
// one of them: resident.json (resident_get_prefs) is its only source of truth.

type Keys =
  | "temp-unit"
  | "delete-mode"
  | "purge-idle-days"
  | "docker-idle-months";

/** Read a preference with a default. */
export function pref(key: Keys, def: string): string {
  return localStorage.getItem(`mole-${key}`) ?? def;
}

/** Write a preference. */
export function setPref(key: Keys, value: string): void {
  localStorage.setItem(`mole-${key}`, value);
}

/** Battery/thermal display unit. */
export const tempUnit = () => pref("temp-unit", "c");

/** Format a °C reading in the chosen unit. */
export function formatTemp(celsius: number): string {
  if (tempUnit() === "f") return `${((celsius * 9) / 5 + 32).toFixed(0)}°F`;
  return `${celsius.toFixed(0)}°C`;
}

/** Clean cache delete mode: "permanent" (default, CLI parity) or "trash". */
export const cleanTrashMode = () => pref("delete-mode", "permanent") === "trash";

/** Read a positive integer pref, falling back to the default when unset or
 * malformed (an empty number input must never turn into "0 days"). */
function intPref(key: Keys, def: number): number {
  const n = Number.parseInt(pref(key, String(def)), 10);
  return Number.isFinite(n) && n > 0 ? n : def;
}

/** Project deps idle for at least this many days are checked by default. */
export const purgeIdleDays = () => intPref("purge-idle-days", 30);

/** Docker images unused for at least this many months are checked by default. */
export const dockerIdleMonths = () => intPref("docker-idle-months", 6);
