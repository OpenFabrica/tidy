#!/usr/bin/env node
// Render the pipeline's artifacts into the static pages this site actually serves.
//
// Division of labour: build-blog.mjs turns Markdown into sanitized HTML plus metadata
// (framework-agnostic data), and this file is the only place that knows what a Tidy page
// looks like. Homepage and legal pages are hand-written HTML and are not touched here;
// this script owns <locale>/blog/**, the RSS feeds, and sitemap.xml.
import fs from 'node:fs'
import path from 'node:path'
import { loadConfig } from './lib/config.mjs'

const config = loadConfig()
const root = config.__cwd
const build = path.resolve(root, config.outDir)

if (!fs.existsSync(path.join(build, 'index.json'))) {
  console.error('✗ no build artifacts found. Run: node tools/build-blog.mjs')
  process.exit(1)
}

const index = JSON.parse(fs.readFileSync(path.join(build, 'index.json'), 'utf8'))

// Static routes that are hand-written HTML. They still belong in the sitemap, so they are
// listed here rather than discovered — a missing entry is a visible omission, not silence.
const STATIC_ROUTES = [
  { path: '/{locale}/', changefreq: 'weekly', priority: '1.0' },
  { path: '/{locale}/privacy/', changefreq: 'yearly', priority: '0.3' },
  { path: '/{locale}/terms/', changefreq: 'yearly', priority: '0.3' },
]

// Categories are stored as stable slugs so one topic cannot split into two across locales.
const STRINGS = {
  zh: {
    htmlLang: 'zh-Hans',
    ogLocale: 'zh_CN',
    nav: { features: '功能', safety: '安全', blog: 'Blog', cta: '下载', skip: '跳到正文', main: '主导航' },
    blogTitle: 'Tidy Blog',
    blogHead: '关于 macOS 空间、卸载与维护的记录',
    blogLead: '写给想弄清楚「这东西到底能不能删」的人。每篇文章先给出脱离 Tidy 也成立的答案，再说明 Tidy 在哪一步缩短了流程。',
    empty: '这个分类下暂时还没有文章。',
    subscribe: '订阅 RSS',
    readingMinutes: (n) => `约 ${n} 分钟`,
    date: (iso) => { const [y, m, d] = iso.split('-'); return `${y} 年 ${Number(m)} 月 ${Number(d)} 日` },
    updated: '更新于',
    toc: '目录',
    related: '相关阅读',
    seriesPrev: '上一篇',
    seriesNext: '下一篇',
    otherLang: 'English',
    langNote: (t) => `English version: ${t}`,
    cta: {
      source: { h: '想确认这些说法？源码是公开的', p: 'Tidy 的删除边界、保护名单与日志格式全部可以在仓库里直接查证。安装包已签名并通过 Apple 公证，也可以自己编译一份对照。', label: '在 GitHub 上查看源码', href: 'https://github.com/rambocode/tidy' },
      issues: { h: '遇到看不懂的拒绝原因？', p: '贴上拒绝码和 ~/Library/Logs/mole 里对应的几行，比描述现象更容易定位。', label: '在 GitHub 提交问题', href: 'https://github.com/rambocode/tidy/issues' },
    },
    categories: { space: '空间与清理', apps: '软件管理', maintenance: '系统维护', safety: '安全边界' },
    footer: { home: 'Tidy 首页', privacy: '隐私说明', terms: '使用条款', other: 'English' },
  },
  en: {
    htmlLang: 'en',
    ogLocale: 'en_US',
    nav: { features: 'Features', safety: 'Safety', blog: 'Blog', cta: 'Download', skip: 'Skip to content', main: 'Main' },
    blogTitle: 'Tidy Blog',
    blogHead: 'Notes on macOS space, uninstalls, and maintenance',
    blogLead: 'For people trying to work out whether a thing is actually safe to delete. Every article gives a standalone answer first, then says where Tidy shortens the work.',
    empty: 'No articles in this category yet.',
    subscribe: 'Subscribe via RSS',
    readingMinutes: (n) => `${n} min read`,
    date: (iso) => new Date(`${iso}T00:00:00Z`).toLocaleDateString('en-US', { year: 'numeric', month: 'short', day: 'numeric', timeZone: 'UTC' }),
    updated: 'Updated',
    toc: 'Contents',
    related: 'Related',
    seriesPrev: 'Previous',
    seriesNext: 'Next',
    otherLang: '中文',
    langNote: (t) => `中文版：${t}`,
    cta: {
      source: { h: 'Want to check these claims? The source is public', p: "Tidy's deletion boundary, protection lists, and log formats can all be verified in the repository. The installer is signed and notarized, and you can always compile your own copy to compare.", label: 'View the source on GitHub', href: 'https://github.com/rambocode/tidy' },
      issues: { h: 'A refusal reason that makes no sense?', p: 'Paste the refusal code together with the matching lines from ~/Library/Logs/mole; that locates it far faster than a description.', label: 'Open an issue on GitHub', href: 'https://github.com/rambocode/tidy/issues' },
    },
    categories: { space: 'Space & cleaning', apps: 'App management', maintenance: 'Maintenance', safety: 'Safety boundary' },
    footer: { home: 'Tidy home', privacy: 'Privacy', terms: 'Terms', other: '中文' },
  },
}

