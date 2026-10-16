//! Evidence store, epoch resolution (`@current`, `@prev`, `@initial`),
//! immutable run manifests, and `@cw:record` stdout wire protocol capture (`spec.md` §6.1, §6.4–§6.5).

use crate::fingerprint::{FingerprintCache, compute_input_set_digest, expand_input_globs};
use anyhow::{Context, Result, bail};
use cw_ir::ClaimsConfig;
use cw_syntax::{CanonicalValue, Diagnostic, DiagnosticCode, Severity, unquote_str};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceConfig {
    pub schema: String,
    #[serde(default)]
    pub producers: IndexMap<String, ProducerDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducerDecl {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub families: Vec<String>,
    #[serde(default)]
    pub inputs: ProducerInputs,
    #[serde(default)]
    pub on_stale: Option<String>,
    #[serde(default)]
    pub epochs: IndexMap<String, String>,
}

impl ProducerDecl {
    pub fn stale_severity(&self) -> Severity {
        match self.on_stale.as_deref() {
            Some("warn") => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProducerInputs {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlatEvidenceRecord {
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub scope: IndexMap<String, String>,
    #[serde(default)]
    pub metrics: IndexMap<String, CanonicalValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub schema: String,
    pub run_id: String,
    pub producer: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    pub git_commit: String,
    #[serde(default)]
    pub git_dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<String>,
    /// The command `cw capture` actually executed (may differ from the declared one via `-- <cmd>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub environment: IndexMap<String, String>,
    pub input_set_digest: String,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub input_files: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,
    /// Family records in the exact emission order (`prefix_sum` relies on record order, §4.4).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub families: IndexMap<String, Vec<EvidenceRecord>>,
    /// Scalar source metrics emitted by this producer (e.g. `om.inventory`).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub scalars: IndexMap<String, CanonicalValue>,
    /// Optional flat record array (`{ "family": ..., "scope": ..., "metrics": ... }`), normalized on load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<FlatEvidenceRecord>,
}

impl RunManifest {
    pub fn normalize(&mut self) {
        if self.hardware.is_none() {
            if let Some(hw) = self.environment.get("hardware") {
                self.hardware = Some(hw.clone());
            }
        }
        if !self.records.is_empty() {
            for rec in self.records.drain(..) {
                match rec.family {
                    Some(fam) => {
                        self.families.entry(fam).or_default().push(EvidenceRecord {
                            scope: rec.scope,
                            metrics: rec.metrics,
                        });
                    }
                    None => {
                        for (k, v) in rec.metrics {
                            self.scalars.insert(k, v);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub scope: IndexMap<String, String>,
    pub metrics: IndexMap<String, CanonicalValue>,
}

impl EvidenceRecord {
    pub fn scope_vec(&self, dimensions: &[String]) -> Vec<String> {
        dimensions
            .iter()
            .map(|d| self.scope.get(d).cloned().unwrap_or_default())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct LoadedEvidenceStore {
    pub config: EvidenceConfig,
    /// `producer_id -> epoch_name -> RunManifest`
    pub manifests: IndexMap<String, IndexMap<String, RunManifest>>,
    /// `producer_id -> current working-tree input_set_digest`
    pub current_tree_digests: IndexMap<String, String>,
    /// `producer_id -> expanded input file relative paths`
    pub producer_input_files: IndexMap<String, Vec<String>>,
    /// `producer_id -> (rel_path -> file_digest)`
    pub producer_file_digests: IndexMap<String, IndexMap<String, String>>,
    /// `producer_id -> bool` (true if `@current` matches working-tree `input_set_digest`)
    pub freshness: IndexMap<String, bool>,
}

impl LoadedEvidenceStore {
    pub fn load(
        workspace_root: &Path,
        evidence_config: EvidenceConfig,
    ) -> Result<(Self, Vec<Diagnostic>)> {
        let mut diagnostics = Vec::new();
        let mut manifests: IndexMap<String, IndexMap<String, RunManifest>> = IndexMap::new();
        let mut current_tree_digests = IndexMap::new();
        let mut producer_input_files = IndexMap::new();
        let mut producer_file_digests = IndexMap::new();
        let mut freshness = IndexMap::new();

        let (mut fp_cache, cache_mtime_ns) = FingerprintCache::load(workspace_root);
        let mut any_cache_miss = false;

        for (prod_id, prod_decl) in &evidence_config.producers {
            // Compute working-tree semantic input digest for this producer
            let rel_files = expand_input_globs(workspace_root, &prod_decl.inputs.files)?;
            let mut file_digests = IndexMap::new();
            for rel_path in &rel_files {
                let (digest, hit) =
                    fp_cache.fingerprint_file(workspace_root, rel_path, cache_mtime_ns)?;
                if !hit {
                    any_cache_miss = true;
                }
                file_digests.insert(rel_path.clone(), digest);
            }
            let mut env_vals = IndexMap::new();
            for var in &prod_decl.inputs.env {
                let val = std::env::var(var).unwrap_or_default();
                env_vals.insert(var.clone(), val);
            }
            let tree_digest = compute_input_set_digest(
                &file_digests,
                &env_vals,
                &prod_decl.command,
                &prod_decl.families,
                &prod_decl.inputs.files,
            );
            current_tree_digests.insert(prod_id.clone(), tree_digest.clone());
            producer_input_files.insert(prod_id.clone(), rel_files);
            producer_file_digests.insert(prod_id.clone(), file_digests.clone());

            // Load manifests for all declared epochs
            let mut epoch_map = IndexMap::new();
            for (epoch_name, run_id) in &prod_decl.epochs {
                let manifest_path = workspace_root
                    .join(".claimwire/runs")
                    .join(prod_id)
                    .join(format!("{run_id}.json"));
                match fs::read_to_string(&manifest_path) {
                    Ok(content) => match serde_json::from_str::<RunManifest>(&content) {
                        Ok(mut manifest) => {
                            manifest.normalize();
                            if epoch_name == "current" {
                                let is_fresh = manifest.input_set_digest == tree_digest;
                                freshness.insert(prod_id.clone(), is_fresh);
                                if !is_fresh {
                                    let mut diag = Diagnostic {
                                        code: DiagnosticCode::CW3001,
                                        severity: prod_decl.stale_severity(),
                                        message: format!(
                                            "Producer `{prod_id}@current` is stale (run `{run_id}` input digest `{}` != working tree `{}`)",
                                            short_digest(&manifest.input_set_digest),
                                            short_digest(&tree_digest),
                                        ),
                                        file: Some(".claimwire/evidence.yaml".to_string()),
                                        line: Some(1),
                                        col: Some(1),
                                        config_path: Some(vec![
                                            "producers".to_string(),
                                            prod_id.clone(),
                                        ]),
                                        span_len: None,
                                        snippet_line: None,
                                        label: None,
                                        notes: Vec::new(),
                                        help: Some(format!(
                                            "run `cw capture --producer {prod_id} -- {}` to refresh `@current`",
                                            prod_decl.command.join(" ")
                                        )),
                                    };
                                    // Find which specific input files changed compared to `manifest.input_files`
                                    if !manifest.input_files.is_empty() {
                                        for (path, cur_d) in &file_digests {
                                            match manifest.input_files.get(path) {
                                                Some(old_d) if old_d != cur_d => {
                                                    diag.notes.push(format!(
                                                        "modified input: {path} ({} → {})",
                                                        short_digest(old_d),
                                                        short_digest(cur_d)
                                                    ));
                                                }
                                                None => {
                                                    diag.notes.push(format!("added input: {path}"));
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    diagnostics.push(diag);
                                }
                            }
                            epoch_map.insert(epoch_name.clone(), manifest);
                        }
                        Err(e) => {
                            diagnostics.push(Diagnostic::error(
                                DiagnosticCode::CW2104,
                                format!(
                                    "Failed to parse run manifest `{}`: {e}",
                                    manifest_path.display()
                                ),
                            ));
                        }
                    },
                    Err(_) => {
                        diagnostics.push(Diagnostic::error(
                            DiagnosticCode::CW2104,
                            format!(
                                "Missing run manifest `.claimwire/runs/{prod_id}/{run_id}.json` for epoch `{prod_id}@{epoch_name}`"
                            ),
                        ));
                    }
                }
            }
            manifests.insert(prod_id.clone(), epoch_map);
        }

        if any_cache_miss {
            let _ = fp_cache.save(workspace_root);
        }

        Ok((
            Self {
                config: evidence_config,
                manifests,
                current_tree_digests,
                producer_input_files,
                producer_file_digests,
                freshness,
            },
            diagnostics,
        ))
    }

    pub fn get_manifest(&self, producer: &str, epoch: &str) -> Option<&RunManifest> {
        self.manifests.get(producer)?.get(epoch)
    }
}

pub fn short_digest(d: &str) -> String {
    if let Some(hex_part) = d.strip_prefix("blake3:") {
        if hex_part.len() > 8 {
            return format!("blake3:{}…", &hex_part[..8]);
        }
    }
    d.to_string()
}

/// Parse `@cw:record` telemetry lines and validate against the producer's declared families and scalar claims (§6.4).
pub fn parse_and_validate_cw_frames(
    producer_id: &str,
    producer_decl: &ProducerDecl,
    claims_config: &ClaimsConfig,
    lines: &[String],
) -> Result<(
    IndexMap<String, Vec<EvidenceRecord>>,
    IndexMap<String, CanonicalValue>,
)> {
    let mut family_records: IndexMap<String, Vec<EvidenceRecord>> = IndexMap::new();
    let mut scalars: IndexMap<String, CanonicalValue> = IndexMap::new();

    for line in lines {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("@cw:record") else {
            continue;
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let kv_pairs = parse_frame_kv_pairs(rest.trim())?;
        if kv_pairs.is_empty() {
            bail!("Empty `@cw:record` frame");
        }

        if let Some(fam_name) = kv_pairs.get("family") {
            if !producer_decl.families.contains(fam_name) {
                bail!(
                    "Frame family `{fam_name}` is not declared under producer `{producer_id}` (`{:?}`)",
                    producer_decl.families
                );
            }
            let Some(fam_decl) = claims_config.families.get(fam_name) else {
                bail!("Unknown family `{fam_name}` in `@cw:record` frame");
            };
            let mut scope = IndexMap::new();
            for dim in &fam_decl.dimensions {
                let Some(val) = kv_pairs.get(dim) else {
                    bail!(
                        "Missing required dimension `{dim}` for family `{fam_name}` in frame `{trimmed}`"
                    );
                };
                scope.insert(dim.clone(), val.clone());
            }
            let mut metrics = IndexMap::new();
            for (k, v) in &kv_pairs {
                if k == "family" || fam_decl.dimensions.contains(k) {
                    continue;
                }
                let Some(m_decl) = fam_decl.metrics.get(k) else {
                    bail!("Unknown metric `{k}` for family `{fam_name}` in frame `{trimmed}`");
                };
                let parsed_val = CanonicalValue::parse_literal(v, &m_decl.value_type)
                    .map_err(|d| anyhow::anyhow!("Invalid metric `{k}={v}`: {}", d.message))?;
                metrics.insert(k.clone(), parsed_val);
            }
            family_records
                .entry(fam_name.clone())
                .or_default()
                .push(EvidenceRecord { scope, metrics });
        } else {
            // Scalar source frame for producer (e.g. `@cw:record rust_tests=362 python_tests=227` or `metric="rust_tests" value=362`)
            if kv_pairs.len() == 2
                && kv_pairs.contains_key("metric")
                && kv_pairs.contains_key("value")
            {
                let m_name = &kv_pairs["metric"];
                let val_str = &kv_pairs["value"];
                let claim_decl = find_scalar_source_claim(claims_config, producer_id, m_name)?;
                let parsed = CanonicalValue::parse_literal(val_str, &claim_decl.value_type)
                    .map_err(|d| {
                        anyhow::anyhow!("Invalid scalar `{m_name}={val_str}`: {}", d.message)
                    })?;
                scalars.insert(m_name.clone(), parsed);
            } else {
                for (m_name, val_str) in &kv_pairs {
                    let claim_decl = find_scalar_source_claim(claims_config, producer_id, m_name)?;
                    let parsed = CanonicalValue::parse_literal(val_str, &claim_decl.value_type)
                        .map_err(|d| {
                            anyhow::anyhow!("Invalid scalar `{m_name}={val_str}`: {}", d.message)
                        })?;
                    scalars.insert(m_name.clone(), parsed);
                }
            }
        }
    }

    Ok((family_records, scalars))
}

fn find_scalar_source_claim<'a>(
    claims_config: &'a ClaimsConfig,
    producer_id: &str,
    metric_name: &str,
) -> Result<&'a cw_ir::ScalarClaimDecl> {
    for (_c_name, c_decl) in &claims_config.claims {
        if c_decl.effective_kind() == "source"
            && c_decl.producer.as_deref() == Some(producer_id)
            && c_decl.metric.as_deref() == Some(metric_name)
        {
            return Ok(c_decl);
        }
    }
    bail!("Unknown scalar metric `{metric_name}` for producer `{producer_id}`")
}

fn parse_frame_kv_pairs(s: &str) -> Result<IndexMap<String, String>> {
    let mut map = IndexMap::new();
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            bail!("Expected `key=value` in `@cw:record` frame: `{s}`");
        }
        let key = s[key_start..i].to_string();
        i += 1; // skip `=`
        if i >= bytes.len() {
            bail!("Missing value after `{key}=` in `@cw:record` frame");
        }
        let val = if bytes[i] == b'"' {
            let val_start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            unquote_str(&s[val_start..i])?
        } else {
            let val_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            s[val_start..i].to_string()
        };
        map.insert(key, val);
    }

    Ok(map)
}

/// Run a producer command, capture `@cw:record` frames, fingerprint inputs, write an immutable
/// run manifest `.claimwire/runs/<producer>/<run-id>.json`, and update `.claimwire/evidence.yaml` `@current`.
/// Returns `(manifest, manifest path, reused)`; `reused` means an identical earlier run was re-pointed
/// to instead of writing a new manifest.
pub fn execute_capture(
    workspace_root: &Path,
    producer_id: &str,
    custom_cmd: &[String],
    run_id_override: Option<&str>,
    evidence_config: &EvidenceConfig,
    claims_config: &ClaimsConfig,
) -> Result<(RunManifest, String, bool)> {
    let Some(prod_decl) = evidence_config.producers.get(producer_id) else {
        bail!("Unknown producer `{producer_id}` in `.claimwire/evidence.yaml`");
    };
    let cmd_vec = if custom_cmd.is_empty() {
        &prod_decl.command
    } else {
        custom_cmd
    };
    if cmd_vec.is_empty() {
        bail!("No command specified for producer `{producer_id}`");
    }

    let input_files_before = fingerprint_producer_inputs(workspace_root, prod_decl)?;
    let output = Command::new(&cmd_vec[0])
        .args(&cmd_vec[1..])
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("Failed to execute `{}`", cmd_vec.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Producer `{producer_id}` exited with {}: {stderr}",
            output.status
        );
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    let mut all_lines: Vec<String> = stdout_str.lines().map(|l| l.to_string()).collect();
    all_lines.extend(stderr_str.lines().map(|l| l.to_string()));

    let (families, scalars) =
        parse_and_validate_cw_frames(producer_id, prod_decl, claims_config, &all_lines)?;
    if families.is_empty() && scalars.is_empty() {
        bail!("Producer `{producer_id}` emitted zero `@cw:record` frames");
    }

    // The manifest certifies the inputs the producer saw. If it (or anything else) edited a
    // declared input while running, the measurement belongs to neither version of the tree.
    let input_files = fingerprint_producer_inputs(workspace_root, prod_decl)?;
    if input_files != input_files_before {
        let changed: Vec<&String> = input_files
            .keys()
            .chain(input_files_before.keys())
            .filter(|k| input_files.get(*k) != input_files_before.get(*k))
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect();
        bail!(
            "Declared inputs of producer `{producer_id}` changed while it ran ({}); no manifest written",
            changed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut env = IndexMap::new();
    for var in &prod_decl.inputs.env {
        env.insert(var.clone(), std::env::var(var).unwrap_or_default());
    }
    let input_set_digest = compute_input_set_digest(
        &input_files,
        &env,
        &prod_decl.command,
        &prod_decl.families,
        &prod_decl.inputs.files,
    );

    let (git_commit, git_dirty) = detect_git_state(workspace_root);
    let run_id = match run_id_override {
        Some(id) => id.to_string(),
        None => {
            let short_hash = input_set_digest
                .strip_prefix("blake3:")
                .unwrap_or(&input_set_digest);
            let short_hash = &short_hash[..8.min(short_hash.len())];
            let short_prod = producer_id.split('.').next_back().unwrap_or(producer_id);
            format!("{short_prod}-{short_hash}")
        }
    };

    let runs_dir = workspace_root.join(".claimwire/runs").join(producer_id);
    fs::create_dir_all(&runs_dir)?;

    // Re-running a deterministic producer on unchanged inputs reproduces an existing manifest.
    // Reuse it instead of minting `<id>-1`, `<id>-2`, ... copies of the same evidence.
    if run_id_override.is_none() {
        if let Some(existing) =
            find_equivalent_run(&runs_dir, &input_set_digest, &families, &scalars, &env)
        {
            set_current_epoch(workspace_root, producer_id, &existing.run_id)?;
            let rel = format!(".claimwire/runs/{producer_id}/{}.json", existing.run_id);
            return Ok((existing, rel, true));
        }
    }

    let mut final_run_id = run_id.clone();
    let mut seq = 1usize;
    while runs_dir.join(format!("{final_run_id}.json")).exists() {
        final_run_id = format!("{run_id}-{seq}");
        seq += 1;
    }

    let manifest = RunManifest {
        schema: "claimwire-run/v1".to_string(),
        run_id: final_run_id.clone(),
        producer: producer_id.to_string(),
        timestamp: Some(utc_now_rfc3339()),
        git_commit,
        git_dirty,
        hardware: Some(detect_hardware_summary()),
        command: cmd_vec.to_vec(),
        environment: IndexMap::new(),
        input_set_digest,
        input_files,
        env,
        families,
        scalars,
        records: Vec::new(),
    };

    let manifest_rel = format!(".claimwire/runs/{producer_id}/{final_run_id}.json");
    let manifest_path = workspace_root.join(&manifest_rel);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;

    set_current_epoch(workspace_root, producer_id, &final_run_id)?;

    Ok((manifest, manifest_rel, false))
}

fn fingerprint_producer_inputs(
    workspace_root: &Path,
    prod_decl: &ProducerDecl,
) -> Result<IndexMap<String, String>> {
    let (mut fp_cache, cache_mtime_ns) = FingerprintCache::load(workspace_root);
    let rel_files = expand_input_globs(workspace_root, &prod_decl.inputs.files)?;
    let mut input_files = IndexMap::new();
    for rel_path in &rel_files {
        let (digest, _) = fp_cache.fingerprint_file(workspace_root, rel_path, cache_mtime_ns)?;
        input_files.insert(rel_path.clone(), digest);
    }
    let _ = fp_cache.save(workspace_root);
    Ok(input_files)
}

fn find_equivalent_run(
    runs_dir: &Path,
    input_set_digest: &str,
    families: &IndexMap<String, Vec<EvidenceRecord>>,
    scalars: &IndexMap<String, CanonicalValue>,
    env: &IndexMap<String, String>,
) -> Option<RunManifest> {
    let mut entries: Vec<_> = fs::read_dir(runs_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|path| {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            return None;
        }
        let mut m: RunManifest = serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
        m.normalize();
        (m.input_set_digest == input_set_digest
            && &m.families == families
            && &m.scalars == scalars
            && &m.env == env)
            .then_some(m)
    })
}

/// Point `@current` of `producer_id` at `run_id` in `.claimwire/evidence.yaml`.
fn set_current_epoch(workspace_root: &Path, producer_id: &str, run_id: &str) -> Result<()> {
    let ev_yaml_path = workspace_root.join(".claimwire/evidence.yaml");
    let ev_text = fs::read_to_string(&ev_yaml_path)
        .with_context(|| format!("Failed to read `{}`", ev_yaml_path.display()))?;
    let Some(updated) = update_current_epoch_in_yaml(&ev_text, producer_id, run_id) else {
        bail!(
            "Could not find `epochs:` → `current:` under producer `{producer_id}` in `.claimwire/evidence.yaml`; add `epochs: {{ current: {run_id} }}` as a block mapping"
        );
    };
    if updated != ev_text {
        fs::write(&ev_yaml_path, updated)?;
    }
    Ok(())
}

/// Current UTC time as RFC 3339 (`2026-10-16T14:47:03Z`).
fn utc_now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant), proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn detect_git_state(workspace_root: &Path) -> (String, bool) {
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);

    (commit, dirty)
}

fn detect_hardware_summary() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
        {
            let brand = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !brand.is_empty() {
                return brand;
            }
        }
    }
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn update_current_epoch_in_yaml(
    yaml_text: &str,
    producer_id: &str,
    new_run_id: &str,
) -> Option<String> {
    let mut lines: Vec<String> = yaml_text.lines().map(|l| l.to_string()).collect();
    let mut in_producer = false;
    let mut in_epochs = false;
    let mut prod_indent = 0usize;
    let mut updated = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if trimmed == format!("{producer_id}:") {
            in_producer = true;
            prod_indent = indent;
            in_epochs = false;
            continue;
        }
        if in_producer && indent <= prod_indent {
            in_producer = false;
            in_epochs = false;
        }
        if in_producer && trimmed == "epochs:" {
            in_epochs = true;
            continue;
        }
        if in_producer && in_epochs && trimmed.starts_with("current:") {
            let prefix_ws = &line[..indent];
            *line = format!("{prefix_ws}current: {new_run_id}");
            updated = true;
            break;
        }
    }

    if !updated {
        return None;
    }
    let mut out = lines.join("\n");
    if yaml_text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}
