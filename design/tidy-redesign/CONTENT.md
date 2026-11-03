# TIDY desktop UI: screen-by-screen content inventory

Source: `/Users/mike/source/project/openfabrica/tidy/desktop/ui/src`. All strings are given in their resolved Chinese form. Pixel values are quoted from `styles/base.css` unless another file is named.

## 0. Window and global shell

- **Window** (`/Users/mike/source/project/openfabrica/tidy/desktop/src-tauri/tauri.conf.json`): default **1320×720**, min **800×560**, `titleBarStyle: "Overlay"`, `hiddenTitle: true`. The traffic lights float over the top-left corner.
- **Page structure** (`index.html`): `#nav` (masthead, the whole thing is a drag region), then `#banner`, then `#view` (scrolls; `padding: 0 48px 40px` via `--page-gutter: 48px`). Most views wrap their content in `.content-narrow` (`max-width: 1240px; margin: 0 auto`).
- **Fonts and colours** (tokens.css): serif is "Noto Serif SC Subset"/Songti; mono is "IBM Plex Mono Subset"/SF Mono; UI text is -apple-system/PingFang SC. Colours: paper `#faf8f3`, ink `#26241f`, ink-soft `#6d675a`, ink-faint `#8a8578`, ink-ghost `#c4bcaa`, rust `#a4432c`. Base body text is 13.5px.

### Masthead (router.ts)
One flex row, `padding: 14px 0 12px 78px` (78px = `--mast-lights`, reserved for the traffic lights):
1. **TIDY** wordmark: serif 700 20px, `letter-spacing: 6px`, `padding-right: 22px`.
2. Tabs **清理 · 软件 · 优化 · 分析 · 状态**: 13.5px, gap 26px. The active tab is rust, weight 600, with a 2px rust underline. 设置 (settings) has no tab; it is opened from the tray menu.
3. Right-aligned meta, `margin-left: auto`, mono 11.5px, faint, **`white-space: nowrap`**. By default it shows the date and time, e.g. `周二 · 11月3日 · 20:56`, refreshed every 20s. Some views replace it (listed per view below).
4. Below the row: a 2.5px heavy rule, a 2px gap, then a 1px hairline (`.mast-rules`).

### Banner slot (banner.ts)
A full-width strip under the masthead with: text, a rule, action buttons, and an optional ✕.
- Telemetry notice: "Tidy 会发送匿名使用统计,帮助判断哪些功能真的有人用。不含任何路径、文件名或体积。" with actions 看看采了什么 / 关掉 / **知道了** (primary). It has no ✕.
- Update notice: "Tidy {version} 已发布" with 稍后 / **更新并重启**, and a ✕. The mandatory version reads "必须更新到 Tidy {version} 才能继续安全使用" and has no 稍后 and no ✕. While downloading, the button shows 下载中… and then a percentage; on failure it shows 重试.

There is **no global footer**.

---

## 1. Shared front page (frontpage.ts), used by 清理, 优化 and 分析

**Skeleton:**
- `.front-head`: kicker on the left (mono 12px, `letter-spacing: 3px`, rust) and strapline on the right (mono 11px, `letter-spacing: 2px`, faint). `padding-top: 16px`.
- `.front-body`: flex, gap 48px, `padding: 38px 0 64px`, `overflow: hidden`, with two columns:
  - **Lead column** (`flex: 1.4`, gap 22px):
    - headline: **serif 600 70px/1.18**, `letter-spacing: 1px`, with a forced `<br>`. One word is wrapped in `<em>` and set in rust.
    - byline: a 44px × 2.5px rule, the desk name (serif 600 13px), and the date (mono 11px).
    - optional extra row (分析 only).
    - action: a double-framed button **`.frame-cta.double`** (24px, `padding: 16px 54px`, inner 1px frame inset 3px), text "{verb} →". Next to it is a mono 11.5px note, `max-width: 200px`, `white-space: pre-line`.
  - **编者按 rail** `.front-note`: **fixed `width: 340px`, `flex-shrink: 0`**, 1px left rule, `padding-left: 40px`. Contents:
    - title "编者按" (serif 16px, underlined by a rule).
    - standfirst paragraph (13.5px, line-height 2, justified). Its first character is a **54px rust drop cap** floated left.
    - a 3-row ledger under a double rule. Each row is label … dotted leader … value. The last value is rust, weight 600.