const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]))
const otherLocale = (locale) => (locale === 'zh' ? 'en' : 'zh')

function head({ locale, title, description, canonical, alternates, extraHead = '', robots = null }) {
  const s = STRINGS[locale]
  const alts = alternates.length
    ? alternates
    : config.locales.map((l) => ({ lang: l, url: null })).filter((a) => a.url)
  const altTags = alts
    .map((a) => `<link rel="alternate" hreflang="${a.lang === 'zh' ? 'zh-Hans' : a.lang}" href="${esc(a.url)}">`)
    .join('\n')
  const xDefault = alts.find((a) => a.lang === config.primaryLocale) || alts[0]
  return `<!doctype html>
<html lang="${s.htmlLang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)}</title>
<meta name="description" content="${esc(description)}">
<link rel="canonical" href="${esc(canonical)}">
${altTags}
${xDefault ? `<link rel="alternate" hreflang="x-default" href="${esc(xDefault.url)}">` : ''}
${robots ? `<meta name="robots" content="${esc(robots)}">` : ''}
<link rel="icon" href="/assets/tidy-logo.png">
<link rel="apple-touch-icon" href="/assets/tidy-app-icon.png">
<link rel="alternate" type="application/rss+xml" title="Tidy Blog" href="/${locale}/blog/rss.xml">
<link rel="stylesheet" href="/assets/site.css">
<meta property="og:type" content="article">
<meta property="og:site_name" content="Tidy">
<meta property="og:locale" content="${s.ogLocale}">
<meta property="og:title" content="${esc(title)}">
<meta property="og:description" content="${esc(description)}">
<meta property="og:url" content="${esc(canonical)}">
<meta property="og:image" content="${esc(new URL('/assets/tidy-app-icon.png', config.siteUrl).toString())}">
<meta name="twitter:card" content="summary">
${extraHead}
</head>`
}

// `localeTargets` lets an article page point the locale switch at its own translation
// instead of the blog index. Untranslated locales fall back to the index rather than
// linking at a route that does not exist.
function header(locale, { currentBlog = false, localeTargets = null } = {}) {
  const s = STRINGS[locale]
  const target = (l) => localeTargets?.[l] || `/${l}/blog/`
  return `<body>
<a class="skip" href="#main">${s.nav.skip}</a>
<header class="site-header">
  <div class="wrap">
    <a class="brand" href="/${locale}/"><img src="/assets/tidy-logo.png" alt="Tidy logo" width="26" height="26"><span>Tidy</span></a>
    <nav class="site-nav" aria-label="${s.nav.main}">
      <a href="/${locale}/#capabilities">${s.nav.features}</a>
      <a href="/${locale}/#trust">${s.nav.safety}</a>
      <a href="/${locale}/blog/"${currentBlog ? ' aria-current="page"' : ''}>${s.nav.blog}</a>
      <span class="lang-switch"><a href="${target('zh')}"${locale === 'zh' ? ' aria-current="true"' : ''}>中</a>/<a href="${target('en')}"${locale === 'en' ? ' aria-current="true"' : ''}>EN</a></span>
      <a class="btn btn-primary" href="https://github.com/rambocode/tidy/releases/latest/download/Tidy.dmg">${s.nav.cta}</a>
    </nav>
  </div>
</header>`
}

function footer(locale) {
  const s = STRINGS[locale]
  const other = otherLocale(locale)
  return `<footer class="site-footer">
  <div class="wrap">
    <p class="footer-note" style="margin-top:0;border:0;padding-top:0">
      <a href="/${locale}/">${s.footer.home}</a> ·
      <a href="/${locale}/blog/">${s.nav.blog}</a> ·
      <a href="/${locale}/blog/rss.xml">RSS</a> ·
      <a href="/${locale}/privacy/">${s.footer.privacy}</a> ·
      <a href="/${locale}/terms/">${s.footer.terms}</a> ·
      <a href="/${other}/blog/">${s.footer.other}</a>
    </p>
  </div>
</footer>
</body>
</html>
`
}

