#!/usr/bin/env bash
# Local release: signed app + DMG, notarized and stapled.
#
#   make release                       # uses the defaults below
#   NOTARY_PROFILE=xxx make release    # different notarytool keychain profile
#
# Credentials, either of:
#   a) a .env file (repo root or desktop/, git-ignored) with
#        APPLE_SIGNING_IDENTITY="Developer ID Application: ..."   (optional)
#        APPLE_ID=<apple id>  APPLE_PASSWORD=<app-specific pw>  APPLE_TEAM_ID=HQ537XMLJY
#   b) a notarytool keychain profile (one-time):
#        xcrun notarytool store-credentials tidy-notary \
#          --apple-id <apple id> --team-id HQ537XMLJY --password <app-specific pw>
#
# Steps: tauri build (codesigns Tidy.app with the Developer ID + hardened
# runtime) → stamp the DMG icon → codesign the DMG → notarize → staple →
# gatekeeper self-check. Everything after the build is skipped with a clear
# message when the notary profile is missing, so the signed DMG is still
# produced.
set -euo pipefail

cd "$(dirname "$0")/.."   # desktop/

# Load credentials from .env (desktop/.env wins over the repo root one).
for envfile in ../.env .env; do
  if [ -f "$envfile" ]; then
    echo "▶ loading $envfile"
    set -a; . "$envfile"; set +a
  fi
done

IDENTITY="${APPLE_SIGNING_IDENTITY:-Developer ID Application: Jiangwei Lan (HQ537XMLJY)}"
PROFILE="${NOTARY_PROFILE:-tidy-notary}"

# 自更新包的 minisign 私钥。没有它 tauri 不会产出 .app.tar.gz.sig，
# publish 这一步就没东西可发（release.sh 本身仍然能出签名 DMG）。
#
# 查找顺序：显式环境变量 → 项目内副本 desktop/.tauri/（git-ignored）→ ~/.tauri/。
# 项目内那份是刻意留的备份：这把私钥丢了，已经装在用户机器上的 Tidy 就永远
# 收不到更新（公钥编译进了老版本，换不掉）。两处都在同一台机器上，所以它只
# 防误删，不防丢机器 —— 真正的备份请另外存一份到密码管理器。
for candidate in "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ".tauri/tidy-updater.key" "$HOME/.tauri/tidy-updater.key"; do
  if [ -n "$candidate" ] && [ -f "$candidate" ]; then
    export TAURI_SIGNING_PRIVATE_KEY_PATH="$candidate"
    break
  fi
done
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
# 这个 tauri 版本只读 TAURI_SIGNING_PRIVATE_KEY，忽略 ..._PATH。传密钥内容而不是
# 路径，两种版本都吃得下；不这么做的话，构建会一路跑到最后（含公证）才报
# "A public key has been found, but no private key"。
if [ -n "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ] && [ -f "$TAURI_SIGNING_PRIVATE_KEY_PATH" ]; then
  TAURI_SIGNING_PRIVATE_KEY="$(cat "$TAURI_SIGNING_PRIVATE_KEY_PATH")"
  export TAURI_SIGNING_PRIVATE_KEY
fi
if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ] || [ ! -f "$TAURI_SIGNING_PRIVATE_KEY_PATH" ]; then
  echo "⚠ 找不到更新签名私钥（desktop/.tauri/ 与 ~/.tauri/ 都没有）—— 这次不会产出自更新包。" >&2
  unset TAURI_SIGNING_PRIVATE_KEY_PATH
else
  echo "▶ 更新签名私钥：$TAURI_SIGNING_PRIVATE_KEY_PATH"
fi

# 公证凭证的前置检查。不查的话，空的 APPLE_PASSWORD 会一路传到 tauri 内部，
# 在编译完两个架构、签完名之后才抛一个 "HTTP 401 Invalid credentials"——
# 白等十分钟，而且错误信息根本看不出是哪个变量没填。
if [ -z "${APPLE_PASSWORD:-}" ] && ! xcrun notarytool history --keychain-profile "${NOTARY_PROFILE:-tidy-notary}" >/dev/null 2>&1; then
  cat >&2 <<'MSG'
✗ 没有可用的公证凭证，构建终止（没必要先编译十分钟再失败）。

  二选一：
  a) 在 https://account.apple.com → 登录与安全 → App 专用密码 生成一个，
     然后填进 desktop/.env：
       APPLE_PASSWORD=xxxx-xxxx-xxxx-xxxx
  b) 存成钥匙串配置（之后就不用管了）：
       xcrun notarytool store-credentials tidy-notary          --apple-id <apple id> --team-id HQ537XMLJY --password <app 专用密码>
MSG
  exit 2
fi

# rustup 的工具链优先。Homebrew 装的 rust 会排在 PATH 前面，而它只带本机架构，
# 交叉编译会以 "can't find crate for core" 失败——rust-toolchain.toml 里那句
# "Homebrew-installed cargo ignores this file" 说的就是这件事。
if [ -x "$HOME/.cargo/bin/cargo" ]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
echo "▶ cargo: $(command -v cargo) ($(cargo --version))"

# 默认出 universal 包。tauri.conf.json 声明支持 macOS 11，那包含 Intel 机器；
# 只发本机架构的话 Intel 用户连装都装不上，而不是"暂时收不到更新"而已。
# 需要单架构快速验证时：TIDY_BUILD_TARGET=aarch64-apple-darwin make release
TARGET="${TIDY_BUILD_TARGET:-universal-apple-darwin}"

echo "▶ building + signing with: $IDENTITY"
echo "▶ target: $TARGET"
export APPLE_SIGNING_IDENTITY="$IDENTITY"
npm ci --prefix ui
ui/node_modules/.bin/tauri build --target "$TARGET"

BUNDLE="target/$TARGET/release/bundle"
APP="$(ls -d "$BUNDLE"/macos/*.app | head -1)"
DMG="$(ls "$BUNDLE"/dmg/*.dmg | head -1)"
echo "▶ app: $APP"
echo "▶ dmg: $DMG"

echo "▶ verifying app signature"
codesign --verify --deep --strict --verbose=2 "$APP"

# The icon lands in the DMG's resource fork; sign the DMG AFTER that so the
# signature covers the final file.
scripts/set-dmg-icon.sh "$DMG"
echo "▶ signing dmg"
codesign --force --sign "$IDENTITY" --timestamp "$DMG"

# Pick the notary auth: explicit Apple ID vars (from .env) or the keychain
# profile. The password never appears in argv logs beyond this process.
if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  NOTARY_AUTH=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
elif xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
  NOTARY_AUTH=(--keychain-profile "$PROFILE")
else
  cat >&2 <<MSG

⚠ no notary credentials — DMG is signed but NOT notarized.
  Either put APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID in desktop/.env, or store a profile:
    xcrun notarytool store-credentials $PROFILE \\
      --apple-id <your-apple-id> --team-id HQ537XMLJY --password <app-specific-password>
  then re-run: make release
MSG
  exit 2
fi

echo "▶ notarizing (this waits for Apple, usually 1–5 min)"
xcrun notarytool submit "$DMG" "${NOTARY_AUTH[@]}" --wait

echo "▶ stapling"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

echo "▶ gatekeeper check"
spctl --assess --type open --context context:primary-signature -v "$DMG"
echo "✅ release ready: $DMG"
echo "   下一步：make publish  # 生成 latest.json 并发到 GitHub Releases"
