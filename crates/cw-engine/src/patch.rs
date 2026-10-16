//! Crash-Safe Transactional Patcher (`.claimwire/.staging/journal.json`) and
//! High-Level Pipeline Operations (`check`, `sync`, `adopt`, `impact`, `explain`, `diff`, `merge-driver`).

use crate::eval::{
    EvalChannel, EvaluationOutput, build_lockfile_claims_and_producers, evaluate_all,
    make_family_claim_key,
};
use crate::lockfile::{EvidenceLockfile, LockedLocation};
use crate::lower::{
    BoundLocation, LoweringResult, audit_locations, build_locked_locations, lower_workspace,
};
use anyhow::{Context, Result, bail};
use cw_evidence::{EvidenceConfig, LoadedEvidenceStore, short_digest};
use cw_ir::{CheckedIr, ClaimsConfig, Expr, check_claims_config, parse_expr};
use cw_syntax::{
    ClaimRef, Diagnostic, DiagnosticCode, Severity, ValueType, YamlNode, apply_byte_patches,
    format_number_half_to_even, from_yaml_str,
};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteAheadJournal {
    pub staged_files: IndexMap<String, String>,
}

/// Recover any interrupted write-ahead journal in `.claimwire/.staging/journal.json`.
pub fn recover_staging_journal(workspace_root: &Path) -> Result<()> {
    let staging_dir = workspace_root.join(".claimwire/.staging");
    let journal_path = staging_dir.join("journal.json");
    if !journal_path.is_file() {
        return Ok(());
    }
    if let Ok(content) = fs::read_to_string(&journal_path) {
        if let Ok(journal) = serde_json::from_str::<WriteAheadJournal>(&content) {
            for (rel_target, staged_name) in &journal.staged_files {
                let staged_path = staging_dir.join(staged_name);
                let target_path = workspace_root.join(rel_target);
                if staged_path.is_file() {
                    if let Some(parent) = target_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&staged_path, &target_path);
                }
            }
        }
    }
    let _ = fs::remove_file(&journal_path);
    Ok(())
}

/// Commit a set of `(rel_path, new_content)` file writes atomically via `.claimwire/.staging/journal.json`
/// (Invariant 5: Crash-Safe Transactionality).
pub fn commit_transactional_writes(
    workspace_root: &Path,
    writes: &IndexMap<String, String>,
) -> Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    let staging_dir = workspace_root.join(".claimwire/.staging");
    fs::create_dir_all(&staging_dir)?;

    let mut staged_files = IndexMap::new();
    for (idx, (rel_path, content)) in writes.iter().enumerate() {
        let staged_name = format!("stage_{idx}.tmp");
        let staged_path = staging_dir.join(&staged_name);
        fs::write(&staged_path, content.as_bytes())?;
        staged_files.insert(rel_path.clone(), staged_name);
    }

    let journal = WriteAheadJournal {
        staged_files: staged_files.clone(),
    };
    let journal_path = staging_dir.join("journal.json");
    fs::write(&journal_path, serde_json::to_string_pretty(&journal)?)?;

    for (rel_path, staged_name) in &staged_files {
        let staged_path = staging_dir.join(staged_name);
        let target_path = workspace_root.join(rel_path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&staged_path, &target_path)?;
    }

    let _ = fs::remove_file(&journal_path);
    Ok(())
}

pub struct WorkspaceContext {
    pub ir: CheckedIr,
    pub evidence: LoadedEvidenceStore,
    pub lockfile: Option<EvidenceLockfile>,
    pub eval: EvaluationOutput,
    pub lowering: LoweringResult,
}

pub fn load_workspace_pipeline(
    workspace_root: &Path,
    exact: bool,
    explicit_files: Option<&[String]>,
) -> Result<(Option<WorkspaceContext>, Vec<Diagnostic>)> {
    recover_staging_journal(workspace_root)?;
    let mut diagnostics = Vec::new();

    let claims_path = workspace_root.join(".claimwire/claims.yaml");
    let evidence_path = workspace_root.join(".claimwire/evidence.yaml");

    let claims_src = fs::read_to_string(&claims_path)
        .with_context(|| format!("Failed to read `{}`", claims_path.display()))?;
    let evidence_src = fs::read_to_string(&evidence_path)
        .with_context(|| format!("Failed to read `{}`", evidence_path.display()))?;

    let (claims_cfg, claims_node) = from_yaml_str::<ClaimsConfig>(&claims_src)?;
    let (evidence_cfg, evidence_node) = from_yaml_str::<EvidenceConfig>(&evidence_src)?;
    let config_nodes = [
        (".claimwire/claims.yaml", &claims_node),
        (".claimwire/evidence.yaml", &evidence_node),
    ];

    let ir = match check_claims_config(&claims_cfg) {
        Ok(checked) => checked,
        Err(errs) => {
            diagnostics.extend(errs);
            resolve_config_locations(&mut diagnostics, &config_nodes);
            return Ok((None, diagnostics));
        }
    };

    let (evidence, ev_diags) = LoadedEvidenceStore::load(workspace_root, evidence_cfg)?;
    diagnostics.extend(ev_diags);

    let lockfile = EvidenceLockfile::load(workspace_root);
    let eval = evaluate_all(&ir, &evidence, lockfile.as_ref(), exact);
    diagnostics.extend(eval.diagnostics.iter().cloned());

    let lowering = lower_workspace(workspace_root, &ir, &eval.display, explicit_files)?;
    diagnostics.extend(lowering.diagnostics.iter().cloned());
    resolve_config_locations(&mut diagnostics, &config_nodes);

    Ok((
        Some(WorkspaceContext {
            ir,
            evidence,
            lockfile,
            eval,
            lowering,
        }),
        diagnostics,
    ))
}