function write(relPath, contents) {
  const file = path.resolve(root, relPath)
  fs.mkdirSync(path.dirname(file), { recursive: true })
  fs.writeFileSync(file, contents, 'utf8')
}

// ---------- index pages ----------

for (const locale of Object.keys(index)) {
  const s = STRINGS[locale]
  const data = index[locale]
  const alternates = Object.keys(index).map((l) => ({ lang: l, url: index[l].url }))

  const catNav = data.categories
    .map((c) => `<a href="#cat-${esc(c.category)}">${esc(s.categories[c.category] || c.category)}</a>`)
    .join('\n        ')

  const sections = data.categories.map((c) => `
      <section id="cat-${esc(c.category)}" style="padding-block:0 40px;border:0">
        <h2>${esc(s.categories[c.category] || c.category)}</h2>
        ${c.items.length ? `<ul class="post-list">${c.items.map((p) => `
          <li class="post-card">
            <h3><a href="${esc(p.route)}">${esc(p.title)}</a></h3>
            <p>${esc(p.description)}</p>
            <div class="post-meta">
              ${p.featured ? '<span class="badge">★</span>' : ''}
              <span>${esc(s.date(p.publishedAt))}</span>
              <span>${esc(s.readingMinutes(p.readingMinutes))}</span>
              ${p.author ? `<span>${esc(p.author)}</span>` : ''}
            </div>
          </li>`).join('')}</ul>` : `<p class="empty">${esc(s.empty)}</p>`}
      </section>`).join('\n')

  const jsonLd = {
    '@context': 'https://schema.org',
    '@type': 'Blog',
    name: s.blogTitle,
    description: s.blogLead,
    inLanguage: locale,
    url: data.url,
  }

  write(`${locale}/blog/index.html`, `${head({
    locale,
    title: `${s.blogTitle} — ${s.blogHead}`,
    description: s.blogLead,
    canonical: data.url,
    alternates,
    extraHead: `<script type="application/ld+json">${JSON.stringify(jsonLd)}</script>`,
  })}
${header(locale, { currentBlog: true })}
<main id="main">
  <div class="wrap blog-head">
    <p class="eyebrow">${esc(s.blogTitle)}</p>
    <h1>${esc(s.blogHead)}</h1>
    <p class="lead">${esc(s.blogLead)}</p>
    <nav class="cat-nav" aria-label="${esc(s.blogTitle)}" style="margin-top:24px">
        ${catNav}
        <a href="/${locale}/blog/rss.xml">${esc(s.subscribe)}</a>
    </nav>
  </div>
  <div class="wrap">
${sections}
  </div>
</main>
${footer(locale)}`)
}

// ---------- article pages ----------

let articleCount = 0
for (const locale of Object.keys(index)) {
  const s = STRINGS[locale]
  const dir = path.join(build, 'posts', locale)
  if (!fs.existsSync(dir)) continue

  for (const fileName of fs.readdirSync(dir)) {
    const post = JSON.parse(fs.readFileSync(path.join(dir, fileName), 'utf8'))
    const cta = s.cta[post.cta] || s.cta.source

    const toc = post.toc.length
      ? `<aside class="toc" aria-label="${esc(s.toc)}">
    <h2>${esc(s.toc)}</h2>
    <ul>${post.toc.map((t) => `<li class="d${t.depth}"><a href="#${encodeURIComponent(t.id)}">${esc(t.text)}</a></li>`).join('')}</ul>
  </aside>`
      : '<div></div>'

    const translation = post.alternates.find((a) => a.lang !== locale)
    const related = post.related.length
      ? `<section class="related">
      <h2>${esc(s.related)}</h2>
      <ul class="post-list">${post.related.map((r) => `
        <li class="post-card"><h3><a href="${esc(r.route)}">${esc(r.title)}</a></h3><p>${esc(r.description)}</p></li>`).join('')}</ul>
    </section>`
      : ''

    const series = post.series && (post.series.prev || post.series.next)
      ? `<nav class="post-meta" style="margin-top:32px" aria-label="${esc(post.series.name)}">
      ${post.series.prev ? `<span>← ${esc(s.seriesPrev)}: <a href="${esc(post.series.prev.route)}">${esc(post.series.prev.title)}</a></span>` : ''}
      ${post.series.next ? `<span>${esc(s.seriesNext)}: <a href="${esc(post.series.next.route)}">${esc(post.series.next.title)}</a> →</span>` : ''}
    </nav>`
      : ''

    write(`${locale}/blog/${post.permalink}/index.html`, `${head({
      locale,
      title: `${post.title} — Tidy`,
      description: post.description,
      canonical: post.canonicalUrl,
      alternates: post.alternates,
      robots: post.noindex ? 'noindex,follow' : null,
      extraHead: `<script type="application/ld+json">${JSON.stringify(post.jsonLd)}</script>`,
    })}
${header(locale, {
  currentBlog: true,
  localeTargets: Object.fromEntries(post.alternates.map((a) => [a.lang, a.route])),
})}
<main id="main" class="wrap article-layout">
  <header class="article-head">
    <p class="eyebrow"><a href="/${locale}/blog/#cat-${esc(post.category)}">${esc(s.categories[post.category] || post.category)}</a></p>
    <h1>${esc(post.title)}</h1>
    <div class="article-meta">
      <span>${esc(s.date(post.publishedAt))}</span>
      ${post.updatedAt ? `<span>${esc(s.updated)} ${esc(s.date(post.updatedAt))}</span>` : ''}
      <span>${esc(s.readingMinutes(post.readingMinutes))}</span>
      ${post.author ? `<span>${esc(post.author)}</span>` : ''}
      ${translation ? `<span><a href="${esc(translation.route)}">${esc(s.otherLang)}</a></span>` : ''}
    </div>
  </header>
  <article class="article">
    <div class="prose">
${post.html}
    </div>
    ${series}
    <section class="article-cta">
      <h2>${esc(cta.h)}</h2>
      <p>${esc(cta.p)}</p>
      <a class="btn btn-secondary" href="${esc(cta.href)}" target="_blank" rel="noopener noreferrer">${esc(cta.label)}</a>
    </section>
    ${related}
  </article>
  ${toc}
</main>
${footer(locale)}`)
    articleCount += 1
  }
}

