// 前端事件 → mole-telemetry 事件的翻译层，也是隐私承诺的执行点。
//
// 前端传过来的是普通 String。这个模块把每个字段过一遍白名单，换成
// `&'static str` 再交给 mole-telemetry。白名单外的值一律变成 "other"，
// 绝不原样透传——否则一个手滑的 track 调用就能把路径送上网。

use serde::Deserialize;

/// 前端 `telemetry_track` 的负载。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackRequest {
    ViewOpened {
        view: String,
    },
    ScanCompleted {
        scan: String,
        duration_ms: u64,
    },
    CleanExecuted {
        mode: String,
        result: String,
    },
    AppUninstalled,
    OptimizeRun {
        action: String,
    },
    UpdatesRun {
        source: String,
    },
    SelfUpdate {
        from: String,
        to: String,
        result: String,
    },
    ErrorOccurred {
        code: String,
        view: String,
    },
}

impl TrackRequest {
    /// 翻译成封闭事件。`home` 用于查优化任务目录。
    pub fn into_event(self, home: &str) -> mole_telemetry::Event {
        use mole_telemetry::Event;
        match self {
            TrackRequest::ViewOpened { view } => Event::ViewOpened {
                view: view_of(&view),
            },
            TrackRequest::ScanCompleted { scan, duration_ms } => Event::ScanCompleted {
                kind: scan_of(&scan),
                duration_ms,
            },
            TrackRequest::CleanExecuted { mode, result } => Event::CleanExecuted {
                mode: clean_mode_of(&mode),
                result: outcome_of(&result),
            },
            TrackRequest::AppUninstalled => Event::AppUninstalled,
            TrackRequest::OptimizeRun { action } => Event::OptimizeRun {
                action: optimize_action_of(home, &action),
            },
            TrackRequest::UpdatesRun { source } => Event::UpdatesRun {
                source: update_source_of(&source),
            },
            TrackRequest::SelfUpdate { from, to, result } => Event::SelfUpdate {
                from,
                to,
                result: outcome_of(&result),
            },
            TrackRequest::ErrorOccurred { code, view } => Event::ErrorOccurred {
                code: error_code_of(&code),
                view: view_of(&view),
            },
        }
    }
}

/// 在白名单里找到就返回白名单自己的那个 `&'static str`，否则 "other"。
/// 返回的是常量表里的引用而不是入参，所以调用方的 String 不可能泄出去。
fn pick(allowed: &[&'static str], value: &str) -> &'static str {
    allowed
        .iter()
        .find(|candidate| **candidate == value)
        .copied()
        .unwrap_or("other")
}

/// 界面名，与 router 注册的路由一致。
fn view_of(value: &str) -> &'static str {
    pick(
        &["clean", "apps", "optimize", "analyze", "status", "settings"],
        value,
    )
}

/// 扫描类型。
fn scan_of(value: &str) -> &'static str {
    pick(
        &[
            "clean",
            "uninstall",
            "purge",
            "docker",
            "simulators",
            "analyze",
            "installer",
            "updates",
        ],
        value,
    )
}

/// 清理删除模式，与设置里的 delete-mode 一致。
fn clean_mode_of(value: &str) -> &'static str {
    pick(&["trash", "permanent"], value)
}

/// 操作结果。
fn outcome_of(value: &str) -> &'static str {
    pick(&["ok", "partial", "failed", "cancelled"], value)
}

/// 「帮别的应用升级」的来源，与 mole-ops::updates 的取值一致。
fn update_source_of(value: &str) -> &'static str {
    pick(
        &["homebrew", "app_store", "sparkle", "electron", "website"],
        value,
    )
}

/// 错误码，与 IpcError 的稳定码表一致。
fn error_code_of(value: &str) -> &'static str {
    pick(
        &[
            "protected_path",
            "plan_expired",
            "plan_not_found",
            "selection_mismatch",
            "cancelled",
            "requires_admin",
            "io",
            "update_check_failed",
            "update_install_failed",
        ],
        value,
    )
}

/// 优化任务 id。不复制一份 22 条的清单，直接回查 mole-ops 的静态目录并返回
/// 目录里的那个 `&'static str`——目录加任务时这里自动跟上，且不会有第二份
/// 会过期的常量表。
fn optimize_action_of(home: &str, value: &str) -> &'static str {
    mole_ops::optimize::tasks(home)
        .into_iter()
        .find(|task| task.id == value)
        .map(|task| task.id)
        .unwrap_or("other")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_never_pass_through() {
        assert_eq!(view_of("/Users/mike/Documents"), "other");
        assert_eq!(scan_of("../../etc/passwd"), "other");
        assert_eq!(error_code_of("failed to delete /Users/mike/a.txt"), "other");
        assert_eq!(update_source_of("Sparkle"), "other");
    }

    #[test]
    fn known_values_map_to_themselves() {
        assert_eq!(view_of("clean"), "clean");
        assert_eq!(clean_mode_of("trash"), "trash");
        assert_eq!(outcome_of("cancelled"), "cancelled");
        assert_eq!(error_code_of("requires_admin"), "requires_admin");
    }

    #[test]
    fn optimize_action_is_resolved_against_the_real_catalog() {
        let home = "/Users/nobody";
        assert_eq!(optimize_action_of(home, "flushDNS"), "flushDNS");
        assert_eq!(optimize_action_of(home, "rm -rf /"), "other");
    }

    #[test]
    fn track_request_deserializes_from_the_frontend_shape() {
        let req: TrackRequest =
            serde_json::from_str(r#"{"kind":"view_opened","view":"clean"}"#).unwrap();
        assert!(matches!(
            req.into_event("/Users/nobody"),
            mole_telemetry::Event::ViewOpened { view: "clean" }
        ));
    }
}