- **Ghost watermark** `.front-watermark`: **position absolute, `left: 420px; top: -70px`, serif 600 300px**, transparent fill with a 1px `#e2dccb` text stroke, clipped by the body's overflow. The `.wide` variant (分析) is `left: 300px; top: -52px; 220px`.

---

## 2. 清理 (views/clean.ts + flow.ts + scancard.ts)

### 2a. Front page (before scan)
- Kicker: **第 {n} 期 · 智能清理专栏**. n = number of past clean runs + 1, e.g. "第 47 期 · 智能清理专栏".
- Strapline: **只清可再生文件 · 全程可预览**
- Watermark: the same n, e.g. "47", at 300px.
- Headline: **今天的磁盘，/ 值得一次彻底的整理。** ("彻底" in rust)
- Byline: **本刊编辑部 · 磁盘管理组** · `周二 · 11月3日`
- Button: **开始扫描 →**; note: **扫描不改动任何文件**
- 编者按 standfirst: "扫描的对象只有四类：系统缓存、过期日志、下载残留与废纸篓。它们都能由系统重新生成——你的文稿、照片与应用数据，从不在清理之列。"
- Ledger rows. Values show "—" when there is no history.
  - 上次清理 … `14 天前` (or 今天 / 昨天)
  - 当期回收 … `8.42GB`
  - 累计已省 … `312.50GB` (in rust)

### 2b. Scanning card (scancard.ts)
- `.scan-stage` is a centred grid with `min-height: calc(100vh - 220px)`.
- The card itself: **`max-width: 610px`**, padding 28px, background `#fffdf9`, radius 12px, large soft shadow. It is the only shadowed element in the app.
- Contents, top to bottom:
  1. Heading row, centred, gap 18px: a 66×66 icon (broom, 30px) inside a spinning ring. Beside it, a small rust 12px label **正在分析中** and an h2 (serif 600 20px, **nowrap**) **正在分析可清理内容…**
  2. Path card: a folder icon; a meta line (11px) **正在扫描** with the elapsed time on the right; then a path line, 16px fixed height, **nowrap**. The head of the path ellipsizes and the last 24 characters always stay visible, e.g. `~/Library/Caches/com.apple.Safari/WebKitCache`. The initial text is "正在准备扫描目录…". In the clean flow, the label can also read like `App 缓存 (132)` or `my-app/node_modules`.
  3. Activity bar, 4px tall. While indeterminate, a 34% segment sweeps across it.
  4. Three tiles in `grid repeat(3,1fr)`, each min-height 55px, radius 9px, wash background. Labels are 10.5px (nowrap), values are mono 600 14px:
     - **已检查文件** `128,403`
     - **已扫描数据** `42.18GB`
     - **已用时间** `12 秒` or `3 分 05 秒`
  5. Hint (11px, centred): "若 macOS 弹出授权请求，请选择“允许”或“不允许”；扫描会在你回应后继续"
  6. **取消** button: 100px wide, 36px tall.

### 2c. Ledger preview, the "清理账目" spread (flow.ts `renderPreview`)
Inside `.content-narrow`, `.cols` (gap 40px, `padding-top: 36px`), three columns:

1. **Left, fixed `flex: 0 0 330px`**:
   - kicker **本期可回收**
   - giant figure **serif 600 104px/0.95, nowrap**, with a 28px unit, e.g. `23.4` `GB`. It steps down to 76px when the numeral is 5 or more characters, and to 58px at 7 or more.
   - lede (14px, max 440px): "以下各类均为可安全移除的文件。清理不触碰文稿与应用数据。"
   - optional notes:
     - blocked-apps line, e.g. "关闭 Chrome、Slack 后可再清理 1.20GB · 34 项"
     - warning badges for projects with git blockers, e.g. `proj/app: uncommitted changes`
   - main action **清理 →**, a rust underlined serif link
   - quiet links **先预览全部明细** and **取消**
