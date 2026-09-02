# Tidy 产品站点

零依赖的静态站点：所有页面都是纯 HTML + 一份 CSS，没有前端框架，没有运行时 JavaScript 依赖，
`site/` 目录本身就是可部署的根目录，丢给任意静态托管即可。

Node 只在**发布博客文章**时用到，用来把 Markdown 编译成文章页、RSS 与 sitemap。
浏览站点、修改首页或法务页都不需要装任何东西。

## 目录

```
site/
  index.html              语言选择 / 跳转（meta refresh + JS 双保险）
  zh/index.html           中文首页（手写）
  en/index.html           英文首页（手写）
  zh|en/privacy/          隐私说明（手写）
  zh|en/terms/            使用条款（手写）
  zh|en/blog/**           文章页与列表页（由 tools 生成，不要手改）
  assets/site.css         全站样式，token 与 desktop/ui/src/styles/tokens.css 对齐
  assets/tidy-logo.png    从 desktop/ui/public/ 复制
  assets/media/           首页产品截图（webp 1x/2x + png 兜底）
  content/blog/<locale>/  文章源文件（唯一真相）
  docs/                   frontmatter schema 与站点规格
  tools/                  内容管线
  blog.config.json        路由、语言、分类、阅读时长等配置
  robots.txt sitemap.xml  生成物 / 静态文件
```

## 域名

正式域名是 `https://tidy.talkwork.vip`（域名托管在阿里云 DNS）。

如果要再改域名，替换这三处后重新生成：

1. `blog.config.json` 的 `siteUrl`
2. `robots.txt` 里的 Sitemap 地址
3. 手写页面（`index.html`、`zh|en/index.html`、`zh|en/privacy/`、`zh|en/terms/`）里的
   `canonical`、`hreflang`、`og:url`、`og:image` 与结构化数据 URL

```bash
cd site && node tools/build-blog.mjs && node tools/render-site.mjs
```

（内容管线的命令要在 `site/` 目录下跑，不是 `site/tools/`——schema 是按 `<cwd>/docs/` 找的。）

## 写一篇文章

```bash
cd tools && npm install && cd ..

# 新建（分类只能取 blog.config.json 里的 space / apps / maintenance / safety）
node tools/new-post.mjs --title "标题" --permalink stable-slug --category space

# 写正文，写完把 frontmatter 里的 draft 改成 false

node tools/validate-content.mjs      # 内容门禁，非零退出即有问题
node tools/build-blog.mjs            # Markdown → 结构化产物
node tools/render-site.mjs           # 产物 → zh|en/blog/** 静态页 + RSS + sitemap
node tools/validate-content.mjs --write-lock   # 发布后刷新已公开路由快照
```

翻译一篇已有文章（自动继承 permalink 与 translationKey，保证语言切换不跳 404）：

```bash
node tools/new-post.mjs --translate stable-slug --locale en
```

新增文章**不需要改动任何 HTML 或路由代码**。如果你发现必须去改 `render-site.mjs` 才能发文章，
说明内容边界破了，先修边界。

## 站点配图（首页截图）

首页的三张产品界面是**真实截图**，取自 0.1.0 本地构建，源文件在 `assets/media/`：

| 文件前缀 | 界面 | 位置 |
| --- | --- | --- |
| `clean-scan` | 清理页起始态 | hero，eager + `fetchpriority="high"` |
| `apps-uninstall` | 软件 → 卸载列表 | 产品证据区，lazy |
| `apps-updates` | 软件 → 更新列表 | 「看清哪些软件该更新」工作流，lazy |

每个前缀有三个文件：`@2x.webp`（2160w）、`@1x.webp`（1080w）、`@1x.png`（1080w 兜底），
通过 `<picture>` 交付，`<img>` 上声明 `width="2160" height="1440"` 预留 3:2 版面防止抖动。

重拍截图时按同一套规范：

- 窗口 2160×1440（@2x，对应 tauri.conf.json 的 1080×720 默认窗口），深色主题，保留窗口栏；
- 画面里不得出现真实姓名、邮箱、token 或带用户名的路径；
- UI 改了就重拍——**过期截图算事实错误，不是美观问题**；
- 生成三个衍生文件：

