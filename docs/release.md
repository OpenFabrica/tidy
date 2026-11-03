# 发版

Tidy 只有 macOS 桌面版。发版靠两个脚本：`make release`（构建、签名、公证）和
`make publish`（发 GitHub Release 和自更新 feed）。本文写的是这两个脚本外面的步骤和规则。

## 版本号

版本号写在 3 个文件里，另有 2 个锁文件跟着变。5 个文件必须一致：

| 文件 | 说明 |
|---|---|
| `desktop/src-tauri/tauri.conf.json` 的 `version` | 主版本号。`publish.sh` 读它决定标签名 `v<版本>` |
| `desktop/src-tauri/Cargo.toml` 的 `version` | |
| `desktop/ui/package.json` 的 `version` | |
| `desktop/Cargo.lock`、`desktop/ui/package-lock.json` | 用下面的命令更新，不手改 |

升版本号（例：0.1.4 → 0.1.5）：

```bash
cd desktop
sed -i '' 's/"version": "0.1.4"/"version": "0.1.5"/' src-tauri/tauri.conf.json
sed -i '' 's/^version = "0.1.4"/version = "0.1.5"/' src-tauri/Cargo.toml
(cd ui && npm version 0.1.5 --no-git-tag-version)
cargo check -p tidy        # 更新 Cargo.lock
git diff --stat            # 应该正好是上表 5 个文件
```

默认补丁号加一。用户说了要升次版本号或主版本号，才按用户说的改。

## 一次性准备

| 需要什么 | 在哪里 | 缺了会怎样 |
|---|---|---|
| 签名身份 `Developer ID Application: Jiangwei Lan (HQ537XMLJY)` | 钥匙串；`release.sh` 默认就用它 | 构建失败 |
| 公证凭据 | 钥匙串 profile `tidy-notary`（`release.sh` 默认），或 `desktop/.env` 里的 `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID`。备用：`NOTARY_PROFILE=aster-notary make release` | `release.sh` 开头就停，不白编译 |
| 自更新签名私钥 `tidy-updater.key` | `desktop/.tauri/` 或 `~/.tauri/`（都不进 git） | 不产出自更新包，`publish` 无法发布 |
| 遥测配置 `TIDY_TELEMETRY_URL` / `TIDY_TELEMETRY_KEY` | 仓库根目录 `.env`（不进 git），`release.sh` 自动加载 | 编译期写入；缺了发出去的包就没有遥测 |
| `gh` 登录，对 `OpenFabrica/tidy` 有写权限 | `gh auth status` | `publish` 失败 |

**自更新私钥丢了，已经装在用户机器上的 Tidy 就永远收不到更新**（公钥编译进了老版本）。
除了上面两处，另存一份到密码管理器。

## 发版步骤

1. **门禁。** 任一不过就停：
   - 在 `master` 上，且已推送：`git status -sb` 显示 `## master...origin/master`，没有 ahead/behind。
   - 已跟踪文件没有未提交改动：`git status --porcelain --untracked-files=no` 为空。
   - `make check` 通过。
2. **起草发布说明。** 按 `git log v<上一版>..HEAD` 起草。面向用户，按区域分组（界面、清理、Docker、状态、安全……），一行一条；
   涉及删除策略时单列「安全」一组，写清楚有没有放宽。格式照 v0.1.3、v0.1.4 的 Release。
   **给用户看过再定稿。** 定稿存成临时文件（如 `/tmp/tidy-<版本>-notes.md`），不进 git。
3. **升版本号**（见上一节），提交 `chore(release): 版本号升到 <版本>`，推送。
4. **构建：`make release`**（约 15 分钟）。产出 universal 包（Intel + Apple 芯片），签名、公证、装订，最后自检 Gatekeeper。
   `TIDY_BUILD_TARGET=aarch64-apple-darwin make release` 只用于本地快速验证，**不要拿去发布**：Intel 用户会装不上。
5. **验收产物：**
   ```bash
   cd desktop/target/universal-apple-darwin/release/bundle
   spctl -a -vv -t exec macos/Tidy.app                                   # 要显示 Notarized Developer ID
   spctl -a -t open --context context:primary-signature dmg/*.dmg -v     # 同上
   lipo -archs macos/Tidy.app/Contents/MacOS/Tidy                        # x86_64 arm64
   lipo -archs macos/Tidy.app/Contents/MacOS/mole-helper                 # x86_64 arm64
   ls macos/Tidy.app.tar.gz macos/Tidy.app.tar.gz.sig                    # 自更新包和签名都在
   ```
6. **发布：`NOTES_FILE=/tmp/tidy-<版本>-notes.md make publish`。** 这一步会在 `master` 当前提交上创建标签 `v<版本>` 和 Release，
   并上传 DMG、固定文件名的 `Tidy.dmg`、自更新包、签名和 `latest.json`。
   **发布前先问用户**（发出去所有用户都会收到更新提示）。用户说「发布」就算同意。
   - 要强制所有人升级到某个版本以上：加 `MINIMUM_VERSION=<版本>`。低于它的版本会弹出关不掉的更新提示。只在有安全问题时用。
7. **发布后验证：**
   ```bash
   gh release list --repo OpenFabrica/tidy --limit 2        # 新版是 Latest
   git fetch --tags && git rev-parse --short v<版本>^{commit} # 指向版本号提交
   curl -sL https://github.com/OpenFabrica/tidy/releases/latest/download/latest.json | grep '"version"'
   curl -sL https://github.com/rambocode/tidy/releases/latest/download/latest.json  | grep '"version"'
   ```
   两个 `latest.json` 都要是新版本。第二个是老版本内置的更新地址，靠仓库跳转。

## 规则

- **发出去就不改。** 已推送的标签不移动、不重打；已发布的 Release 不删、不改回草稿。发错了就发下一个版本。
  `publish.sh` 对已存在的 Release 会用 `--clobber` 覆盖文件——只在同一次发版上传失败、重试时用。
- **不删除、不重建 GitHub 仓库。** 0.1.3 及更早版本的更新地址是 `rambocode/tidy`，靠 GitHub 的仓库迁移跳转到 `OpenFabrica/tidy`。
  删库重建会断掉跳转，这些用户就收不到更新。
- **`latest.json` 里的下载地址指向固定标签**（`/download/v<版本>/`），不能写 `/latest/`。`publish.sh` 已经这样做，不要改。
- **网站下载按钮用固定地址** `releases/latest/download/Tidy.dmg`，每次发版自动指向新包，网站不用改。
- 日常推送不改版本号，只有发版时改。
- 不打印、不提交任何密码、私钥或 `.env` 内容。

## 尚未采用的通用规则

下面几条来自通用发版规范，和本项目现在的做法不同，**还没有决定是否采用**：

- **开发版号带 `-dev` 标记。** 两次发版之间，源码版本号写成 `0.1.5-dev`，发版时去掉；发版后马上推到下一个开发版。
  现在的做法：发版前才把版本号从上一版改到新版。
- **`CHANGELOG.md`。** 现在的发布说明只在 GitHub Release 里，仓库里没有更新日志文件。
- **注释标签。** 现在的标签由 `gh release create` 创建，是轻量标签。
- **门禁写进脚本。** 现在第 1 步的检查靠手动做，`release.sh` / `publish.sh` 不检查分支和工作区。