2. **Middle, flexible, with a 1px inked left rule and `padding-left: 40px`**:
   - Header: **清理账目**, with **全选 / 清空** on the right.
   - One section row per category: checkbox (can be indeterminate), section name (600 13.5px, nowrap), then a mono 11.5px muted count " 已选 12/14". Under it, a description (11.5px, nowrap/ellipsis). On the right, the size in mono 13px nowrap: `3.21GB` when everything is selected, otherwise `1.10GB / 3.21GB`. A 14px chevron toggles the detail.
   - Realistic sections, in typical order:

     | Section | Description |
     |---|---|
     | App 缓存 | 应用临时文件,下次启动会重新生成 |
     | 日志 | 诊断日志,应用会按需重建 |
     | 开发工具 | Xcode / npm 等开发缓存,首次构建会稍慢 |
     | AI 工具 | — |
     | 浏览器 | 浏览器缓存,登录状态和历史记录会保留 |
     | 设计工具 | — |
     | 通讯工具 | — |
     | 系统(需管理员) | — |
     | 临时文件 | — |
     | 包管理器缓存 | — |
     | Xcode 设备支持文件 | — |
     | 安装包 | — |
     | 项目依赖与构建产物 | — |
     | Docker | — |
     | iOS 模拟器 | — |
     | 废纸篓 | — |
     | 已卸载应用的残留 | — |
     | 快照 · Homebrew | — |

     Descriptions not quoted here are in i18n.ts lines 33–87.
   - Expanded item rows contain:
     - a checkbox
     - the file name (600, nowrap)
     - a mono 11px path (`max-width: 420px`, nowrap), e.g. `~/Library/Caches/com.spotify.client`
     - an optional "214 项" count
     - the size, e.g. `1.02GB`
     - an optional badge. Examples: `3 天内改过` (warn) / `92 天未动` (ok); `2 个容器使用中` / `悬空镜像` / `120 天未使用`; `iOS 17.2 · 45 天未使用`; `Mike's iPhone · iPhone15,2 · 30 天前备份`; `com.foo.bar · 60 天未动`; `Time Machine 本地快照 · …` or `brew cleanup --prune=all`; `需管理员`
     - a ↗ reveal-in-Finder button.
   - Total row: **小计 · 已选 142 项** … `18.73GB` (16px).
3. **Right sidebar `.col-side`, fixed `width: 250px`**, hairline left border, `padding-left: 36px`:
   - sec-label **上次清理**, figure.sm (24px) `14 天前`
   - **累计已省** `312.50GB`
   - rules note, 12px faint: "清理规则：只移除可再生文件；受保护项目需要单独确认；执行前的每一项都可以取消勾选。" In trash mode it reads "…本次全部送入废纸篓，30 天内可以恢复。"

- Empty result: a centred hero with kicker **准备开始清理** and a 64px serif line **没有可处理的项目 ✨**.

### 2d. Confirm sheet (modal)
- Backdrop is 36% ink. The sheet is 460px wide, paper background, 1px inked border.
- h2 (22px): **确认处理 142 项?**
- Lede: **约 18.73GB · 删除瞬间会再次逐项校验保护策略**
- Optional rust note (12.5px), e.g. "包含 3 个项目的依赖/构建产物(约 4.2GB)。其中 1 个最近 14 天内有改动。删除后需重新执行 npm install / cargo build 等 包含 5 项 Docker 镜像/缓存,删除后需重新 pull 或 build。"
- Buttons: **取消** and **清理** (danger).

### 2e. Executing
This uses the same scan card with these differences:
- no cancel button and no sweep (the bar is determinate)
- stage label **处理中**; title **正在清理所选项目…**; path label **正在处理**
- tiles: **已处理** `37 / 142`, **已释放** `6.10GB`, **已用时间**
- hint: "逐项删除，每一项删除前都会再校验一次；需要管理员权限的项目可能弹出系统授权"

