// In-panel modal dialogs (release settings, reset confirmation). The panel
// is a fixed 390×610 window, so dialogs are overlays inside it rather than
// new windows. Focus is trapped while open and handed back on close; Escape
// closes the dialog and never reaches the panel's "Escape hides" handler.

import { h } from "./dom";

/** A dialog handle: `close()` dismisses it, `busy` blocks dismissal. */
export interface DialogHandle {
  close(): void;
  setBusy(busy: boolean): void;
  root: HTMLElement;
}

let openCount = 0;

/** True while any panel dialog is open (the panel ignores Escape then). */
export function dialogOpen(): boolean {
  return openCount > 0;
}

/** Focusable descendants in tab order. */
function focusables(root: HTMLElement): HTMLElement[] {
  return [
    ...root.querySelectorAll<HTMLElement>(
      'button:not([disabled]), select:not([disabled]), input:not([disabled]), [tabindex="0"]',
    ),
  ].filter((el) => !el.hidden && el.offsetParent !== null);
}

/** Open a modal over the panel; `onClose` runs once when it is dismissed. */
export function openDialog(opts: {
  title: string;
  description?: string;
  body?: HTMLElement;
  footer: HTMLElement;
  onClose?: () => void;
}): DialogHandle {
  const host = document.getElementById("panel")!;
  const returnFocus = document.activeElement as HTMLElement | null;
  const titleId = `dlg-${Date.now()}`;
  const box = h(
    "div",
    { class: "dialog", role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
    h("h2", { id: titleId }, opts.title),
    opts.description ? h("p", { class: "dialog-desc" }, opts.description) : null,
    opts.body ?? null,
    opts.footer,
  );
  const scrim = h("div", { class: "scrim" }, box);
  let busy = false;
  let closed = false;

  // Close without letting the click/keypress fall through to the panel.
  const close = () => {
    if (closed) return;
    closed = true;
    openCount -= 1;
    scrim.remove();
    document.removeEventListener("keydown", onKey, true);
    opts.onClose?.();
    if (returnFocus?.isConnected) returnFocus.focus({ preventScroll: true });
    else host.focus({ preventScroll: true });
  };

  // Escape closes; Tab cycles inside the dialog (focus trap).
  const onKey = (ev: KeyboardEvent) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      if (!busy) close();
    } else if (ev.key === "Tab") {
      const items = focusables(box);
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (ev.shiftKey && document.activeElement === first) {
        ev.preventDefault();
        last.focus();
      } else if (!ev.shiftKey && document.activeElement === last) {
        ev.preventDefault();
        first.focus();
      } else if (!box.contains(document.activeElement)) {
        ev.preventDefault();
        first.focus();
      }
    }
  };

  openCount += 1;
  document.addEventListener("keydown", onKey, true);
  host.append(scrim);
  (focusables(box)[0] ?? box).focus({ preventScroll: true });
  return {
    close,
    root: box,
    setBusy(value) {
      busy = value;
      box.setAttribute("aria-busy", String(value));
    },
  };
}
