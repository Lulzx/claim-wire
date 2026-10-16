//! `cw-engine`: Topological evaluator, deadband/sentinel handler, context/edit conflict auditor, and atomic patcher.

pub mod diff_merge;
pub mod eval;
pub mod lockfile;
pub mod lower;
pub mod patch;

pub use diff_merge::{
    DiffCategory, DiffEntry, RevisionSnapshot, compare_snapshots, run_merge_driver,
};
pub use eval::{
    EvalChannel, EvaluatedRecord, EvaluationOutput, FamilyEvalContext,
    build_lockfile_claims_and_producers, eval_expr_in_channel, evaluate_all, json_to_canonical,
    make_family_claim_key, resolve_claim_ref_value, within_deadband,
};
pub use lockfile::{
    EvidenceLockfile, LockedClaimValue, LockedEpoch, LockedLocation, LockedProducer,
};
pub use lower::{
    BoundLocation, LocationKind, LoweringResult, audit_locations, build_locked_locations,
    discover_markdown_files, list_workspace_markdown, lower_workspace,
};
pub use patch::{
    AdoptReport, CheckReport, ImpactLocation, ImpactReport, PatchedItem, SyncReport,
    WorkspaceContext, WriteAheadJournal, commit_transactional_writes, load_workspace_pipeline,
    recover_staging_journal, run_adopt, run_check, run_check_files, run_explain, run_impact,
    run_sync, run_sync_options,
};