### 2f. Done page
A centred `.hero`, `padding: 88px 0 82px`:
- kicker **完成**
- figure 104px, e.g. `18.7` `GB`
- sub **已释放**
- mono statline **跳过 · 2　失败 · 0**
- `.frame-cta` **返回 →**
- In trash mode, an extra line: "其中 18.7GB 只是移入了废纸篓,清空后才会真正释放。" with a link **清空废纸篓 →**. That link runs the confirm sheet; if the trash is already empty it shows "废纸篓已经是空的。"
- If some items were not removed: header **未处理 2 项** and a 3-column table with these widths:
  - path (12.5px, nowrap/ellipsis)
  - size (80px)
  - reason (42%), e.g. "文件正在被使用", "在白名单里，已跳过", "没有删除权限（文件属于其他用户或被系统保护）"
- Failure variant: kicker **失败**, the error message, and **返回 →**.

---

## 3. 软件 (views/uninstall.ts)

The view is inside `.content-narrow`.
- **Sub-tab row** `.flow-toolbar` (gap 28px): **卸载 · 更新 · 启动项**, set as serif 600 22px words. The inactive ones are ghost grey; the active one is rust with a 2.5px underline.
- On the right, mono 11.5px extra links: in the 卸载 tab, **↻ 刷新** (shows 刷新中… while working); in the 更新 tab, **↻ 重新检查**.
- `#tab-body` has `padding-top: 22px`.

### 3a. 卸载 (three columns)
- Masthead meta: `142 个应用 · 86.40GB`
- Loading state: **正在读取应用清单…**

1. **名录, fixed `flex: 0 0 400px`**:
   - Header **名录**, with **名称 ⇅  大小 ⇅** on the right. The default sort is by size.
   - Full-width search box with placeholder **搜索…**.
   - App rows (dotted bottom border, padding 9px 0). Each row has:
     - a checkbox
     - a 16px icon (the fallback is a letter)
     - the **name** (600 13.5px, **nowrap**)
     - meta (11px, nowrap/ellipsis), e.g. `124.0.6367 · 活跃中` or `14.2 · 系统组件` or `· 需官方卸载器 (Adobe Uninstaller)`
     - a dotted leader
     - a rust mono summary `5 个已选`
     - the size in mono 13px, e.g. `1.82GB`
   - The focused or selected row has a rust title.
   - Empty search: **没有匹配的应用**.
2. **Dossier, flexible, inked left rule**:
   - **App name in serif 600 44px** (`overflow-wrap: anywhere`), e.g. "Docker".
   - Mono 12px line: `版本 4.30.0 · /Applications/Docker.app`
   - Optional warning badges (notes).
   - Category ledger, e.g.:
     - 应用 … 2.10GB
     - App Support · 3 个文件 … 12.4GB
     - Cache … 340MB
     - Logs … 12MB
     - Preferences · 2 个文件 … 48KB
     - 启动项 … 4KB
     - 合计 … 14.86GB

     Other categories: HTTP Storage, WebKit, 窗口状态, Containers, 其他.
   - Action **完全卸载 →** (16px link) and the muted 12px note "卸载包含以上全部残留；展开下方明细可以逐项取消。"
   - Box: head **逐项明细 · 9 个文件**, then the mono count `已选 9/9 项 · 14.86GB`, then a **全选/取消全选** link, then a ▾ toggle. Expanded rows: checkbox | category (nowrap) | mono 11px full path (nowrap/ellipsis) | size.
   - While loading: the name plus **正在统计残留…**
3. **Sidebar 250px**:
   - **建议关注**
   - "占用最多空间的 3 个应用，合计 38.2GB"
   - three linked rows, e.g. "Xcode … 12.10GB"
   - 12px note: "Tidy 无法读取应用的最近使用时间，因此这里只按体积排序，不做“很久没用”的推断。"

- **Sticky footer bar**, shown when something is selected:
  - **已选 2 项** (serif 14px)
  - mono `15.20GB`
  - link **取消选择**
  - a dotted leader
  - rust serif 16px **移除 2 项 →**
- Confirm-sheet verb: **移除**. The run then goes through the shared exec card and done page (§2e/2f).

