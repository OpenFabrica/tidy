// Mock IPC registry shared by tauri.ts and every fixture file. Kept in its
// own module so fixtures can import it without a circular dependency.
//
// Fixture usage:
//   import { on, emit, scene, delay } from "../registry";
//   on("plan_clean", async (args) => { ...; return payload; });
//   Inside a handler, emit(args.onProgress, message) pushes a Channel message.
// Unregistered commands resolve to null with a "[mock]" console warning.

export type Args = Record<string, unknown>;
type Handler = (args: Args) => unknown | Promise<unknown>;

export const handlers = new Map<string, Handler>();
export const callbacks = new Map<number, (payload: unknown) => void>();
export const counter = { next: 1 };
/** 每个 Channel 的下一条消息序号（Channel 按序号保证顺序）。 */
const channelIndex = new Map<number, number>();

/** 注册一个命令的假实现。后注册的覆盖先注册的。 */
export function on(cmd: string, handler: Handler): void {
  handlers.set(cmd, handler);
}

/** 当前场景名：URL 里的 ?scene=xxx，缺省 "default"。 */
export function scene(): string {
  return new URLSearchParams(location.search).get("scene") ?? "default";
}

/** 等待若干毫秒，用来模拟扫描耗时。 */
export function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** 取出参数里的 Channel 回调 id（Channel 实例或 "__CHANNEL__:id" 字符串）。 */
function channelId(ch: unknown): number | null {
  if (ch && typeof ch === "object" && "id" in ch) return Number((ch as { id: number }).id);
  if (typeof ch === "string" && ch.startsWith("__CHANNEL__:")) return Number(ch.slice(12));
  return null;
}

/** 向界面传来的 Channel 推送一条消息。 */
export function emit(ch: unknown, message: unknown): void {
  const id = channelId(ch);
  if (id === null) return;
  const index = channelIndex.get(id) ?? 0;
  channelIndex.set(id, index + 1);
  callbacks.get(id)?.({ index, message });
}

/** 向界面派发一个 Tauri 事件（listen() 订阅的那种）。 */
export function emitEvent(event: string, payload: unknown): void {
  for (const [id, sub] of listeners) {
    if (sub === event) callbacks.get(id)?.({ event, id, payload });
  }
}

/** listen() 的订阅：回调 id → 事件名。 */
export const listeners = new Map<number, string>();

// 事件插件与对话框插件的最小实现，界面启动时会调用。
on("plugin:event|listen", (args) => {
  listeners.set(Number(args.handler), String(args.event));
  return Number(args.handler);
});
on("plugin:event|unlisten", () => null);
on("plugin:dialog|open", () => null);

