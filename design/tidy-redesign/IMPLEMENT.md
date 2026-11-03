# 方案 B 落地 — 子代理通用规则

用户选了方案 B「小报 · 模块栏格」。现在把它落到真实应用 `desktop/ui`（无框架 TypeScript + Vite，Tauri 2）。
四个子代理并行，各自只改自己名下的文件。

## 必读

1. `design/tidy-redesign/BRIEF.md` — 问题、品牌基因、8 条自适应硬规则、方案 B 说明。
2. `design/tidy-redesign/b/*.html` + `b/b.css` — 目标外观。这是静态稿，照着它的结构和比例做。
3. `desktop/ui/src/styles/tabloid.css` — **共享部件（已从 b.css 移植）**：`.mast*`、`.page`、`.folio`、`.band/.ticker/.tick*`、`.grid(.auto/.split/.apps)/.mod/.side/.kick/.span-*`、`.headline/.title/.byline/.prose/.dropcap/.note/.muted`、`.btn(.lg/.sm)/.link/.act-row`、`.fig/.databox/.cell*/.pair`、`.kv`、`.tbl/.tr/.ell/.num`、`.badge/.chev/.reveal/.icon/.bar/.meter/.cores/.spark`、原生 checkbox / button / input 默认样式、`≤1100px / ≤980px` 断点。**直接用这些类，不要改这个文件。** 需要共享层改动时写在你的报告里，由 team-lead 统一改。
4. `desktop/ui/src/styles/tokens.css` — 颜色和流体尺寸变量（`--gutter --mod-x --fs-headline --fs-figure --fs-tick`）。不要改。
5. `desktop/ui/src/styles/base.css` — **旧样式，只作参考**。主窗口已不再加载它，最后会被删除。你名下的界面以前依赖的旧规则（开关、分段控件、弹窗等），按方案 B 风格移植到你自己的 `view-*.css`。不要编辑 base.css。
6. 用户全局规则（`~/.claude/CLAUDE.md`）里的注释要求：每个新建或修改过的函数顶部都要有一行注释，说明它做什么。复杂逻辑上方写清楚“为什么这样写”。注释语言和所在文件保持一致（这些文件多为英文）。

## 硬规则

- **只改外观和标记结构**，不改行为：IPC 调用、状态逻辑、事件、遥测、安全相关的分支一律保持原样。JS 用到的 `id`、`data-*` 属性、事件绑定都要保留（可以换到新元素上）。
- **不加新功能、不编数据**。方案 B 静态稿里有几块应用里没有的内容，不要做：清理首页「A3 扫描范围」「A4 往期回收」、扫描页「A2 扫描进度」、完成页「本期账目」。行情带和模块只放应用真实有的数据。
- **不新增 i18n 字符串**（`src/i18n.ts` 是共享文件，不要编辑）。版号、眉题编号（A1、B2…）这类装饰用固定的拉丁字母/数字，不需要翻译。真缺文案时写进报告。
- 页面最外层用 `.page`（它管页边距和最大宽度）；`#view` 不再有内边距。
- 自适应：1320 和 800 两个宽度都必须整齐。不能有横向滚动、叠字、按钮/Tab/数字换行、头条一行只剩一两个字。栏宽用 `minmax()`，不用固定像素的 flex-basis。
- 锈红只给可点击元素和一个强调词。
- 不提交 git。

## 预览和验证（必须做）

- Vite 已在跑：`http://localhost:5790/mock/index.html#/<route>`（`#/clean`、`#/apps`、`#/optimize`、`#/analyze`、`#/status`、`#/settings`）。改动会热更新。不要另起服务。
- 这是假的 Tauri 桥接（`desktop/ui/mock/`）。在你名下的 `mock/fixtures/<区域>.ts` 里用 `on("命令名", handler)` 注册假数据，类型照 `src/types.ts` 和 `src/ipc.ts`。Channel 进度用 `emit(args.onProgress, msg)`；`scene()` 读 `?scene=` 切换场景（如空列表、错误）；`delay(ms)` 模拟耗时。接口见 `mock/registry.ts`。数据要真实可信（参考 `design/tidy-redesign/CONTENT.md`）。
- 用 **ego-browser**（先运行 `ego-browser skill` 并照指南做，**不要用 Chrome**）。只开一个 task space。每个界面的每个状态都在 1320×720 和 800×720 截图（`page.cdp("Emulation.setDeviceMetricsOverride", {width, height, deviceScaleFactor: 1, mobile: false})`），用 Read 看每一张。在 800 宽跑 DOM 检查：横向溢出、文字框相交、nowrap 控件换行。修到没有问题。结束时 `task.finish({ keep: [] })`。
- `cd desktop/ui && npx tsc --noEmit` 必须对你名下的文件零报错（别人的文件此刻可能正在改，报错与你无关就忽略）。
- 截图存到 `/tmp/tidy-impl-<你的区域>/`。

## 报告（简短）

- 改了哪些文件。
- 每个界面/状态在 800 宽怎么变。
- 需要 team-lead 改共享层（tabloid.css / i18n / router）的地方，写清楚具体规则或文案。
- 没做到的规则和原因。
