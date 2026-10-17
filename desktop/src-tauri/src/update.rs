// Tidy 自更新：检查、下载、替换、重启。
//
// 走 Rust 侧而不是 updater 的 JS API，是为了不放宽窗口 CSP（`default-src
// 'self'`）——前端一行网络权限都不需要。
//
// 更新包由 GitHub Release 分发，安装前由 tauri-plugin-updater 用内置的
// minisign 公钥验签；签名不符会直接失败，不会落地任何文件。

use crate::error::IpcError;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// 设置键：是否在启动时自动检查更新。
pub const KEY_AUTOCHECK: &str = "update.autocheck";
/// 设置键：上次成功检查的 Unix 秒。
pub const KEY_LAST_CHECK: &str = "update.last_check";

/// 设置键：最近一次真实检查发现的新版本号（没有新版本时为空）。
/// 存盘而不是只放内存：自动检查 24 小时才跑一次，重启后菜单栏面板的
/// 「有更新」红点仍要能显示。
pub const KEY_KNOWN_VERSION: &str = "update.known_version";

/// 自动检查的最小间隔。启动即拉一次会让每天开机十次的人发十次请求，
/// 对一个每周最多发一版的产品毫无意义。
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// 现在是否该做一次自动检查：开关开着，且距上次成功检查超过 24 小时。
pub fn autocheck_due(home: &str) -> bool {
    if !mole_core::settings::bool_or(home, KEY_AUTOCHECK, true) {
        return false;
    }
    let last = mole_core::settings::get(home, KEY_LAST_CHECK)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    now_secs().saturating_sub(last) >= CHECK_INTERVAL_SECS
}

/// 记下这次检查的时间。
fn mark_checked(home: &str) {
    let _ = mole_core::settings::set(home, KEY_LAST_CHECK, &now_secs().to_string());
}

/// 当前 Unix 秒。
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 一个可用更新的摘要。
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    /// 新版本号。
    pub version: String,
    /// 当前运行的版本号。
    pub current_version: String,
    /// release notes（feed 里的 `notes` 字段）。
    pub notes: Option<String>,
    /// 当前版本低于 feed 声明的 `minimum_version`：出事版本的止血通道，
    /// 前端据此把更新提示改成不可关闭。
    pub mandatory: bool,
}

/// 下载进度，通过 Channel 推给前端。
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// 查询是否有新版本。没有新版本返回 None。
///
/// `auto` 表示这是启动时的自动检查：受开关和 24 小时频率限制约束。用户在
/// 设置里手动点「检查更新」时传 false，永远真发一次请求。
pub async fn check(
    app: &AppHandle,
    auto: bool,
    home: &str,
) -> Result<Option<UpdateInfo>, IpcError> {
    if auto && !autocheck_due(home) {
        return Ok(None);
    }
    let updater = app
        .updater()
        .map_err(|e| IpcError::new("update_check_failed", e.to_string()))?;
    let found = updater
        .check()
        .await
        .map_err(|e| IpcError::new("update_check_failed", e.to_string()))?;
    mark_checked(home);
    let known = found.as_ref().map_or("", |update| update.version.as_str());
    let _ = mole_core::settings::set(home, KEY_KNOWN_VERSION, known);
    Ok(found.map(|update| UpdateInfo {
        mandatory: below_minimum(&update.current_version, &update.raw_json),
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
    }))
}

/// 下载并安装，然后重启应用。
///
/// 这里刻意重新 check 一次而不是复用上一次的句柄：句柄要跨 IPC 调用存活就得
/// 塞进全局状态，而更新是低频操作，多一次 HTTP 换掉一份可能过期的状态很划算。
pub async fn install(app: &AppHandle, progress: Channel<DownloadProgress>) -> Result<(), IpcError> {
    let updater = app
        .updater()
        .map_err(|e| IpcError::new("update_install_failed", e.to_string()))?;
    let update = updater
        .check()
        .await
        .map_err(|e| IpcError::new("update_install_failed", e.to_string()))?
        .ok_or_else(|| IpcError::new("update_install_failed", "no update available"))?;

    let from = update.current_version.clone();
    let to = update.version.clone();
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.send(DownloadProgress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| IpcError::new("update_install_failed", e.to_string()))?;

    // 成功事件必须在这里记：restart() 之后是新进程，旧版本号就没人知道了。
    // flush 紧随其后，否则这条事件会跟着被替换掉的进程一起消失。
    mole_telemetry::track(mole_telemetry::Event::SelfUpdate {
        from,
        to,
        result: "ok",
    });
    mole_telemetry::flush();
    app.restart();
}

/// 已知有比当前运行版本更新的版本吗？只读缓存，不发网络请求。
/// 装完新版后缓存里的版本号不再比当前新，红点自然消失。
pub fn update_known(home: &str) -> bool {
    mole_core::settings::get(home, KEY_KNOWN_VERSION)
        .is_some_and(|known| newer_than(&known, env!("CARGO_PKG_VERSION")))
}

/// `known` 是否严格高于 `current`；任一版本号解析失败都当作"不是"。
fn newer_than(known: &str, current: &str) -> bool {
    match (
        semver::Version::parse(known),
        semver::Version::parse(current),
    ) {
        (Ok(known), Ok(current)) => known > current,
        _ => false,
    }
}

/// 当前版本是否低于 feed 里的 `minimum_version`。
///
/// `minimum_version` 不是 Tauri 的标准字段，是我们自己加在 latest.json 里的
/// 止血开关。字段缺失或版本号解析失败一律当作"不强制"——止血机制自己出错时
/// 必须往宽松方向失败，绝不能把用户锁在一个关不掉的弹窗里。
fn below_minimum(current: &str, raw: &serde_json::Value) -> bool {
    let Some(minimum) = raw.get("minimum_version").and_then(|v| v.as_str()) else {
        return false;
    };
    match (
        semver::Version::parse(current),
        semver::Version::parse(minimum),
    ) {
        (Ok(current), Ok(minimum)) => current < minimum,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minimum_version_marks_older_builds_mandatory() {
        let feed = json!({ "minimum_version": "0.3.0" });
        assert!(below_minimum("0.2.9", &feed));
        assert!(!below_minimum("0.3.0", &feed));
        assert!(!below_minimum("1.0.0", &feed));
    }

    #[test]
    fn cached_version_counts_only_when_strictly_newer() {
        assert!(newer_than("0.2.0", "0.1.2"));
        assert!(!newer_than("0.1.2", "0.1.2"));
        assert!(!newer_than("", "0.1.2"));
        assert!(!newer_than("garbage", "0.1.2"));
    }

    #[test]
    fn a_broken_or_missing_minimum_version_never_locks_the_user_in() {
        assert!(!below_minimum("0.1.0", &json!({})));
        assert!(!below_minimum(
            "0.1.0",
            &json!({ "minimum_version": "not-a-version" })
        ));
        assert!(!below_minimum(
            "not-a-version",
            &json!({ "minimum_version": "9.0.0" })
        ));
    }
}