### 3b. 更新
- Masthead meta: `检查时间 20:56:12`, or `↻ 正在后台检查更新…` while checking.
- Loading state: **正在检查软件更新…** with a **取消** button. While updating: **正在更新…** with **取消**.
- Results appear as a panel of rows (outcome + message) at the top.
- Two columns:
  - **Main (flexible)**:
    - Header: **可在 {app} 内更新** (where {app} = Tidy), then `5 个`, then **全部更新 →**.
    - Rows contain:
      - a 14px icon
      - the name, plus a source badge (Homebrew / App Store / Sparkle / Electron / 官网), plus a ▾ when release notes exist
      - the version line `1.4.2 → 1.5.0`
      - a link **忽略更新**
      - a button **更新 →**
    - Sub-header **Homebrew 软件包** `3 个`. These rows show a `<code>` command such as `brew upgrade wget` instead of a button, followed by the hint "Homebrew 显示精确终端命令，其他来源交回原更新器。"
    - Sub-header **已隐藏更新**; its rows use **恢复提醒**.
    - A collapsible **已是最新的软件 (48)** list of name + `version · source`.
    - Empty states: **所有可检查的软件均为最新** / **没有确认可用的更新，请查看来源诊断**
  - **Sidebar, `.col-side.wide`, 280px**:
    - **来源诊断** ledger with header 来源 … 个数 and rows like `Homebrew … 21`, `Sparkle … 14`, `App Store … 9`
    - collapsible rust **来源诊断 · 2 ▸** with the warning lines
    - "已忽略 1 个更新"

### 3c. 启动项
- Masthead meta: **登录项与辅助程序 · 23 个**
- **Main column:**
  - Head: **后台服务**, with "开 = 随系统启动" on the right.
  - Rows contain:
    - ▸
    - the label, e.g. `com.docker.vmnetd`
    - a sub line `LaunchAgent · /Library/…/helper`, plus a danger badge **目标不存在** when the target is missing
    - ↗
    - a toggle (34×18). System-scope rows have the toggle disabled.
  - Head: **登录项与辅助程序**, then muted **仅供确认**, then `12 个`.
  - Embedded rows contain:
    - an icon
    - the app name
    - `ItemName · 登录项|辅助程序 · 内嵌在 App 中,仅供确认`
    - ↗
    - **只读**
  - Empty state: **没有启动项**.
- **Sidebar 250px:**
  - **随系统启动**, figure `8 / 23`
  - lede (12.5px) "关闭后该项不再随系统启动；内嵌在 App 中的辅助程序由其宿主管理，这里仅列出供确认。"
  - **系统级项需特权助手**

---

## 4. 优化 (views/optimize.ts)

### 4a. Front page
- Kicker **例行专栏 · 系统维护**
- Strapline **{n} 项例行 · 全部走系统公开接口**. The catalog has **22 tasks**; 4 need admin (Re-index Spotlight, Purge inactive memory, Repair disk permissions, Run periodic maintenance) and are hidden when the helper is unavailable. So n is 18 or 22.
- Watermark: a CJK numeral if n ≤ 10, otherwise digits (e.g. "22").
- Headline **机器不需要奇迹，/ 只需要按时的照料。** ("按时" in rust)
- Byline **本刊编辑部 · 系统维护组** · `上次维护 · 昨天 22:14`, or **尚未运行过维护**
- Button **开始检查 →**. The note is **检查不改动任何设置**, preceded by `约 2 分钟` on its own line when there is history.
- Optional line: "助手未安装,需管理员权限的任务将被跳过"
- Standfirst: "维护只有六项：可清除空间、索引、缓存与维护脚本，每一项都有明确边界。我们不做“深度加速”——那些奇迹，通常以稳定性为代价。"
- Ledger: 上次维护 … `昨天 22:14`; 已完成任务 … `37 次`; 预计用时 … `约 2 分钟` (rust)

### 4b. Task list
`.cols`, `padding-top: 32px`:
- **Main column:**
  - Header **例行维护 · 22 项** (26px), with **执行已选 18 项 →** on the right.
  - **2-column grid** (`1fr 1fr`, gap 28px 40px). Each task has:
    - a large rust number **01** (serif 600 34px). Unchosen tasks show the number in ghost grey with a softer title.
    - the title (600 14.5px)
    - the description (12.5px faint)
    - an action **✓ 已选 · 取消** (rust) / **加入本次**, or the state `✓ 今天已运行`
  - **Task titles and descriptions come from the Rust backend in English only**: e.g. "Rebuild QuickLook cache / Fixes broken file previews in Finder.", "Flush DNS cache / Refreshes stale DNS answers after a network change.", "Rebuild font cache", "Rebuild Launch Services database", "Compact Mail and Messages databases", "Prune Notification Center database". The catalog is in `/Users/mike/source/project/openfabrica/tidy/desktop/crates/mole-ops/src/optimize/catalog.rs`.
