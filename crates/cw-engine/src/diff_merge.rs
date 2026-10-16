//! `cw diff` (semantic claim diffing across revisions) and `cw merge-driver` (`spec.md` §4.4, §9.2).

use crate::lockfile::EvidenceLockfile;
use anyhow::{Context, Result};
use cw_evidence::{EvidenceConfig, RunManifest};
use cw_ir::ClaimsConfig;
use cw_syntax::{ByteRange, MarkdownDocument, apply_byte_patches, from_yaml_str};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffCategory {
    ValueChanged,
    DefinitionChanged,
    ScopeChanged,
    EvidenceRefreshed,
    AssertionRegressed,
}

impl std::fmt::Display for DiffCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ValueChanged => f.write_str("Value Changed"),
            Self::DefinitionChanged => f.write_str("Definition Changed"),
            Self::ScopeChanged => f.write_str("Scope Changed"),
            Self::EvidenceRefreshed => f.write_str("Evidence Refreshed"),
            Self::AssertionRegressed => f.write_str("Assertion Regressed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub category: DiffCategory,
    pub subject: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct RevisionSnapshot {
    pub claims_config: Option<ClaimsConfig>,
    pub evidence_config: Option<EvidenceConfig>,
    pub lockfile: Option<EvidenceLockfile>,
    pub manifests: IndexMap<String, RunManifest>,
}

impl RevisionSnapshot {
    pub fn from_dir(dir: &Path) -> Self {
        let claims_config = fs::read_to_string(dir.join(".claimwire/claims.yaml"))
            .ok()
            .and_then(|s| from_yaml_str::<ClaimsConfig>(&s).ok().map(|(c, _)| c));
        let evidence_config = fs::read_to_string(dir.join(".claimwire/evidence.yaml"))
            .ok()
            .and_then(|s| from_yaml_str::<EvidenceConfig>(&s).ok().map(|(c, _)| c));
        let lockfile = EvidenceLockfile::load(dir);

        let mut manifests = IndexMap::new();
        if let Some(ev) = &evidence_config {
            for (prod_id, prod_decl) in &ev.producers {
                if let Some(run_id) = prod_decl.epochs.get("current") {
                    let p = dir
                        .join(".claimwire/runs")
                        .join(prod_id)
                        .join(format!("{run_id}.json"));
                    if let Ok(s) = fs::read_to_string(p) {
                        if let Ok(mut m) = serde_json::from_str::<RunManifest>(&s) {
                            m.normalize();
                            manifests.insert(prod_id.clone(), m);
                        }
                    }
                }
            }
        }
        Self {
            claims_config,
            evidence_config,
            lockfile,
            manifests,
        }
    }

    pub fn from_git_rev(workspace_root: &Path, rev: &str) -> Result<Self> {
        if rev == "WORKDIR" || rev == "." {
            return Ok(Self::from_dir(workspace_root));
        }
        let claims_config = git_show_file(workspace_root, rev, ".claimwire/claims.yaml")
            .and_then(|s| from_yaml_str::<ClaimsConfig>(&s).ok().map(|(c, _)| c));
        let evidence_config = git_show_file(workspace_root, rev, ".claimwire/evidence.yaml")
            .and_then(|s| from_yaml_str::<EvidenceConfig>(&s).ok().map(|(c, _)| c));
        let lockfile = git_show_file(workspace_root, rev, ".claimwire/evidence.lock.json")
            .and_then(|s| serde_json::from_str::<EvidenceLockfile>(&s).ok());

        let mut manifests = IndexMap::new();
        if let Some(ev) = &evidence_config {
            for (prod_id, prod_decl) in &ev.producers {
                if let Some(run_id) = prod_decl.epochs.get("current") {
                    let rel = format!(".claimwire/runs/{prod_id}/{run_id}.json");
                    if let Some(s) = git_show_file(workspace_root, rev, &rel) {
                        if let Ok(mut m) = serde_json::from_str::<RunManifest>(&s) {
                            m.normalize();
                            manifests.insert(prod_id.clone(), m);
                        }
                    }
                }
            }
        }
        Ok(Self {
            claims_config,
            evidence_config,
            lockfile,
            manifests,
        })
    }
}

fn git_show_file(workspace_root: &Path, rev: &str, rel_path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["show", &format!("{rev}:{rel_path}")])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

pub fn compare_snapshots(old: &RevisionSnapshot, new: &RevisionSnapshot) -> Vec<DiffEntry> {
    let mut entries = Vec::new();

    // 1. Check Definition Changed & Scope Changed in `claims.yaml` and record order for `prefix_sum` (§4.4)
    if let (Some(old_c), Some(new_c)) = (&old.claims_config, &new.claims_config) {
        for (fam_name, new_fam) in &new_c.families {
            if let Some(old_fam) = old_c.families.get(fam_name) {
                if old_fam.dimensions != new_fam.dimensions {
                    entries.push(DiffEntry {
                        category: DiffCategory::ScopeChanged,
                        subject: fam_name.clone(),
                        detail: format!(
                            "dimensions {:?} → {:?}",
                            old_fam.dimensions, new_fam.dimensions
                        ),
                    });
                }
                for (d_name, new_d) in &new_fam.derivations {
                    if let Some(old_d) = old_fam.derivations.get(d_name) {
                        if old_d.expr != new_d.expr || old_d.value_type != new_d.value_type {
                            entries.push(DiffEntry {
                                category: DiffCategory::DefinitionChanged,
                                subject: format!("{fam_name}.{d_name}"),
                                detail: format!("`{}` → `{}`", old_d.expr, new_d.expr),
                            });
                        }
                    } else {
                        entries.push(DiffEntry {
                            category: DiffCategory::DefinitionChanged,
                            subject: format!("{fam_name}.{d_name}"),
                            detail: format!("added derivation `{}`", new_d.expr),
                        });
                    }
                }
            } else {
                entries.push(DiffEntry {
                    category: DiffCategory::ScopeChanged,
                    subject: fam_name.clone(),
                    detail: "added claim family".to_string(),
                });
            }
        }

        // §4.4: "Because record order is semantic for `prefix_sum`, a run whose record order differs from the epoch it replaces
        // is reported as a Definition Changed event by `cw diff`, never silently re-summed."
        for (fam_name, new_fam) in &new_c.families {
            let uses_prefix_sum = new_fam
                .derivations
                .values()
                .any(|d| d.expr.contains("prefix_sum("));
            if let (Some(old_m), Some(new_m)) = (
                old.manifests.get(&new_fam.producer),
                new.manifests.get(&new_fam.producer),
            ) {
                if let (Some(old_recs), Some(new_recs)) = (
                    old_m.families.get(fam_name),
                    new_m.families.get(fam_name),
                ) {
                    let old_scopes: Vec<Vec<String>> = old_recs
                        .iter()
                        .map(|r| r.scope_vec(&new_fam.dimensions))
                        .collect();
                    let new_scopes: Vec<Vec<String>> = new_recs
                        .iter()
                        .map(|r| r.scope_vec(&new_fam.dimensions))
                        .collect();
                    let old_set: IndexSet<&Vec<String>> = old_scopes.iter().collect();
                    let new_set: IndexSet<&Vec<String>> = new_scopes.iter().collect();
                    if old_set != new_set {
                        entries.push(DiffEntry {
                            category: DiffCategory::ScopeChanged,
                            subject: fam_name.clone(),
                            detail: format!(
                                "record set changed ({} → {} records)",
                                old_scopes.len(),
                                new_scopes.len()
                            ),
                        });
                    } else if old_scopes != new_scopes && uses_prefix_sum {
                        entries.push(DiffEntry {
                            category: DiffCategory::DefinitionChanged,
                            subject: fam_name.clone(),
                            detail: "manifest record order changed for family using `prefix_sum`"
                                .to_string(),
                        });
                    }
                }
            }
        }
    }

    // 2. Check Evidence Refreshed & Value Changed in lockfile
    if let (Some(old_lock), Some(new_lock)) = (&old.lockfile, &new.lockfile) {
        for (prod_id, new_prod) in &new_lock.producers {
            if let Some(old_prod) = old_lock.producers.get(prod_id) {
                if let (Some(old_cur), Some(new_cur)) = (
                    old_prod.epochs.get("current"),
                    new_prod.epochs.get("current"),
                ) {
                    if old_cur.run_id != new_cur.run_id
                        || old_cur.input_set_digest != new_cur.input_set_digest
                    {
                        entries.push(DiffEntry {
                            category: DiffCategory::EvidenceRefreshed,
                            subject: format!("{prod_id}@current"),
                            detail: format!("{} → {}", old_cur.run_id, new_cur.run_id),
                        });
                    }
                }
            }
        }

        for (claim_key, new_val) in &new_lock.evaluated_claims {
            if let Some(old_val) = old_lock.evaluated_claims.get(claim_key) {
                if old_val.value != new_val.value {
                    entries.push(DiffEntry {
                        category: DiffCategory::ValueChanged,
                        subject: claim_key.clone(),
                        detail: format!(
                            "{} {} → {} {}",
                            old_val.value, old_val.unit, new_val.value, new_val.unit
                        ),
                    });
                }
            }
        }
    }

    entries
}

/// `cw merge-driver %O %A %B`: Structural Git merge driver for `.claimwire/evidence.lock.json`
/// and wired Markdown files (`spec.md` §9.2).
/// Writes the merged result into `%A` (`ours_path`). Returns `Ok(true)` if cleanly merged, `Ok(false)` on conflict.
pub fn run_merge_driver(base_path: &Path, ours_path: &Path, theirs_path: &Path) -> Result<bool> {
    let base_str = fs::read_to_string(base_path)
        .with_context(|| format!("Failed to read base `{}`", base_path.display()))?;
    let ours_str = fs::read_to_string(ours_path)
        .with_context(|| format!("Failed to read ours `{}`", ours_path.display()))?;
    let theirs_str = fs::read_to_string(theirs_path)
        .with_context(|| format!("Failed to read theirs `{}`", theirs_path.display()))?;

    // Check if the inputs are `evidence.lock.json` files
    if let (Ok(base_lock), Ok(ours_lock), Ok(theirs_lock)) = (
        serde_json::from_str::<EvidenceLockfile>(&base_str),
        serde_json::from_str::<EvidenceLockfile>(&ours_str),
        serde_json::from_str::<EvidenceLockfile>(&theirs_str),
    ) {
        let Some(merged) = merge_lockfiles(&base_lock, &ours_lock, &theirs_lock) else {
            return Ok(false);
        };
        fs::write(ours_path, merged.to_pretty_json()?)?;
        return Ok(true);
    }

    // Otherwise, merge wired Markdown structurally:
    // If only owned inline spans / table cells changed on one side or non-overlapping sides, merge them cleanly!
    let base_doc = MarkdownDocument::parse("base.md", &base_str);
    let ours_doc = MarkdownDocument::parse("ours.md", &ours_str);
    let theirs_doc = MarkdownDocument::parse("theirs.md", &theirs_str);

    if base_doc.inline_spans.len() == ours_doc.inline_spans.len()
        && base_doc.inline_spans.len() == theirs_doc.inline_spans.len()
    {
        let mut patches: Vec<(ByteRange, String)> = Vec::new();
        for i in 0..base_doc.inline_spans.len() {
            let b_span = &base_doc.inline_spans[i];
            let o_span = &ours_doc.inline_spans[i];
            let t_span = &theirs_doc.inline_spans[i];
            if o_span.current_text == b_span.current_text
                && t_span.current_text != b_span.current_text
            {
                patches.push((o_span.value_range.clone(), t_span.current_text.clone()));
            } else if o_span.current_text != b_span.current_text
                && t_span.current_text != b_span.current_text
                && o_span.current_text != t_span.current_text
            {
                return Ok(false);
            }
        }
        let merged_md = apply_byte_patches(&ours_str, patches);
        fs::write(ours_path, merged_md)?;
        return Ok(true);
    }

    Ok(false)
}

fn merge_lockfiles(
    base: &EvidenceLockfile,
    ours: &EvidenceLockfile,
    theirs: &EvidenceLockfile,
) -> Option<EvidenceLockfile> {
    let mut merged = ours.clone();

    // Merge producers
    for (prod_id, t_prod) in &theirs.producers {
        let b_prod = base.producers.get(prod_id);
        let o_prod = ours.producers.get(prod_id);
        if o_prod == b_prod && Some(t_prod) != b_prod {
            merged.producers.insert(prod_id.clone(), t_prod.clone());
        } else if o_prod != b_prod && Some(t_prod) != b_prod && o_prod != Some(t_prod) {
            return None;
        }
    }

    // Merge evaluated_claims
    for (k, t_val) in &theirs.evaluated_claims {
        let b_val = base.evaluated_claims.get(k);
        let o_val = ours.evaluated_claims.get(k);
        if o_val == b_val && Some(t_val) != b_val {
            merged.evaluated_claims.insert(k.clone(), t_val.clone());
        } else if o_val != b_val && Some(t_val) != b_val && o_val != Some(t_val) {
            return None;
        }
    }

    // Merge locations
    for (k, t_loc) in &theirs.locations {
        let b_loc = base.locations.get(k);
        let o_loc = ours.locations.get(k);
        if o_loc == b_loc && Some(t_loc) != b_loc {
            merged.locations.insert(k.clone(), t_loc.clone());
        } else if o_loc != b_loc && Some(t_loc) != b_loc && o_loc != Some(t_loc) {
            return None;
        }
    }

    Some(merged)
}