// ---------- RSS ----------

for (const locale of Object.keys(index)) {
  const src = path.join(build, 'rss', `${locale}.xml`)
  if (fs.existsSync(src)) write(`${locale}/blog/rss.xml`, fs.readFileSync(src, 'utf8'))
}

// ---------- sitemap ----------

const xml = (s) => String(s ?? '').replace(/[<>&'"]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;', "'": '&apos;', '"': '&quot;' }[c]))
const abs = (route) => new URL(route, config.siteUrl).toString()
const today = new Date().toISOString().slice(0, 10)

const entries = []

for (const tpl of STATIC_ROUTES) {
  for (const locale of config.locales) {
    const route = tpl.path.replace('{locale}', locale)
    const alts = config.locales.map((l) => ({ lang: l, url: abs(tpl.path.replace('{locale}', l)) }))
    entries.push({ loc: abs(route), lastmod: today, alts, changefreq: tpl.changefreq, priority: tpl.priority })
  }
}

for (const locale of Object.keys(index)) {
  const alts = Object.keys(index).map((l) => ({ lang: l, url: index[l].url }))
  entries.push({ loc: index[locale].url, lastmod: today, alts, changefreq: 'weekly', priority: '0.6' })
}

for (const locale of Object.keys(index)) {
  const dir = path.join(build, 'posts', locale)
  if (!fs.existsSync(dir)) continue
  for (const fileName of fs.readdirSync(dir)) {
    const post = JSON.parse(fs.readFileSync(path.join(dir, fileName), 'utf8'))
    if (post.noindex) continue
    entries.push({
      loc: post.url,
      lastmod: post.updatedAt || post.publishedAt,
      alts: post.alternates.map((a) => ({ lang: a.lang, url: a.url })),
      changefreq: 'monthly',
      priority: '0.5',
    })
  }
}

const sitemap = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">
${entries.map((e) => {
  const links = e.alts.map((a) => `    <xhtml:link rel="alternate" hreflang="${xml(a.lang === 'zh' ? 'zh-Hans' : a.lang)}" href="${xml(a.url)}"/>`).join('\n')
  const xDefault = e.alts.find((a) => a.lang === config.primaryLocale)
  return `  <url>
    <loc>${xml(e.loc)}</loc>
    <lastmod>${xml(e.lastmod)}</lastmod>
    <changefreq>${e.changefreq}</changefreq>
    <priority>${e.priority}</priority>
${links}${xDefault ? `\n    <xhtml:link rel="alternate" hreflang="x-default" href="${xml(xDefault.url)}"/>` : ''}
  </url>`
}).join('\n')}
</urlset>
`
write('sitemap.xml', sitemap)

console.log(`✓ rendered ${articleCount} articles and ${Object.keys(index).length} blog indexes`)
console.log(`✓ sitemap.xml: ${entries.length} URLs`)