- **Sidebar 250px:**
  - **维护日志**, with up to 6 rows like `11/2  Flush DNS cache … ✓` (failed runs show the outcome text instead of ✓), or "—" when empty
  - note "所有任务使用系统公开接口，有明确边界，不做“深度加速”类危险操作。"
  - **取消** link

### 4c. Running
The shared scan card with:
- gauge icon
- stage **正在优化**; title **正在执行维护任务…**; path label **当前任务**
- path line `Flush DNS cache · 3/18`
- tiles **已完成** `3 / 18`, **成功** `3`, 已用时间
- hint "任务会逐项执行；需要管理员权限的任务可能弹出系统授权"
- no cancel button

### 4d. Results
A centred hero:
- kicker **优化完成**
- a 64px serif line **16 成功 · 1 跳过 · 1 失败**
- sub **本期维护到此结束。**
- **返回 →**

---

## 5. 分析 (views/analyze.ts)

### 5a. Front page
- Kicker **空间调查 · 磁盘占用**; strapline **逐层统计 · 按大小排出账目**
- Watermark (wide variant, 220px): the rounded size of the selected scope, e.g. "994". It is empty if the size is unknown.
- Headline **每一 GB，/ 都该有去向可查。** ("去向" in rust)
- Byline **本刊编辑部 · 空间调查组** · `上次扫描 · 今天 09:12`, or **尚未扫描过**
- **Scope row** (13.5px, gap 22px, underline tabs): **整个磁盘 · 994.66GB** | **用户目录 ~ · 412.30GB** | **选择文件夹…** (once a folder is picked, this shows its path, e.g. `~/Projects`)
- Button **开始分析 →**. The note is either **首次扫描会遍历整层目录**, or the clickable underlined pair "上次结果仍可查看 / 缓存于 09:12".
- Standfirst: "占用不是罪，看不见才是。逐层统计每个目录的真实大小，把磁盘排成一本可以核对的账，点进任何一层继续深入。"
- Ledger: 整盘容量 … `994.66GB`; 已用 … `712.04GB`; 可用 … `282.62GB` (rust). Values show "…" while loading.

### 5b. Scanning
The scan card with a pie icon, title **正在统计空间占用…**, the default stage and labels, and **取消**.

### 5c. Results (ledger view, the default)
- Masthead meta: `缓存于 09:12 图形视图 · 重新扫描` (both are rust links). In treemap mode the toggle reads **账目视图**.
- `.ana-layout` (flex, gap 40px, `padding-top: 30px`), two columns:
  1. **Main, flexible:**
     - Breadcrumb headline, **serif 600 26px, `white-space: nowrap`**, e.g. `整个磁盘 › Users › mike`. Deep paths collapse to `整个磁盘 › … › Library › Caches`; the last crumb is rust.
     - Right of the breadcrumb, mono 12px **nowrap**: `38 项 · 412.30GB`, plus a warn badge **扫描已取消,结果不完整** if the scan was cut short.
     - A rule, then 6 ranked rows (gap 14px). Each row is name (14px, nowrap/ellipsis) … `186.20GB · 45%` (mono 13px nowrap), with a solid **12px bar** below. Bar colours by rank: `#a4432c`, `#c08468`, `#d9c1ac`, `#d9c1ac`, `#e8ddce`, `#efe9dc`. The first row is rust and bold. A last row reads **其余 32 项** … `22.10GB · 5%`. Realistic children: Library, Projects, Movies, Downloads, Documents, .docker.
     - Hint (12px muted): **点击目录名深入 · 面包屑返回上级**
  2. **Rail, fixed `flex: 0 0 330px`**, inked left rule:
     - Title (serif 22px): `Library` with a small **内**, or **本层明细**.
     - Ledger of the lead child's top 4 entries, e.g. `Containers … 88.1GB`, plus `其余 n 项`. If that child has not been scanned yet: **尚未扫描 · 进入后统计**.
     - Link **进入 Library →**, then a disabled quiet link **移到废纸篓**.
     - sec-label **勾选后可移到废纸篓**, then 10 rows of checkbox | name (12.5px) … size.
