// Shared plan executor: the ONLY consumer of mole-core::sink in this crate.
// Every feature's execute path funnels here, so whitelist checks, session
// markers, per-item logging, and progress reporting cannot diverge between
// features. Preview (dry_run=true) and real execution consume the same plan.

use mole_core::logging::{DeletionsLog, OperationsLog};
use mole_core::plan::{DeletionPlan, Scope, SizeKb};
use mole_core::policy::{is_path_whitelisted, PolicyCtx};
use mole_core::probes::LiveProbes;
use mole_core::providers::{PrivilegedRunner, TrashProvider};
use mole_core::sink::{self, DeleteError, DeleteMode, DeleteOutcome, OpContext};
use mole_core::state::load_whitelist;
use mole_core::validate::ValidationCtx;
use serde::Serialize;

/// Per-item execution result, streamed to the UI and returned in the report.
#[derive(Debug, Clone, Serialize)]
pub struct ExecItem {
    pub id: String,
    pub path: String,
    /// trashed / removed / dry-run / skipped / missing / failed.
    pub outcome: String,
    pub size_kb: Option<u64>,
    pub error: Option<String>,
}

/// Full execution report for one plan run.
#[derive(Debug, Clone, Serialize)]
pub struct ExecReport {
    pub items: Vec<ExecItem>,
    pub total_freed_kb: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Providers the engine deletes through; injected so tests never touch the
/// real Trash or a privileged transport.
pub struct Providers<'a> {
    pub trash: &'a dyn TrashProvider,
    pub privileged: &'a dyn PrivilegedRunner,
    pub probes: &'a dyn LiveProbes,
}

/// Execution settings for one run.
pub struct ExecOptions {
    pub home: String,
    /// Command tag for the shared logs ("clean", "uninstall", ...).
    pub command: String,
    pub mode: DeleteMode,
    pub dry_run: bool,
    /// MOLE_UNINSTALL_MODE: lifts data-protection for explicit uninstalls.
    pub uninstall_mode: bool,
}

/// Execute the selected candidates of a plan through the deletion sink.
/// Selection ids outside the plan are an error the CALLER must have rejected
/// already (two-phase contract); here they are skipped defensively.
pub fn execute(
    plan: &DeletionPlan,
    selection: &[String],
    opts: &ExecOptions,
    providers: &Providers,
    mut progress: impl FnMut(&ExecItem),
) -> ExecReport {
    let whitelist = load_whitelist(&opts.home);
    let deletions_log = DeletionsLog::from_env(&opts.home);
    let oplog = OperationsLog::from_env(&opts.home);
    oplog.rotate_if_needed();
    // Session markers only for real runs: dry-run previews must not pollute
    // the `mo history` session rollup (CLI parity).
    if !opts.dry_run {
        oplog.session_start(&opts.command);
    }

    let ctx = OpContext {
        validation: ValidationCtx {
            policy: PolicyCtx {
                home: opts.home.clone(),
                uninstall_mode: opts.uninstall_mode,
            },
            probes: providers.probes,
        },
        deletions_log: &deletions_log,
        oplog: &oplog,
        command: opts.command.clone(),
        dry_run: opts.dry_run,
        trash: providers.trash,
        privileged: providers.privileged,
    };

    let mut report = ExecReport {
        items: Vec::new(),
        total_freed_kb: 0,
        skipped: 0,
        failed: 0,
    };

    for id in selection {
        let Some(candidate) = plan.find(id) else {
            continue;
        };
        let mut item = ExecItem {
            id: candidate.id.clone(),
            path: candidate.path.clone(),
            outcome: String::new(),
            size_kb: match candidate.size_kb {
                SizeKb::Known(kb) => Some(kb),
                SizeKb::Unknown => None,
            },
            error: None,
        };

        // Whitelist is enforced at the engine (the caller-side filter the CLI
        // runs in _safe_clean_impl); the sink re-checks policy separately.
        if is_path_whitelisted(&candidate.path, &whitelist.patterns) {
            item.outcome = "skipped".into();
            item.error = Some("whitelisted".into());
            oplog.operation(&opts.command, "SKIPPED", &candidate.path, "whitelist");
            report.skipped += 1;
            progress(&item);
            report.items.push(item);
            continue;
        }

        match sink::delete(
            &candidate.path,
            candidate.scope,
            opts.mode,
            candidate.identity.as_ref(),
            &ctx,
        ) {
            Ok(DeleteOutcome::Trashed { size_kb }) => {
                item.outcome = "trashed".into();
                add_freed(&mut report, &mut item, size_kb);
            }
            Ok(DeleteOutcome::Removed { size_kb }) => {
                item.outcome = "removed".into();
                add_freed(&mut report, &mut item, size_kb);
            }
            Ok(DeleteOutcome::DryRun { size_kb }) => {
                item.outcome = "dry-run".into();
                if let SizeKb::Known(kb) = size_kb {
                    item.size_kb = Some(kb);
                    report.total_freed_kb += kb;
                }
            }
            Ok(DeleteOutcome::Missing) => {
                item.outcome = "missing".into();
            }
            Err(e) => {
                item.outcome = "failed".into();
                item.error = Some(delete_error_code(&e));
                report.failed += 1;
            }
        }
        progress(&item);
        report.items.push(item);
    }

    if !opts.dry_run {
        let freed = report.total_freed_kb;
        let count = report
            .items
            .iter()
            .filter(|i| i.outcome == "trashed" || i.outcome == "removed")
            .count() as u64;
        oplog.session_end(&opts.command, count, freed);
        // Retention: history is log-only storage; keep the newest sessions.
        oplog.prune_sessions();
    }
    report
}

