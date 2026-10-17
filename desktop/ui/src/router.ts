// Hash router with a mount/unmount lifecycle. Renders the newspaper masthead
// (wordmark · section tabs · marginalia) above the double rule, applies the
// per-route body theme, and re-renders on language switches (setLang fires a
// hashchange). Settings and language switching live in the tray menu / the
// settings view, not in the masthead.

import { mastheadDate, t } from "./i18n";
import { track } from "./telemetry";

export interface View {
  /** Render into the container; called on every navigation to this route. */
  mount(container: HTMLElement): void;
  /** Tear down timers/subscriptions; called before leaving the route. */
  unmount?(): void;
}

interface Route {
  hash: string;
  labelKey: string;
  theme: string;
  minor: boolean;
  hidden: boolean;
  view: View;
}

const routes: Route[] = [];
let current: View | null = null;

/** True while a view owns the masthead marginalia, so the clock stays quiet. */
let metaOverridden = false;
let clockTimer: number | null = null;

/** Register a view under a hash route; hidden routes stay navigable (tray) but render no nav link. */
export function register(
  hash: string,
  labelKey: string,
  theme: string,
  view: View,
  minor = false,
  hidden = false,
): void {
  routes.push({ hash, labelKey, theme, view, minor, hidden });
}

/**
 * Replace the masthead's right-hand marginalia with view-supplied copy
 * (already-escaped HTML). Passing null hands the slot back to the dateline.
 * Reset on every navigation, so a view must re-assert it after mounting.
 */
export function setNavMeta(html: string | null): void {
  const el = document.getElementById("nav-meta");
  metaOverridden = html !== null;
  if (!el) return;
  el.innerHTML = html ?? mastheadDate();
}

/** Render the masthead for the active route. */
function renderNav(activeHash: string): void {
  const nav = document.getElementById("nav")!;
  const visible = routes.filter((r) => !r.hidden);
  const major = visible.filter((r) => !r.minor);
  const minor = visible.filter((r) => r.minor);
  const link = (r: Route, cls: string) =>
    `<a href="${r.hash}" class="${cls} ${r.hash === activeHash ? "active" : ""}">${t(r.labelKey)}</a>`;
  nav.innerHTML = `
    <div class="masthead">
      <span class="mast-wordmark">TIDY</span>
      <div class="mast-tabs">
        ${major.map((r) => link(r, "")).join("")}
        ${minor.length > 0 ? `<span class="mast-sep"></span>${minor.map((r) => link(r, "minor")).join("")}` : ""}
      </div>
      <span class="mast-meta" id="nav-meta">${mastheadDate()}</span>
    </div>
    <div class="mast-rules"></div>`;
}

/** Navigate to the current location.hash (first route on unknown). A
 * `?query` suffix (e.g. `#/settings?section=menubar`) is left for the view
 * to read and ignored when matching the route. */
function navigate(): void {
  const container = document.getElementById("view")!;
  const hash = location.hash.split("?")[0] || routes[0].hash;
  const route = routes.find((r) => r.hash === hash) ?? routes[0];
  current?.unmount?.();
  container.innerHTML = "";
  document.body.dataset.theme = route.theme;
  current = route.view;
  metaOverridden = false;
  renderNav(route.hash);
  // 界面名取自路由注册表而不是 location.hash：未知 hash 会落到第一个路由，
  // 上报的必须是真正渲染出来的那个界面。
  track({ kind: "view_opened", view: route.hash.replace(/^#\//, "") });
  route.view.mount(container);
}

/** Start routing. */
export function start(): void {
  window.addEventListener("hashchange", navigate);
  navigate();
  // The dateline is part of the page furniture, so it has to stay honest;
  // a view that claimed the slot keeps it until the next navigation.
  clockTimer = window.setInterval(() => {
    if (metaOverridden || document.hidden) return;
    const el = document.getElementById("nav-meta");
    if (el) el.textContent = mastheadDate();
  }, 20_000);
}

/** Stop the masthead clock (tests / teardown). */
export function stop(): void {
  if (clockTimer !== null) {
    clearInterval(clockTimer);
    clockTimer = null;
  }
}
