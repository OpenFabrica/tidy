#!/usr/bin/env bash
# 发版门禁：release.sh 和 publish.sh 开头都会跑。任一项不过就退出，不往下走。
#
#   scripts/release-gates.sh build     # make release 前：仓库状态 + 版本号
#   scripts/release-gates.sh publish   # make publish 前：再加产物和标签检查
#
# 单独跑也行，用来在发版前自查。规则见 docs/release.md。
set -euo pipefail

cd "$(dirname "$0")/.."   # desktop/

MODE="${1:-build}"
FAILED=0

# 记一条失败，继续检查下一项，最后一起报。
fail() {
  echo "✗ $*" >&2
  FAILED=1
}

# 记一条通过。
pass() {
  echo "✓ $*"
}

# ── 仓库状态 ─────────────────────────────────────────────────────────
# 发出去的包必须能对回 master 上已推送的某个提交：否则标签指向的代码和
# 包里的代码对不上，出了问题无从查起。
check_repo() {
  local branch
  branch="$(git rev-parse --abbrev-ref HEAD)"
  if [ "$branch" = "master" ]; then
    pass "当前分支是 master"
  else
    fail "当前分支是 ${branch}，发版只能在 master 上"
  fi

  # 只看已跟踪的文件：未跟踪的临时文件不影响构建结果。
  if [ -z "$(git status --porcelain --untracked-files=no)" ]; then
    pass "已跟踪文件没有未提交改动"
  else
    fail "有未提交的改动（git status 查看），先提交或撤回"
  fi

  if ! git fetch -q origin master; then
    fail "无法连接 origin，没法确认 master 已推送"
    return
  fi
  if [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ]; then
    pass "HEAD 与 origin/master 一致"
  else
    fail "HEAD 与 origin/master 不一致（未推送或落后），先 git push / git pull"
  fi
}

# ── 版本号 ───────────────────────────────────────────────────────────
# 版本号写在 5 个文件里（见 docs/release.md），必须完全一致。
# 结果写进全局变量 VERSION，后面的检查要用。
VERSION=""
check_versions() {
  local conf cargo pkg lock_root lock_pkg cargo_lock
  conf="$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")"
  cargo="$(sed -n 's/^version = "\(.*\)"$/\1/p' src-tauri/Cargo.toml | head -1)"
  pkg="$(python3 -c "import json;print(json.load(open('ui/package.json'))['version'])")"
  lock_root="$(python3 -c "import json;print(json.load(open('ui/package-lock.json'))['version'])")"
  lock_pkg="$(python3 -c "import json;print(json.load(open('ui/package-lock.json'))['packages']['']['version'])")"
  # Cargo.lock 里 tidy 包那一段的下一行就是它的版本号。
  cargo_lock="$(awk '/^name = "tidy"$/{getline; gsub(/version = |"/, ""); print; exit}' Cargo.lock)"

  VERSION="$conf"
  local all="$conf $cargo $pkg $lock_root $lock_pkg $cargo_lock"
  if [ "$(echo "$all" | tr ' ' '\n' | sort -u | wc -l | tr -d ' ')" = "1" ]; then
    pass "5 个文件的版本号一致：$VERSION"
  else
    fail "版本号不一致：tauri.conf.json=$conf Cargo.toml=$cargo package.json=$pkg package-lock.json=$lock_root/$lock_pkg Cargo.lock=$cargo_lock"
  fi

  # 正式版只能是 X.Y.Z：带后缀的版本号不应该发出去。
  if [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    pass "版本号是正式版格式"
  else
    fail "版本号 $VERSION 不是 X.Y.Z 格式，不能发版"
  fi
}

# ── 标签 ─────────────────────────────────────────────────────────────
# 已推送的标签不移动：v<版本> 已经指向别的提交，说明这个版本已经发过，
# 应该升版本号而不是重发。指向当前提交则是同一次发版的重试，允许。
check_tag() {
  local tag="v$VERSION" remote
  remote="$(git ls-remote --tags origin "refs/tags/$tag" | cut -f1)"
  if [ -z "$remote" ]; then
    pass "标签 $tag 还不存在"
  elif [ "$remote" = "$(git rev-parse HEAD)" ]; then
    pass "标签 $tag 已指向当前提交（视为同一次发版重试）"
  else
    fail "标签 $tag 已存在且指向 ${remote:0:7}，不是当前提交。这个版本已经发过，请升版本号"
  fi
}

# ── 产物（publish 专用） ─────────────────────────────────────────────
# 要发的包必须就是这个版本：防止拿旧构建发新版本号。
check_artifacts() {
  local target="${TIDY_BUILD_TARGET:-universal-apple-darwin}"
  local bundle="target/$target/release/bundle"
  local app="$bundle/macos/Tidy.app"

  if [ "$target" != "universal-apple-darwin" ]; then
    fail "TIDY_BUILD_TARGET=$target 不是 universal，Intel 或 Apple 芯片用户会装不上"
  fi

  if [ -d "$app" ]; then
    local built
    built="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$app/Contents/Info.plist")"
    if [ "$built" = "$VERSION" ]; then
      pass "构建产物版本是 $VERSION"
    else
      fail "构建产物版本是 ${built}，源码是 ${VERSION}。先重新 make release"
    fi
  else
    fail "找不到 ${app}，先 make release"
  fi

  if ls "$bundle"/dmg/*_"$VERSION"_*.dmg >/dev/null 2>&1; then
    pass "DMG 文件名带 $VERSION"
  else
    fail "找不到 $VERSION 的 DMG，先 make release"
  fi
}

case "$MODE" in
  build)
    check_repo
    check_versions
    check_tag
    ;;
  publish)
    check_repo
    check_versions
    check_tag
    check_artifacts
    ;;
  *)
    echo "用法：$0 build|publish" >&2
    exit 64
    ;;
esac

if [ "$FAILED" -ne 0 ]; then
  echo "✗ 发版门禁没通过，已停止。规则见 docs/release.md。" >&2
  exit 1
fi
echo "✓ 发版门禁通过（${MODE}，v${VERSION}）"
