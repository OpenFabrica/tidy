#!/usr/bin/env bash
# 把 make release 产出的构建发到 GitHub Releases，并生成自更新用的 latest.json。
#
#   make publish                          # 用 tauri.conf.json 里的版本号
#   MINIMUM_VERSION=0.4.0 make publish    # 同时设置强制更新下限
#   NOTES_FILE=notes.md make publish      # 自定义 release notes
#
# 前提：
#   1. 先跑过 make release（签名 + 公证 + 装订），本脚本不重新构建；
#   2. `gh auth status` 已登录，且对 $REPO 有写权限；
#   3. 私钥在 ~/.tauri/tidy-updater.key —— 丢了它，已经装在用户机器上的
#      Tidy 就永远收不到更新了（公钥编译进了老版本，换不掉）。备份它。
set -euo pipefail

cd "$(dirname "$0")/.."   # desktop/

REPO="${REPO:-rambocode/tidy}"
CONF="src-tauri/tauri.conf.json"
VERSION="$(python3 -c "import json;print(json.load(open('$CONF'))['version'])")"
TAG="v$VERSION"

TARGET="${TIDY_BUILD_TARGET:-universal-apple-darwin}"
BUNDLE="target/$TARGET/release/bundle"
DMG="$(ls "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -1 || true)"
TARBALL="$(ls "$BUNDLE"/macos/*.app.tar.gz 2>/dev/null | head -1 || true)"
SIGFILE="$(ls "$BUNDLE"/macos/*.app.tar.gz.sig 2>/dev/null | head -1 || true)"

if [ -z "$DMG" ] || [ -z "$TARBALL" ] || [ -z "$SIGFILE" ]; then
  cat >&2 <<MSG
✗ 缺少发布产物。先跑 make release。
  dmg:     ${DMG:-(缺)}
  tar.gz:  ${TARBALL:-(缺)}
  sig:     ${SIGFILE:-(缺)}
  没有 .sig 通常意味着构建时没有 TAURI_SIGNING_PRIVATE_KEY_PATH，
  或者 tauri.conf.json 里 bundle.createUpdaterArtifacts 不是 true。
MSG
  exit 1
fi

# 更新包的下载地址必须指向一个**固定**的 tag，不能用 /latest/：老版本读到的
# latest.json 里如果写 /latest/，某天发新版时这个 URL 会指向另一个包，签名
# 对不上，更新反而全线失败。
ASSET_BASE="https://github.com/$REPO/releases/download/$TAG"
SIGNATURE="$(cat "$SIGFILE")"

# 构建产物的架构：universal 包同时服务两种 Mac，单架构包只服务本机架构。
if [ "$TARGET" = "universal-apple-darwin" ]; then
  TARGETS=(darwin-universal darwin-aarch64 darwin-x86_64)
else
  case "$(uname -m)" in
    arm64) TARGETS=(darwin-aarch64) ;;
    *) TARGETS=(darwin-x86_64) ;;
  esac
  echo "⚠ 非 universal 构建：latest.json 只覆盖 ${TARGETS[0]}，另一种架构的用户收不到更新。"
fi

# 站点的下载按钮指向一个**固定文件名**的副本。带版本号的文件名在
# /releases/latest/download/ 下每次发版都会失效，那样站点就得跟着改一次。
STABLE_DMG="$BUNDLE/dmg/Tidy.dmg"
cp -f "$DMG" "$STABLE_DMG"

NOTES="$( [ -n "${NOTES_FILE:-}" ] && cat "$NOTES_FILE" || echo "Tidy $VERSION" )"

LATEST_JSON="$BUNDLE/latest.json"
VERSION="$VERSION" NOTES="$NOTES" SIGNATURE="$SIGNATURE" \
ASSET_BASE="$ASSET_BASE" TARBALL_NAME="$(basename "$TARBALL")" \
TARGETS="${TARGETS[*]}" MINIMUM_VERSION="${MINIMUM_VERSION:-}" \
python3 - > "$LATEST_JSON" <<'PY'
import json, os, datetime

feed = {
    "version": os.environ["VERSION"],
    "notes": os.environ["NOTES"],
    "pub_date": datetime.datetime.now(datetime.timezone.utc)
    .replace(microsecond=0)
    .isoformat()
    .replace("+00:00", "Z"),
    "platforms": {
        target: {
            "signature": os.environ["SIGNATURE"],
            "url": f"{os.environ['ASSET_BASE']}/{os.environ['TARBALL_NAME']}",
        }
        for target in os.environ["TARGETS"].split()
    },
}
# minimum_version 不是 Tauri 的标准字段，是 Tidy 自己的止血开关：低于它的
# 版本启动时会弹一个关不掉的更新提示（见 src-tauri/src/update.rs）。
if os.environ.get("MINIMUM_VERSION"):
    feed["minimum_version"] = os.environ["MINIMUM_VERSION"]
print(json.dumps(feed, indent=2, ensure_ascii=False))
PY

echo "▶ latest.json"
cat "$LATEST_JSON"

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "▶ 更新已存在的 release $TAG"
  gh release upload "$TAG" "$DMG" "$STABLE_DMG" "$TARBALL" "$SIGFILE" "$LATEST_JSON" \
    --repo "$REPO" --clobber
else
  echo "▶ 创建 release $TAG"
  gh release create "$TAG" "$DMG" "$STABLE_DMG" "$TARBALL" "$SIGFILE" "$LATEST_JSON" \
    --repo "$REPO" --title "Tidy $VERSION" --notes "$NOTES"
fi

echo "✅ 已发布：https://github.com/$REPO/releases/tag/$TAG"
echo "   自更新 feed：https://github.com/$REPO/releases/latest/download/latest.json"
echo "   固定下载地址：https://github.com/$REPO/releases/latest/download/Tidy.dmg"