/// The evidence a claim ultimately reads: `producer:<id>` for measured values and `const:<name>`
/// for declared constants, following `derive` expressions and family derivations transitively.
fn claim_feeds(claim_key: &str, ir: &CheckedIr) -> IndexSet<String> {
    let mut out = IndexSet::new();
    if let Ok(cref) = ClaimRef::parse(claim_key) {
        ref_feeds(&cref, ir, &mut out, &mut IndexSet::new());
    }
    out
}

fn expr_feeds(expr: &Expr, ir: &CheckedIr) -> IndexSet<String> {
    let mut out = IndexSet::new();
    walk_expr_feeds(expr, ir, None, &mut out, &mut IndexSet::new());
    out
}

fn ref_feeds(
    cref: &ClaimRef,
    ir: &CheckedIr,
    out: &mut IndexSet<String>,
    seen: &mut IndexSet<String>,
) {
    match cref {
        ClaimRef::Scalar(s) => {
            let name = s.canonical_string();
            if !seen.insert(name.clone()) {
                return;
            }
            if ir.config.constants.contains_key(&name) {
                out.insert(format!("const:{name}"));
            } else if let Some(decl) = ir.config.claims.get(&name) {
                if let Some(prod) = &decl.producer {
                    out.insert(format!("producer:{prod}"));
                }
                if let Some(expr) = decl.expr.as_deref().and_then(|e| parse_expr(e).ok()) {
                    walk_expr_feeds(&expr, ir, None, out, seen);
                }
            }
        }
        ClaimRef::Family(fref) => family_field_feeds(&fref.family, &fref.field, ir, out, seen),
    }
}

fn family_field_feeds(
    family: &str,
    field: &str,
    ir: &CheckedIr,
    out: &mut IndexSet<String>,
    seen: &mut IndexSet<String>,
) {
    let Some(fam) = ir.config.families.get(family) else {
        return;
    };
    out.insert(format!("producer:{}", fam.producer));
    if !seen.insert(format!("{family}.{field}")) {
        return;
    }
    if let Some(expr) = fam
        .derivations
        .get(field)
        .and_then(|d| parse_expr(&d.expr).ok())
    {
        walk_expr_feeds(&expr, ir, Some(family), out, seen);
    }
}

fn walk_expr_feeds(
    expr: &Expr,
    ir: &CheckedIr,
    family: Option<&str>,
    out: &mut IndexSet<String>,
    seen: &mut IndexSet<String>,
) {
    match expr {
        Expr::Ref(ClaimRef::Scalar(s)) => {
            // Inside a family derivation a bare name is a sibling metric or derivation.
            let name = s.canonical_string();
            match family.filter(|fam| {
                ir.config.families.get(*fam).is_some_and(|f| {
                    f.metrics.contains_key(&name) || f.derivations.contains_key(&name)
                })
            }) {
                Some(fam) => family_field_feeds(fam, &name, ir, out, seen),
                None => ref_feeds(&ClaimRef::Scalar(s.clone()), ir, out, seen),
            }
        }
        Expr::Ref(cref) => ref_feeds(cref, ir, out, seen),
        Expr::Reduction { target, .. } => {
            family_field_feeds(&target.family, &target.field, ir, out, seen)
        }
        Expr::EpochField { field, .. } | Expr::PrefixSum { field, .. } => {
            if let Some(fam) = family {
                family_field_feeds(fam, field, ir, out, seen);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr_feeds(lhs, ir, family, out, seen);
            walk_expr_feeds(rhs, ir, family, out, seen);
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            walk_expr_feeds(e, ir, family, out, seen)
        }
        Expr::Number(_) | Expr::Bool(_) => {}
    }
}

/// Lockfile locations whose Markdown binding disappeared, as `(location id, file)`.
/// Files that already carry an error (a broken lens, an unmatched block line) are skipped:
/// that error explains the missing locations and fixing it brings them back.
fn vanished_locations(
    lock: &EvidenceLockfile,
    lowering: &LoweringResult,
    explicit_files: Option<&[String]>,
    diagnostics: &[Diagnostic],
) -> Vec<(String, String)> {
    let present: IndexSet<&str> = lowering.locations.iter().map(|l| l.id.as_str()).collect();
    // A lens error (moved or deleted directive) already accounts for that lens's locations.
    let errored_lenses: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.code == DiagnosticCode::CW2003)
        .map(|d| d.message.as_str())
        .collect();
    let errored_files: IndexSet<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .filter_map(|d| d.file.as_deref())
        .collect();
    lock.locations
        .keys()
        .filter(|id| !present.contains(id.as_str()))
        .filter_map(|id| {
            let file = id.split("::").next()?.to_string();
            let lens = id
                .split("::")
                .nth(1)
                .and_then(|k| k.split_once(':'))
                .map(|(_, l)| l);
            if lens.is_some_and(|l| {
                errored_lenses
                    .iter()
                    .any(|m| m.contains(&format!("lens `{l}`")))
            }) {
                return None;
            }
            let in_scope = explicit_files.is_none_or(|fs| fs.iter().any(|f| f == &file));
            (in_scope && !errored_files.contains(file.as_str())).then(|| (id.clone(), file))
        })
        .collect()
}

