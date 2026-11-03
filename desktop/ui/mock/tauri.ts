// 假的 Tauri 桥接：让真实界面在普通浏览器里跑起来，用于版式验证。
// 只在 mock/index.html 里加载，不进生产构建。命令的假数据在 fixtures/*.ts，
// 注册接口在 registry.ts。

import { callbacks, counter, handlers, type Args } from "./registry";
import "./fixtures/shell";
import "./fixtures/clean";
import "./fixtures/apps";
import "./fixtures/optimize";
import "./fixtures/analyze";
import "./fixtures/status";
import "./fixtures/settings";

// 界面语言：默认中文（浏览器多半是英文），?lang=en 切到英文。
localStorage.setItem("mole-lang", new URLSearchParams(location.search).get("lang") ?? "zh");

(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  transformCallback(cb: (payload: unknown) => void, once = false): number {
    const id = counter.next++;
    callbacks.set(id, (payload) => {
      if (once) callbacks.delete(id);
      cb(payload);
    });
    return id;
  },
  unregisterCallback(id: number): void {
    callbacks.delete(id);
  },
  async invoke(cmd: string, args: Args = {}): Promise<unknown> {
    const handler = handlers.get(cmd);
    if (!handler) {
      console.warn(`[mock] no handler for ${cmd}`, args);
      return null;
    }
    return await handler(args);
  },
  convertFileSrc(path: string): string {
    return path;
  },
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
};
