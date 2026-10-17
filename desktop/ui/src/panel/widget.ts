// Shared shapes for panel widgets: every block is created once per tab and
// patched in place by `update` on each reading.

import type { MainDestination, PanelTab, ResidentReading } from "../types";

/** A mounted block of panel UI. */
export interface Widget {
  el: HTMLElement;
  update(r: ResidentReading): void;
  /** The panel was shown again (lists restart their loading grace). */
  resume?(): void;
  /** Called when the tab is torn down (timers, observers). */
  dispose?(): void;
}

/** Panel-level actions widgets may trigger. */
export interface PanelContext {
  selectTab(tab: PanelTab): void;
  openMain(destination: MainDestination): void;
  /** Report a failed panel action through the error banner. */
  fail(): void;
  /** True while the panel window is on screen. */
  visible(): boolean;
}