```bash
convert 原图.png -resize 2160x -quality 86 -define webp:method=6 assets/media/<name>@2x.webp
convert 原图.png -resize 1080x -quality 86 -define webp:method=6 assets/media/<name>@1x.webp
convert 原图.png -resize 1080x -strip                            assets/media/<name>@1x.png
```

`alt` 写图里有什么，`figcaption` 写这张图证明了什么以及它取自哪个构建，两者不重复。
完整规范见 skill 的 `references/product-media.md`。

## 文章配图

图片单独成段时会渲染成 `<figure>`，Markdown 的 title 变成 `<figcaption>`：

```markdown
![清理页在执行前列出全部候选](/blog/clean-preview/preview@2x.png "预览列出每一项候选，被挡下的行会写明原因。")
```

- `alt` 写图里有什么（给看不见的人），`figcaption` 写这张图要说明什么以及它是怎么来的，两者不要重复。
- figure 里的图片 **必须有 alt**，空的会被门禁拦下。
- 不是真实截图的（代码原生示意、示意图）必须在 caption 里写清楚是什么、依据什么画的。
- 配图放 `public/blog/<permalink>/`，文件不存在会被门禁拦下。
- 截图里不要出现真实姓名、邮箱、token 或带你用户名的路径。

完整规范见 skill 的 `references/product-media.md`。

## 内容门禁会拦住什么

- frontmatter 不符合 `docs/blog-frontmatter.schema.json`
- 目录语言与 `lang` 不一致；分类、语言、CTA 不在白名单
- 同语言 permalink 重复；同一 `translationKey` 下 permalink 漂移
- 正文出现 `h1`（页面级 h1 由 `title` 渲染）
- 受控 HTML 被剥离的标签、事件属性、`javascript:` 协议、非白名单 iframe host
- 指向不存在文章的站内链接、不存在的图片、figure 图片缺少 alt
- `cover` 缺 `coverAlt`、`updatedAt` 早于 `publishedAt`
- 已公开的 permalink 消失且 `content/blog/_redirects.json` 里没有 301

改文章 URL 之前先在 `content/blog/_redirects.json` 里补一条 301，否则门禁会直接拦下。
`_redirects.json` 的内容会被原样输出到 `.blog-build/redirects.json`，交给托管方配置。

## 事实边界

站点上的每一条产品说明都对应 `desktop/` 仓库里已实现的行为。特别注意这几条会随发布状态变化：

| 说法 | 依据 | 变化时机 |
| --- | --- | --- |
| 提供签名并公证的安装包 | GitHub Releases v0.1.0，`spctl` 显示 `Notarized Developer ID` | 每次发版更新版本号与体积 |
| 系统范围操作以 `requires_admin` 拒绝 | 特权助手的 SMAppService/XPC 传输未发布 | 传输层发布后更新 |
| 匿名使用统计默认开、可关闭 | `desktop/crates/mole-telemetry`，字段清单见 `event.rs` | 采集面一变，`zh/privacy` 与 `en/privacy` 的字段表必须同步改 |
| 应用会检查自身更新 | `desktop/src-tauri/src/update.rs`，feed 在 GitHub Releases | 换分发渠道时更新隐私页的联网说明 |
| 主 CTA 是下载 | 固定地址 `releases/latest/download/Tidy.dmg`（publish.sh 每次发版都会传一份同名副本） | 换分发渠道时更新 |

站点刻意没有的东西：用户评价（还没有公开评价，不能编造）、具体许可证名称（以仓库
LICENSE 为准）、任何下载量或评分数字。

## 部署

`site/` 就是站点根目录，直接指向它即可。托管方需要支持目录索引
（`/zh/blog/foo/` → `/zh/blog/foo/index.html`），GitHub Pages、Netlify、Vercel、
Cloudflare Pages、nginx 默认配置都满足。

上线后至少确认：首页在中英文下都能打开、`/zh/blog/` 到文章页可读且**没有评论区**、
语言切换在文章页跳到同一篇的另一语言、`/sitemap.xml` 与 `/zh/blog/rss.xml` 可访问、
控制台无报错。

## 与应用的关系

`assets/site.css` 顶部的 token 是 `desktop/ui/src/styles/tokens.css` 的镜像。
应用里改了品牌色、圆角或动效时长，这边要在同一次提交里跟上，否则站点和应用会慢慢分叉。
品牌使用规则见 `desktop/docs/BRAND_GUIDELINES.md`。