- **Disk strip** footer, below both columns: mono 11.5px: **磁盘** [a 10px bar: rust "used" part plus a dashed "free" part] `已用 712.04GB · 可用 282.62GB`.
- **Treemap mode:** replaces the bar list with a box **460px tall**, 1px border, holding absolutely positioned blocks. There are up to 9 entries plus a "其余 n 项" block. Colours: `#a4432c`, `#b95f43`, `#c08468`, `#c9a25e`, `#a3865a`, `#8f7a92`, `#7c6f83`, `#6e6258`, `#7f8a6a`, `#6a7a8a`. Blocks have paper-coloured text: a 13px bold name and an 11px mono size, shown only when the block is wider than 70px and taller than 40px.
- **Trash flow:** reuses the clean flow (§2c–2f) with title and verb **移到废纸篓**.

---

## 6. 状态 (views/dashboard.ts)

Refreshes every 2s.
- Masthead meta shows the hardware: `M3 Pro · 36 GB · macOS 15.1`.
- First open shows a skeleton: 6 grey bar columns.

Everything is inside `.content-narrow`, top to bottom:

1. **Metric strip**: 6 equal `flex: 1` columns, `padding: 28px 0 22px`. Every column after the first has a hairline left border and `padding-left: 32px`. Each column has:
   - a figure in **serif 600 58px, nowrap**, with a 20px unit
   - a mono 11.5px label (nowrap/ellipsis)
   - a chart that is **fixed at 120px wide**

   The columns:

   | Column | Figure | Label | Chart | Notes |
   |---|---|---|---|---|
   | CPU | `23%` | `CPU 52°C · 12 核` | per-core bars, 22px tall, 2px gap | turns rust above 80% |
   | Memory | `68%` | `内存 · 压力 12%` (or `内存 · 36.00GB`) | 8px meter | turns rust if pressure > 50% or usage > 90% |
   | GPU | `14%` | `GPU 48°C · 18 GPU 核 · 低负载` | sparkline | label alternatives: 中负载 / 高负载 |
   | Disk | `282` `GB` (free) | `磁盘 · 已用 72%` | meter | — |
   | Network | `1.2` `MB/s` | `网络 · en0 · ↑48KB` | dual line (ink area = down, rust line = up) | — |
   | Battery, `flex: 0.8` | `87%` | `电池 ⚡ · 312 次循环`; second label `风扇 2400 RPM ×2` or `此机型无风扇 · 被动散热` | — | without a battery: `2400 RPM`, label `风扇 · 由 macOS 调节` |

2. **Fan-control panel**, hidden on fanless Macs:
   - sec-label **风扇控制**
   - Idle: `自动（macOS 调节）`, button **开启手动控制**, and 11px "需要一次管理员密码。退出 Tidy 时风扇自动交还给 macOS。"
   - Active: toggle, `手动`, a range slider (min-width 160px), `3200 RPM` (min-width 72px), `范围 1200–6500`, and button **结束控制 · 交还 macOS**
   - Error: "风扇控制失败: …"
3. **Double rule**.
4. **Three columns** (`padding-top: 20px`):
   - **进程 · 按 CPU**: 5 ledger rows, e.g. `WindowServer … 18.4`. Rows above 100% are flagged rust.
   - **进程 · 按内存**: inked left rule; e.g. `Google Chrome Helper … 1.82GB`.
   - **健康度** (250px): figure **44px** `92` with a small ` / 良好` (or 一般). Then a rust note **各项指标正常**, or e.g. `CPU · 内存 有指标偏高`. Then mono 11.5px `已运行 4d 12h · 自 10/30`.