/// Book both the report total and the item's final measured size.
fn add_freed(report: &mut ExecReport, item: &mut ExecItem, size_kb: SizeKb) {
    if let SizeKb::Known(kb) = size_kb {
        item.size_kb = Some(kb);
        report.total_freed_kb += kb;
    }
}

/// Stable error code string for the UI (mirrors sink statuses).
fn delete_error_code(e: &DeleteError) -> String {
    match e {
        DeleteError::EmptyPath => "empty-path".into(),
        DeleteError::Rejected(reason) => format!("rejected:{}", reason.code()),
        DeleteError::MutableParent => "mutable-parent".into(),
        DeleteError::IdentityChanged => "identity-changed".into(),
        DeleteError::HelperUnavailable => "requires_admin".into(),
        DeleteError::PrivacyDenied => "privacy-denied".into(),
        DeleteError::TrashFailed(detail) => format!("trash-failed:{detail}"),
        DeleteError::Io(err) => format!("io:{err}"),
    }
}

/// Count how many selected items are system-scope (UI needs this to warn
/// before execution when the helper is unavailable).
pub fn system_scope_count(plan: &DeletionPlan, selection: &[String]) -> usize {
    selection
        .iter()
        .filter_map(|id| plan.find(id))
        .filter(|c| c.scope == Scope::System)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mole_core::probes::StubProbes;
    use mole_core::providers::{DeniedPrivilegedRunner, TempDirTrash};

    /// Full engine round trip in a temp home: whitelist skip + trash + report.
    #[test]
    fn engine_trashes_selected_and_skips_whitelisted() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let victim = tmp.path().join("junk");
        let keep = tmp.path().join("keep");
        let config_dir = tmp
            .path()
            .join(".config")
            .join(mole_core::brand::CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("whitelist"),
            format!("{}\n", keep.display()),
        )
        .unwrap();
        std::fs::write(&victim, b"x").unwrap();
        std::fs::write(&keep, b"x").unwrap();

        // Redirect the shared logs into the temp home for the test.
        std::env::set_var("MOLE_DELETE_LOG", tmp.path().join("del.log"));
        std::env::set_var("MOLE_OPERATIONS_LOG", tmp.path().join("op.log"));

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let plan = DeletionPlan {
            candidates: vec![
                crate::scanutil::make_candidate(&victim, "test", Scope::User, &cancel).unwrap(),
                crate::scanutil::make_candidate(&keep, "test", Scope::User, &cancel).unwrap(),
            ],
        };
        let selection: Vec<String> = plan.candidates.iter().map(|c| c.id.clone()).collect();

        let trash = TempDirTrash {
            dir: tmp.path().join("Trash"),
        };
        let denied = DeniedPrivilegedRunner;
        let probes = StubProbes::idle();
        let providers = Providers {
            trash: &trash,
            privileged: &denied,
            probes: &probes,
        };
        let opts = ExecOptions {
            home: home.clone(),
            command: "clean".into(),
            mode: DeleteMode::Trash,
            dry_run: false,
            uninstall_mode: false,
        };
        let mut events = 0;
        let report = execute(&plan, &selection, &opts, &providers, |_| events += 1);

        std::env::remove_var("MOLE_DELETE_LOG");
        std::env::remove_var("MOLE_OPERATIONS_LOG");

        assert_eq!(events, 2);
        assert_eq!(report.skipped, 1);
        assert!(!victim.exists(), "selected item must be gone");
        assert!(keep.exists(), "whitelisted item must survive");
        let op = std::fs::read_to_string(tmp.path().join("op.log")).unwrap();
        assert!(op.contains("session started"));
        assert!(op.contains("SKIPPED"));
        assert!(op.contains("session ended"));
    }
}
