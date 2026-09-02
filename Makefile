# Tidy standalone Makefile — adapted from the original Mole repo's desktop-*
# targets so the README's `make desktop-*` commands also work in this checkout.
.PHONY: dev build bundle release publish site-deploy check desktop-dev desktop-build desktop-bundle desktop-check

# The tauri CLI must run from desktop/ (it locates src-tauri/tauri.conf.json
# by scanning subfolders of the invocation directory).
# Dev picks a free localhost port each run (5173 is often taken by another
# Vite project) and hands the SAME port to Vite (TAURI_DEV_PORT, read by
# ui/vite.config.ts) and to the Tauri CLI (devUrl override), so the two never
# disagree. Set TAURI_DEV_PORT yourself to pin one.
dev:
	cd desktop && npm install --prefix ui && \
	PORT=$${TAURI_DEV_PORT:-$$(node -e 'const s=require("net").createServer();s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')} && \
	echo "dev server on http://localhost:$$PORT" && \
	TAURI_DEV_PORT=$$PORT ui/node_modules/.bin/tauri dev --config "{\"build\":{\"devUrl\":\"http://localhost:$$PORT\"}}"

build:
	cd desktop && npm ci --prefix ui && ui/node_modules/.bin/tauri build --no-bundle

# Full bundle: Tidy.app + DMG, both carrying icons/icon.icns. The extra
# set-dmg-icon step themes the .dmg FILE; tauri only themes the mounted volume.
bundle:
	cd desktop && npm ci --prefix ui && ui/node_modules/.bin/tauri build
	desktop/scripts/set-dmg-icon.sh desktop/target/release/bundle/dmg/*.dmg

# Signed + notarized DMG (see desktop/scripts/release.sh for the one-time
# notarytool credential setup).
release:
	desktop/scripts/release.sh

# 把 make release 的产物发到 GitHub Releases，并生成自更新 feed。
publish:
	desktop/scripts/publish.sh

# 部署站点到 Cloudflare Pages。
#
# 不直接上传 site/：那个目录里混着可部署产物和源材料——tools/（13MB 的
# node_modules）、content/（文章 Markdown 源）、docs/（站点规格）、.blog-build/
# （中间产物）都不该出现在公网上。先 rsync 出一份干净的再传。
site-deploy:
	rm -rf .site-dist && mkdir -p .site-dist
	rsync -a \
	  --exclude 'tools/' --exclude 'content/' --exclude 'docs/' \
	  --exclude '.blog-build/' --exclude 'README.md' --exclude '.gitignore' \
	  site/ .site-dist/
	npx --yes wrangler@latest pages deploy .site-dist \
	  --project-name=tidy-site --branch=main --commit-dirty=true
	rm -rf .site-dist

check:
	cd desktop && cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace
	cd desktop/ui && npm run check

# Parent-repo-compatible aliases.
desktop-dev: dev
desktop-build: build
desktop-bundle: bundle
desktop-check: check