5. **Process table** (rule above), heading **进程全表 · 60 项**:
   - Columns: **名称 (60) ⇅ | PID | CPU | 能耗 | 内存**. Headers are 12.5px; sortable headers show a chevron when active and ⇅ otherwise.
   - Row: a 5px dot | a 14px icon or ⚙︎ | name (plus 🔥 when CPU > 100%) | `4512` | a bar up to 60px wide + `34.2` | `2.4h` | `512.0MB`
6. **Footer line** (centred mono 11px): `macOS 15.1 (24B83) · Mikes-MacBook-Pro`.

**Process modal** (opened by clicking a row; 620px wide):
- a 34px icon, the h2 name (26px), and ✕
- mono line `PID 4512 · CPU 34.2% · MEM 512.0MB · mike · 来自 OrbStack`
- the parent chain `launchd 1 › OrbStack 880 › OrbStack Helper 4512`
- key/value rows (labels 110px wide): 线程数, 打开文件, 磁盘 I/O (`1.2GB R · 340MB W`), 监听端口 (with the badge "进程正在监听公开网络接口。"), 子进程, 用户, 运行时长, 工作目录, 可执行文件, 命令 (mono block)
- buttons **复制摘要 / 显示 / 终止 / 强制退出** (the last is danger)

---

## 7. 设置 (views/settings.ts; reached from the tray, no masthead tab)

- An inline sheet, **560px wide**, centred, `margin: 30px auto`.
- Header: h1 **设置** (22px) and ✕.
- Underline tabs: **通用 · 工具 · 菜单栏**.
- Rows are a title plus muted description on the left and a control on the right:
  - **通用**:
    - 完全磁盘访问权限 / 深度扫描会覆盖 App Support 和容器 → badge **已授权** or button **去授权**
    - 界面语言 / 跟随系统语言,当前为 中文 → segmented 自动 | 中文 | EN
    - 温度单位 → °C | °F
    - 开机自动启动 → toggle
    - 自动检查 Tidy 更新 → **立即检查** + toggle
    - 匿名使用统计 → toggle
  - **工具**:
    - 缓存删除 → 永久 | 废纸篓
    - 保护项目 (textarea + 保存)
    - 项目扫描目录
    - 项目依赖默认勾选阈值 `[90] 天`
    - Docker 镜像默认勾选阈值 `[6] 个月`
    - Touch ID 用于 sudo → badge 已启用 / 未启用
  - **菜单栏**:
    - 在菜单栏显示 (+ 系统状态 / 配置… links)
    - 登录时自动启动
- Footer: mono 11px `Tidy v1.2.0 · 3fa9c1e0b7d2… · 系统授权:可用`.

---

## 8. Things that will break on narrow windows (min width 800px)

Usable width at the minimum is 800 − 96 (gutters) = **704px**.

- **Front page:**
  - 70px headline
  - the fixed **340px** 编者按 rail plus a 48px gap leaves the lead column about 316px
  - the 300px absolutely positioned watermark at **`left: 420px`** (300px at `left: 300px` for 分析)
  - the 24px double CTA with 54px side padding
  - the nowrap ledger labels
- **Clean preview:**
  - **330px** left column, 250px sidebar, and 40px gaps plus 40/36px column paddings. That is roughly 740px of fixed width before the middle ledger gets any space.
  - The 104px nowrap figure.
  - Nowrap section names and item paths (`max-width: 420px`).
- **Apps 卸载:**
  - a **400px** list, a 250px sidebar, and a flexible dossier with a **44px** app name
  - nowrap app titles and meta
- **Updates:** 280px sidebar.
- **Analyze:**
  - a **330px** rail
  - a nowrap 26px breadcrumb
  - a nowrap item-count line
  - a 460px-tall treemap
- **Status:** six 58px nowrap figures, each with a 120px chart and a 32px left padding, which needs about 960px or more. The metric labels ellipsize.
- **Optimize grid:** 2 columns with a 40px gap, plus the 250px sidebar.
- **Masthead:** the meta line is nowrap and can be long, e.g. the 分析 links, or `检查时间 …`.
- **Scan card:** `max-width: 610px`; the h2 is nowrap.
- **Media queries:** the only ones in base.css are `prefers-reduced-motion`. **There are no responsive breakpoints.**