/// Point diagnostics that carry a YAML key path at that key's line instead of `1:1`.
fn resolve_config_locations(diagnostics: &mut [Diagnostic], nodes: &[(&str, &YamlNode)]) {
    for d in diagnostics.iter_mut() {
        let Some(path) = &d.config_path else { continue };
        let Some((_, node)) = nodes.iter().find(|(f, _)| d.file.as_deref() == Some(*f)) else {
            continue;
        };
        let segs: Vec<&str> = path.iter().map(String::as_str).collect();
        if let Some(span) = node.get_path_span(&segs) {
            d.line = Some(span.line);
            d.col = Some(span.col);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub diagnostics: Vec<Diagnostic>,
    pub total_locations: usize,
    pub failed_locations: usize,
}

impl CheckReport {
    pub fn has_errors(&self, deny_warnings: bool) -> bool {
        self.diagnostics.iter().any(|d| {
            d.severity == Severity::Error || (deny_warnings && d.severity == Severity::Warning)
        })
    }
}

pub fn run_check(workspace_root: &Path) -> Result<CheckReport> {
    run_check_files(workspace_root, None)
}

pub fn run_check_files(
    workspace_root: &Path,
    explicit_files: Option<&[String]>,
) -> Result<CheckReport> {
    let (ctx_opt, mut diagnostics) =
        load_workspace_pipeline(workspace_root, false, explicit_files)?;
    let Some(ctx) = ctx_opt else {
        return Ok(CheckReport {
            failed_locations: 0,
            diagnostics,
            total_locations: 0,
        });
    };

    let audit_diags = audit_locations(
        &ctx.lowering.locations,
        ctx.lockfile.as_ref(),
        &ctx.ir,
        &ctx.evidence,
        true,
        false,
        false,
    );
    // One location can carry both CW4001 and CW4002; count locations, not diagnostics.
    let failed_locations = audit_diags
        .iter()
        .map(|d| (d.file.clone(), d.line, d.col))
        .collect::<IndexSet<_>>()
        .len();
    diagnostics.extend(audit_diags);
    if let Some(lock) = &ctx.lockfile {
        let vanished = vanished_locations(lock, &ctx.lowering, explicit_files, &diagnostics);
        diagnostics.extend(vanished.into_iter().map(|(id, file)| {
            Diagnostic::error(
                DiagnosticCode::CW4003,
                format!("Locked location `{id}` is no longer present in Markdown"),
            )
            .with_location(file, 1, 1)
            .with_help("restore the binding, or run `cw sync` to prune it from `.claimwire/evidence.lock.json` and commit the removal")
        }));
    }

    Ok(CheckReport {
        diagnostics,
        total_locations: ctx.lowering.locations.len(),
        failed_locations,
    })
}

#[derive(Debug, Clone)]
pub struct PatchedItem {
    pub file: String,
    pub line: usize,
    pub claim_summary: String,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub diagnostics: Vec<Diagnostic>,
    pub patched_items: Vec<PatchedItem>,
    pub files_written: usize,
    pub lockfile_updated: bool,
    /// Lockfile locations dropped because their Markdown binding no longer exists.
    pub pruned_locations: Vec<String>,
    /// Out-of-sync locations not patched because a failing assertion reads the same evidence.
    pub held_locations: Vec<String>,
    /// Map of `rel_file -> (old_source, new_source)` for changed Markdown files.
    pub file_diffs: IndexMap<String, (String, String)>,
}

pub fn run_sync(
    workspace_root: &Path,
    exact: bool,
    force: bool,
    ack_context: bool,
) -> Result<SyncReport> {
    run_sync_options(workspace_root, exact, force, ack_context, false)
}

pub fn run_sync_options(
    workspace_root: &Path,
    exact: bool,
    force: bool,
    ack_context: bool,
    dry_run: bool,
) -> Result<SyncReport> {
    let (ctx_opt, mut diagnostics) = load_workspace_pipeline(workspace_root, exact, None)?;
    let Some(ctx) = ctx_opt else {
        return Ok(SyncReport {
            diagnostics,
            patched_items: Vec::new(),
            files_written: 0,
            lockfile_updated: false,
            pruned_locations: Vec::new(),
            held_locations: Vec::new(),
            file_diffs: IndexMap::new(),
        });
    };

    // Audit for manual edits (CW4001) and context drift (CW4002), without emitting CW3003
    let audit_diags = audit_locations(
        &ctx.lowering.locations,
        ctx.lockfile.as_ref(),
        &ctx.ir,
        &ctx.evidence,
        false,
        force,
        ack_context,
    );
    diagnostics.extend(audit_diags);

    // Any error other than a failed assertion (CW4001 manual edit, CW4002 context drift, CW3001
    // stale evidence, ...) aborts the sync without modifying any files. A failed assertion (CW3002)
    // only holds back the locations fed by the same evidence (§4.2: "dependent bindings"), so an
    // unrelated regression does not freeze every other number in the repository.
    if diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error && d.code != DiagnosticCode::CW3002)
    {
        return Ok(SyncReport {
            diagnostics,
            patched_items: Vec::new(),
            files_written: 0,
            lockfile_updated: false,
            pruned_locations: Vec::new(),
            held_locations: Vec::new(),
            file_diffs: IndexMap::new(),
        });
    }

    let held_feeds: IndexSet<String> = ctx
        .eval
        .assertions
        .iter()
        .filter(|(_, passed)| !**passed)
        .filter_map(|(name, _)| ctx.ir.config.assertions.get(name))
        .filter_map(|a| parse_expr(&a.expr).ok())
        .flat_map(|e| expr_feeds(&e, &ctx.ir))
        .collect();
    let held_ids: IndexSet<&str> = if held_feeds.is_empty() {
        IndexSet::new()
    } else {
        ctx.lowering
            .locations
            .iter()
            .filter(|l| {
                l.referenced_claims
                    .iter()
                    .any(|c| !claim_feeds(c, &ctx.ir).is_disjoint(&held_feeds))
            })
            .map(|l| l.id.as_str())
            .collect()
    };

    // Group patches per Markdown file
    let mut patches_by_file: IndexMap<String, Vec<(cw_syntax::ByteRange, String)>> =
        IndexMap::new();
    let mut patched_items = Vec::new();
    let mut held_locations = Vec::new();

    for loc in &ctx.lowering.locations {
        if loc.current_text != loc.expected_text && held_ids.contains(loc.id.as_str()) {
            held_locations.push(loc.id.clone());
            continue;
        }
        if loc.current_text != loc.expected_text {
            patches_by_file
                .entry(loc.file.clone())
                .or_default()
                .push((loc.byte_range.clone(), loc.expected_text.clone()));
            let claim_summary = loc
                .referenced_claims
                .first()
                .cloned()
                .unwrap_or_else(|| loc.id.clone());
            patched_items.push(PatchedItem {
                file: loc.file.clone(),
                line: loc.line,
                claim_summary,
                old_text: loc.current_text.clone(),
                new_text: loc.expected_text.clone(),
            });
        }
    }

    let mut writes: IndexMap<String, String> = IndexMap::new();
    let mut file_diffs: IndexMap<String, (String, String)> = IndexMap::new();
    for (file, file_patches) in patches_by_file {
        if let Some(doc) = ctx.lowering.documents.get(&file) {
            let new_source = apply_byte_patches(&doc.source, file_patches);
            if new_source != doc.source {
                file_diffs.insert(file.clone(), (doc.source.clone(), new_source.clone()));
                writes.insert(file, new_source);
            }
        }
    }
    let md_files_written = writes.len();

    // Build updated lockfile. Held locations, and the evidence behind them, keep their locked
    // state so the next `cw check` still reports them until the assertion passes.
    let (mut producers, mut evaluated_claims) =
        build_lockfile_claims_and_producers(&ctx.eval.display, &ctx.evidence);
    let mut locations = build_locked_locations(&ctx.lowering.locations, true);
    if !held_feeds.is_empty() {
        let old = ctx.lockfile.clone().unwrap_or_default();
        let held_locs: Vec<BoundLocation> = ctx
            .lowering
            .locations
            .iter()
            .filter(|l| held_ids.contains(l.id.as_str()))
            .cloned()
            .collect();
        let as_found = build_locked_locations(&held_locs, false);
        for (id, found) in as_found {
            let kept = old.locations.get(&id).cloned().unwrap_or(found);
            locations.insert(id, kept);
        }
        for (prod_id, entry) in producers.iter_mut() {
            if held_feeds.contains(&format!("producer:{prod_id}")) {
                if let Some(prev) = old.producers.get(prod_id) {
                    *entry = prev.clone();
                }
            }
        }
        let held_claim = |key: &str| !claim_feeds(key, &ctx.ir).is_disjoint(&held_feeds);
        evaluated_claims
            .retain(|key, _| !held_claim(key) || old.evaluated_claims.contains_key(key));
        for (key, entry) in evaluated_claims.iter_mut() {
            if held_claim(key) {
                *entry = old.evaluated_claims[key].clone();
            }
        }
    }
    let pruned_locations: Vec<String> = ctx
        .lockfile
        .as_ref()
        .map(|l| {
            l.locations
                .keys()
                .filter(|k| !locations.contains_key(*k))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let new_lockfile = EvidenceLockfile {
        schema: "claimwire-lock/v3".to_string(),
        producers,
        evaluated_claims,
        locations,
    };

    let lockfile_changed = match &ctx.lockfile {
        Some(existing) => existing != &new_lockfile,
        None => true,
    };

    // Invariant 1 (Idempotence): Running `cw sync` twice on unchanged inputs writes zero bytes!
    if !dry_run && (md_files_written > 0 || lockfile_changed) {
        writes.insert(
            ".claimwire/evidence.lock.json".to_string(),
            new_lockfile.to_pretty_json()?,
        );
        commit_transactional_writes(workspace_root, &writes)?;
    }

    Ok(SyncReport {
        diagnostics,
        patched_items,
        files_written: md_files_written,
        lockfile_updated: md_files_written > 0 || lockfile_changed,
        pruned_locations,
        held_locations,
        file_diffs,
    })
}

#[derive(Debug, Clone)]
pub struct AdoptReport {
    pub diagnostics: Vec<Diagnostic>,
    pub adopted_locations: usize,
}

/// `cw adopt [<file...>]`: Reverse-parses existing Markdown spans, table cells, and block lenses
/// using bidirectional format strings to initialize `.claimwire/evidence.lock.json` without
/// modifying any Markdown bytes (`spec.md` §9.2 & §10.2 Point 1).
pub fn run_adopt(workspace_root: &Path, explicit_files: Option<&[String]>) -> Result<AdoptReport> {
    let (ctx_opt, diagnostics) = load_workspace_pipeline(workspace_root, true, explicit_files)?;
    let Some(mut ctx) = ctx_opt else {
        return Ok(AdoptReport {
            diagnostics,
            adopted_locations: 0,
        });
    };

    let mut writes = IndexMap::new();

    // If any `@current` manifest has a bootstrap placeholder digest (`blake3:00000000...`),
    // stamp the working tree's semantic `input_set_digest` and `input_files` into it.
    for (prod_id, prod_decl) in &ctx.evidence.config.producers {
        if let Some(run_id) = prod_decl.epochs.get("current") {
            let Some(tree_digest) = ctx.evidence.current_tree_digests.get(prod_id).cloned() else {
                continue;
            };
            let file_digests = ctx
                .evidence
                .producer_file_digests
                .get(prod_id)
                .cloned()
                .unwrap_or_default();

            if let Some(ep_map) = ctx.evidence.manifests.get_mut(prod_id) {
                if let Some(manifest) = ep_map.get_mut("current") {
                    if manifest.input_set_digest.starts_with("blake3:00000000")
                        || manifest.input_files.is_empty()
                    {
                        manifest.input_set_digest = tree_digest;
                        manifest.input_files = file_digests;
                        let rel_path = format!(".claimwire/runs/{prod_id}/{run_id}.json");
                        writes.insert(rel_path, serde_json::to_string_pretty(manifest)? + "\n");
                    }
                }
            }
        }
    }

    let (producers, evaluated_claims) =
        build_lockfile_claims_and_producers(&ctx.eval.fresh, &ctx.evidence);

    // Record the existing Markdown bytes (`use_expected_text = false`) in `locations`,
    // so zero Markdown bytes are touched while `cw check` can immediately compare Markdown against evidence!
    // Entries for re-adopted files are rebuilt from scratch so bindings removed since the last
    // adopt don't linger; with an explicit file list, other files' entries are kept.
    let adopted_files: IndexSet<String> = ctx.lowering.documents.keys().cloned().collect();
    let mut locations: IndexMap<String, LockedLocation> = ctx
        .lockfile
        .map(|l| l.locations)
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| {
            let file = k.split("::").next().unwrap_or_default();
            explicit_files.is_some() && !adopted_files.contains(file)
        })
        .collect();
    for (k, v) in build_locked_locations(&ctx.lowering.locations, false) {
        locations.insert(k, v);
    }

    let adopted_locations = ctx.lowering.locations.len();
    let new_lockfile = EvidenceLockfile {
        schema: "claimwire-lock/v3".to_string(),
        producers,
        evaluated_claims,
        locations,
    };

    writes.insert(
        ".claimwire/evidence.lock.json".to_string(),
        new_lockfile.to_pretty_json()?,
    );
    commit_transactional_writes(workspace_root, &writes)?;

    Ok(AdoptReport {
        diagnostics,
        adopted_locations,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactReport {
    pub modified_files: Vec<String>,
    pub stale_producers: Vec<String>,
    pub affected_families: Vec<String>,
    pub affected_scalar_claims: Vec<String>,
    pub affected_assertions: Vec<String>,
    pub affected_locations: Vec<ImpactLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactLocation {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub id: String,
    pub description: String,
}

/// `cw impact [<path...> | --git-diff <rev>]`: Traces modified source files forward through
/// the invalidation graph to list affected producers, claims, and exact `file:line` Markdown locations.
pub fn run_impact(workspace_root: &Path, paths: &[String]) -> Result<ImpactReport> {
    let (ctx_opt, _) = load_workspace_pipeline(workspace_root, false, None)?;
    let Some(ctx) = ctx_opt else {
        bail!("Workspace failed to load for impact analysis");
    };

    let mut modified_files: Vec<String> = paths
        .iter()
        .map(|p| p.trim_start_matches("./").to_string())
        .collect();

    // If no explicit paths were passed, detect any producer whose working-tree digest differs from `@current`
    let mut stale_producers = IndexSet::new();
    if modified_files.is_empty() {
        for (prod_id, is_fresh) in &ctx.evidence.freshness {
            if !*is_fresh {
                stale_producers.insert(prod_id.clone());
                if let Some(manifest) = ctx.evidence.get_manifest(prod_id, "current") {
                    if let Some(prod_files) = ctx.evidence.producer_input_files.get(prod_id) {
                        let (mut fp_cache, cache_mtime) =
                            cw_evidence::FingerprintCache::load(workspace_root);
                        for rf in prod_files {
                            if let Ok((cur_d, _)) =
                                fp_cache.fingerprint_file(workspace_root, rf, cache_mtime)
                            {
                                if manifest.input_files.get(rf) != Some(&cur_d)
                                    && !modified_files.contains(rf)
                                {
                                    modified_files.push(rf.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        for (prod_id, prod_files) in &ctx.evidence.producer_input_files {
            if modified_files.iter().any(|mf| prod_files.contains(mf)) {
                stale_producers.insert(prod_id.clone());
            }
        }
    }

    let mut affected_families = IndexSet::new();
    let mut affected_scalar_claims = IndexSet::new();

    for (fam_name, fam_decl) in &ctx.ir.config.families {
        if stale_producers.contains(&fam_decl.producer) {
            affected_families.insert(fam_name.clone());
        }
    }
    for (c_name, c_decl) in &ctx.ir.config.claims {
        if let Some(prod) = &c_decl.producer {
            if stale_producers.contains(prod) {
                affected_scalar_claims.insert(c_name.clone());
            }
        }
        if let Some(expr_str) = &c_decl.expr {
            if let Ok(expr) = parse_expr(expr_str) {
                if expr_touches_affected(&expr, &affected_families, &affected_scalar_claims) {
                    affected_scalar_claims.insert(c_name.clone());
                }
            }
        }
    }

    let mut affected_assertions = IndexSet::new();
    for (a_name, a_decl) in &ctx.ir.config.assertions {
        if let Ok(expr) = parse_expr(&a_decl.expr) {
            if expr_touches_affected(&expr, &affected_families, &affected_scalar_claims) {
                affected_assertions.insert(a_name.clone());
            }
        }
    }

    let mut affected_locations = Vec::new();
    for loc in &ctx.lowering.locations {
        let touches = loc.referenced_claims.iter().any(|c_str| {
            if affected_scalar_claims.contains(c_str) {
                return true;
            }
            if let Ok(ClaimRef::Family(fref)) = ClaimRef::parse(c_str) {
                if fref.effective_epoch() == "current" && affected_families.contains(&fref.family) {
                    return true;
                }
            }
            false
        });
        if touches {
            affected_locations.push(ImpactLocation {
                file: loc.file.clone(),
                line: loc.line,
                col: loc.col,
                id: loc.id.clone(),
                description: loc.kind.short_description(),
            });
        }
    }

    Ok(ImpactReport {
        modified_files,
        stale_producers: stale_producers.into_iter().collect(),
        affected_families: affected_families.into_iter().collect(),
        affected_scalar_claims: affected_scalar_claims.into_iter().collect(),
        affected_assertions: affected_assertions.into_iter().collect(),
        affected_locations,
    })
}

fn expr_touches_affected(
    expr: &Expr,
    affected_families: &IndexSet<String>,
    affected_scalars: &IndexSet<String>,
) -> bool {
    match expr {
        Expr::Ref(ClaimRef::Scalar(s)) => affected_scalars.contains(&s.canonical_string()),
        Expr::Ref(ClaimRef::Family(f)) => {
            f.effective_epoch() == "current" && affected_families.contains(&f.family)
        }
        Expr::Reduction { target, .. } => {
            target.effective_epoch() == "current" && affected_families.contains(&target.family)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_touches_affected(lhs, affected_families, affected_scalars)
                || expr_touches_affected(rhs, affected_families, affected_scalars)
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            expr_touches_affected(e, affected_families, affected_scalars)
        }
        _ => false,
    }
}

/// `cw explain <location-or-claim>`: Prints the full provenance tree for any rendered value (`spec.md` §10.2 Point 10).
pub fn run_explain(workspace_root: &Path, query: &str) -> Result<String> {
    let (ctx_opt, _) = load_workspace_pipeline(workspace_root, false, None)?;
    let Some(ctx) = ctx_opt else {
        bail!("Workspace failed to load for explain");
    };

    // 1. Check if `query` is `<file>:<line>` or `<file>:<line>:<col>`
    if let Some((file_part, rest)) = query.split_once(':') {
        let line_str = rest.split(':').next().unwrap_or(rest);
        if let Ok(line_no) = line_str.parse::<usize>() {
            let matching_locs: Vec<&BoundLocation> = ctx
                .lowering
                .locations
                .iter()
                .filter(|l| l.file == file_part && l.line == line_no)
                .collect();
            if !matching_locs.is_empty() {
                let mut out = String::new();
                for (idx, loc) in matching_locs.iter().enumerate() {
                    if idx > 0 {
                        out.push('\n');
                    }
                    out.push_str(&format!(
                        "{}:{}  \"{}\"\n",
                        loc.file, loc.line, loc.current_text
                    ));
                    explain_location_claims(&mut out, loc, &ctx);
                }
                return Ok(out);
            }
            if ctx.lowering.documents.contains_key(file_part) {
                bail!(
                    "No Claim Wire location on `{file_part}:{line_no}` (try `cw list locations`)"
                );
            }
        }
    }

    // 2. Otherwise treat `query` as a claim reference or location ID
    if let Some(loc) = ctx.lowering.locations.iter().find(|l| l.id == query) {
        let mut out = format!("{}:{}  \"{}\"\n", loc.file, loc.line, loc.current_text);
        explain_location_claims(&mut out, loc, &ctx);
        return Ok(out);
    }

    let cref = ClaimRef::parse(query)
        .map_err(|d| anyhow::anyhow!("Invalid claim or location `{query}`: {}", d.message))?;
    cw_ir::resolve_claim_ref_type(&cref, &ctx.ir.config, None, false).map_err(|d| {
        anyhow::anyhow!(
            "Unknown claim `{query}`: {} (try `cw list claims`)",
            d.message
        )
    })?;
    let canon = cref.canonical_string();
    let mut out = String::new();
    explain_claim_tree(&mut out, &canon, "└─", "   ", &ctx);
    Ok(out)
}

/// Explain each distinct claim a location renders (a block line may repeat `matrix.total`).
fn explain_location_claims(out: &mut String, loc: &BoundLocation, ctx: &WorkspaceContext) {
    let claims: IndexSet<&String> = loc.referenced_claims.iter().collect();
    for (c_idx, claim_key) in claims.iter().enumerate() {
        let is_last = c_idx + 1 == claims.len();
        let branch = if is_last { "└─" } else { "├─" };
        let indent = if is_last { "   " } else { "│  " };
        explain_claim_tree(out, claim_key, branch, indent, ctx);
    }
}

fn explain_claim_tree(
    out: &mut String,
    claim_key: &str,
    branch: &str,
    child_indent: &str,
    ctx: &WorkspaceContext,
) {
    let Ok(cref) = ClaimRef::parse(claim_key) else {
        out.push_str(&format!("{branch} {claim_key}\n"));
        return;
    };

    match cref {
        ClaimRef::Family(fref) => {
            let Some(fam_decl) = ctx.ir.config.families.get(&fref.family) else {
                return;
            };
            let epoch = fref.effective_epoch();
            if let Some(deriv_decl) = fam_decl.derivations.get(&fref.field) {
                out.push_str(&format!("{branch} {claim_key} = {}\n", deriv_decl.expr));
                // Find leaf dependencies of `deriv_decl.expr`
                let mut scope_map = IndexMap::new();
                for (dim, sel) in fam_decl.dimensions.iter().zip(fref.selectors.iter()) {
                    if let cw_syntax::Selector::Point(k) = sel {
                        scope_map.insert(dim.clone(), k.value.clone());
                    }
                }
                if let Ok(expr) = parse_expr(&deriv_decl.expr) {
                    let mut leaves = Vec::new();
                    collect_explain_leaves(
                        &expr,
                        &fref.family,
                        fam_decl,
                        epoch,
                        &scope_map,
                        &mut leaves,
                    );
                    for (i, (leaf_label, leaf_canon, leaf_prod, leaf_ep)) in
                        leaves.iter().enumerate()
                    {
                        let is_last = i + 1 == leaves.len();
                        let sub_branch = if is_last { "└─" } else { "├─" };
                        let val_str =
                            format_claim_human_value(leaf_canon, &ctx.eval.display, &ctx.ir);
                        if !leaf_prod.is_empty() {
                            out.push_str(&format!(
                                "{child_indent}{sub_branch} {leaf_label:<10} = {val_str:<11} ← {leaf_prod}@{leaf_ep}\n"
                            ));
                        } else {
                            out.push_str(&format!(
                                "{child_indent}{sub_branch} {leaf_label:<10} = {val_str}\n"
                            ));
                        }
                        if is_last && !leaf_prod.is_empty() {
                            append_manifest_provenance(
                                out,
                                &format!("{child_indent}   "),
                                leaf_prod,
                                leaf_ep,
                                &ctx.evidence,
                            );
                        }
                    }
                }
            } else if fam_decl.metrics.contains_key(&fref.field) {
                let val_str = format_claim_human_value(claim_key, &ctx.eval.display, &ctx.ir);
                let prod = &fam_decl.producer;
                out.push_str(&format!(
                    "{branch} {claim_key} = {val_str}   ← {prod}@{epoch}\n"
                ));
                append_manifest_provenance(out, child_indent, prod, epoch, &ctx.evidence);
            }
        }
        ClaimRef::Scalar(sref) => {
            let name = sref.canonical_string();
            let val_str = format_claim_human_value(&name, &ctx.eval.display, &ctx.ir);
            if let Some(c_decl) = ctx.ir.config.claims.get(&name) {
                if let Some(prod) = &c_decl.producer {
                    out.push_str(&format!("{branch} {name} = {val_str}   ← {prod}@current\n"));
                    append_manifest_provenance(out, child_indent, prod, "current", &ctx.evidence);
                    return;
                }
            }
            out.push_str(&format!("{branch} {name} = {val_str}\n"));
        }
    }
}

fn collect_explain_leaves(
    expr: &Expr,
    fam_name: &str,
    fam_decl: &cw_ir::FamilyDecl,
    epoch: &str,
    scope: &IndexMap<String, String>,
    out: &mut Vec<(String, String, String, String)>,
) {
    match expr {
        Expr::Ref(ClaimRef::Scalar(s)) if s.segments.len() == 1 => {
            let field = &s.segments[0];
            let canon = make_family_claim_key(fam_name, epoch, &fam_decl.dimensions, scope, field);
            out.push((
                field.clone(),
                canon,
                fam_decl.producer.clone(),
                epoch.to_string(),
            ));
        }
        Expr::Ref(ClaimRef::Scalar(s)) => {
            let name = s.canonical_string();
            out.push((name.clone(), name, String::new(), String::new()));
        }
        Expr::Ref(ClaimRef::Family(fref)) => {
            let ep = fref.effective_epoch().to_string();
            let mut target_scope = IndexMap::new();
            for (dim, sel) in fam_decl.dimensions.iter().zip(fref.selectors.iter()) {
                if let cw_syntax::Selector::Point(k) = sel {
                    let v = if !k.was_quoted {
                        scope
                            .get(&k.value)
                            .cloned()
                            .unwrap_or_else(|| k.value.clone())
                    } else {
                        k.value.clone()
                    };
                    target_scope.insert(dim.clone(), v);
                }
            }
            let canon = make_family_claim_key(
                &fref.family,
                &ep,
                &fam_decl.dimensions,
                &target_scope,
                &fref.field,
            );
            out.push((canon.clone(), canon, fam_decl.producer.clone(), ep));
        }
        Expr::Reduction { op, target } => {
            let label = format!("{op:?}({target})").to_lowercase();
            out.push((
                label.clone(),
                label,
                fam_decl.producer.clone(),
                target.effective_epoch().to_string(),
            ));
        }
        Expr::PrefixSum { field, .. } => {
            let canon = make_family_claim_key(fam_name, epoch, &fam_decl.dimensions, scope, field);
            out.push((
                field.clone(),
                canon,
                fam_decl.producer.clone(),
                epoch.to_string(),
            ));
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_explain_leaves(lhs, fam_name, fam_decl, epoch, scope, out);
            collect_explain_leaves(rhs, fam_name, fam_decl, epoch, scope, out);
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            collect_explain_leaves(e, fam_name, fam_decl, epoch, scope, out);
        }
        _ => {}
    }
}

fn format_claim_human_value(claim_key: &str, channel: &EvalChannel, _ir: &CheckedIr) -> String {
    let Some((val, ty, _)) = channel.flat_claims.get(claim_key) else {
        return "—".to_string();
    };
    let Some(n) = val.as_f64() else {
        return format!("{val:?}");
    };
    match ty {
        ValueType::Cycles => format_number_half_to_even(n, Some(0), true, None),
        ValueType::Duration => {
            let secs = n / 1e9;
            // Print unrounded canonical seconds up to 4 decimals (trimming trailing zeros)
            let s = format!("{secs:.4}");
            let trimmed = s.trim_end_matches('0').trim_end_matches('.');
            format!("{trimmed} s")
        }
        ValueType::Bytes => format!("{} B", format_number_half_to_even(n, Some(0), true, None)),
        ValueType::Count => format_number_half_to_even(n, Some(0), true, None),
        ValueType::Quantity(u) => format!("{n} {u}"),
        _ => format!("{n:.2}"),
    }
}

fn append_manifest_provenance(
    out: &mut String,
    indent: &str,
    producer: &str,
    epoch: &str,
    evidence: &LoadedEvidenceStore,
) {
    let Some(manifest) = evidence.get_manifest(producer, epoch) else {
        return;
    };
    let hw = manifest
        .hardware
        .as_deref()
        .map(|h| format!("  ({h})"))
        .unwrap_or_default();
    let dirty_label = if manifest.git_dirty { "dirty" } else { "clean" };
    let fresh_label = if epoch == "current" {
        if *evidence.freshness.get(producer).unwrap_or(&true) {
            "fresh"
        } else {
            "stale"
        }
    } else {
        "pinned"
    };
    out.push_str(&format!("{indent}run     {}{hw}\n", manifest.run_id));
    out.push_str(&format!(
        "{indent}commit  {}  ({dirty_label})\n",
        manifest.git_commit
    ));
    out.push_str(&format!(
        "{indent}inputs  {}  {fresh_label}\n",
        short_digest(&manifest.input_set_digest)
    ));
}
