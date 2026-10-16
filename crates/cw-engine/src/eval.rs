//! Topological Evaluator and Deadband Hysteresis (`spec.md` §4.1–§4.4, §6.3).

use crate::lockfile::{EvidenceLockfile, LockedClaimValue, LockedEpoch, LockedProducer};
use cw_evidence::{EvidenceRecord, LoadedEvidenceStore};
use cw_ir::{
    BinOp, CheckedIr, Expr, FamilyDecl, ReductionOp, parse_expr, resolve_family_field_type,
};
use cw_syntax::{
    CanonicalValue, ClaimRef, Diagnostic, DiagnosticCode, FamilyRef, ScalarRef, Selector,
    SelectorKey, ValueType, round_to_decimals,
};
use indexmap::IndexMap;

/// Holds the evaluated values of all claims for a single channel (`fresh` or `display`).
#[derive(Debug, Clone)]
pub struct EvalChannel {
    /// Scalar constants & scalar claims: `canonical_name -> (CanonicalValue, ValueType, Option<epoch>)`
    pub scalars: IndexMap<String, (CanonicalValue, ValueType, Option<String>)>,
    /// Family records per epoch: `family_name -> epoch_name -> Vec<EvaluatedRecord>`
    pub families: IndexMap<String, IndexMap<String, Vec<EvaluatedRecord>>>,
    /// Flat map of canonical claim refs (`guest_bench[radix-sort-u64, test].prove_wall`) -> `(CanonicalValue, ValueType, Option<epoch>)`
    pub flat_claims: IndexMap<String, (CanonicalValue, ValueType, Option<String>)>,
}

#[derive(Debug, Clone)]
pub struct EvaluatedRecord {
    pub scope: IndexMap<String, String>,
    pub values: IndexMap<String, (CanonicalValue, ValueType)>,
}

#[derive(Debug, Clone)]
pub struct EvaluationOutput {
    /// Fresh unrounded evaluation from current manifests (used for assertions and `--exact`).
    pub fresh: EvalChannel,
    /// Display evaluation with deadband hysteresis applied (used for rendering unless `--exact`).
    pub display: EvalChannel,
    /// Assertion evaluation results (`assertion_id -> passed`)
    pub assertions: IndexMap<String, bool>,
    /// Diagnostics emitted during evaluation (e.g. `CW2104`, `CW3002`)
    pub diagnostics: Vec<Diagnostic>,
}

pub fn evaluate_all(
    ir: &CheckedIr,
    evidence: &LoadedEvidenceStore,
    lockfile: Option<&EvidenceLockfile>,
    exact: bool,
) -> EvaluationOutput {
    let mut diagnostics = Vec::new();

    // 1. Evaluate the `fresh` channel directly from `evidence`
    let fresh = evaluate_channel(ir, evidence, None, &mut diagnostics);

    // 2. Evaluate assertions strictly against `fresh` values (§6.3: "Assertions never see held values")
    let mut assertions = IndexMap::new();
    for (a_name, a_decl) in &ir.config.assertions {
        if let Ok(expr) = parse_expr(&a_decl.expr) {
            match eval_expr_in_channel(&expr, &fresh, ir, None) {
                Ok((CanonicalValue::Bool(passed), _)) => {
                    assertions.insert(a_name.clone(), passed);
                    if !passed {
                        let msg = a_decl.message.clone().unwrap_or_else(|| {
                            format!(
                                "Assertion `{a_name}` failed: `{}` evaluated to `false`",
                                a_decl.expr
                            )
                        });
                        let mut diag = Diagnostic::error(DiagnosticCode::CW3002, msg)
                            .with_config_path(".claimwire/claims.yaml", &["assertions", a_name])
                            .with_note(format!("predicate: {}", a_decl.expr));
                        // Include concrete values of referenced claims in notes
                        let mut ref_notes = Vec::new();
                        collect_expr_value_notes(&expr, &fresh, &mut ref_notes);
                        for n in ref_notes {
                            diag.notes.push(n);
                        }
                        diag.help = Some(
                            "Claim Wire refuses to silently weaken guarded claims; fix the regression or update the specification bound explicitly"
                                .to_string(),
                        );
                        diagnostics.push(diag);
                    }
                }
                Ok((other, _)) => {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Assertion `{a_name}` evaluated to non-boolean `{other:?}`"),
                    ));
                }
                Err(d) => diagnostics.push(d),
            }
        }
    }

    let any_assertion_failed = assertions.values().any(|&passed| !passed);

    // 3. Evaluate the `display` channel with deadband hysteresis if `!exact && !any_assertion_failed`
    let display = if exact || any_assertion_failed || lockfile.is_none() {
        fresh.clone()
    } else {
        let mut dummy_diags = Vec::new();
        evaluate_channel(ir, evidence, lockfile, &mut dummy_diags)
    };

    EvaluationOutput {
        fresh,
        display,
        assertions,
        diagnostics,
    }
}

fn evaluate_channel(
    ir: &CheckedIr,
    evidence: &LoadedEvidenceStore,
    lockfile_for_deadband: Option<&EvidenceLockfile>,
    diagnostics: &mut Vec<Diagnostic>,
) -> EvalChannel {
    let mut channel = EvalChannel {
        scalars: IndexMap::new(),
        families: IndexMap::new(),
        flat_claims: IndexMap::new(),
    };

    // 1. Populate constants
    for (c_name, (val, ty)) in &ir.parsed_constants {
        channel
            .scalars
            .insert(c_name.clone(), (val.clone(), ty.clone(), None));
        channel
            .flat_claims
            .insert(c_name.clone(), (val.clone(), ty.clone(), None));
    }

    // 2. Populate raw family metrics across all epochs (`current`, `prev`, `initial`, etc.)
    for (fam_name, fam_decl) in &ir.config.families {
        let mut epoch_records_map: IndexMap<String, Vec<EvaluatedRecord>> = IndexMap::new();
        if let Some(prod_epochs) = evidence.manifests.get(&fam_decl.producer) {
            for (epoch_name, manifest) in prod_epochs {
                let raw_records = manifest.families.get(fam_name).cloned().unwrap_or_default();
                let mut eval_records = Vec::with_capacity(raw_records.len());

                for raw_rec in &raw_records {
                    let hold_record = if epoch_name == "current" {
                        if let Some(lock) = lockfile_for_deadband {
                            should_hold_record(fam_name, fam_decl, raw_rec, lock)
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    let mut values = IndexMap::new();
                    for (m_name, m_decl) in &fam_decl.metrics {
                        let raw_val = raw_rec
                            .metrics
                            .get(m_name)
                            .cloned()
                            .unwrap_or(CanonicalValue::Null);
                        let effective_val = if hold_record {
                            let canonical_key = make_family_claim_key(
                                fam_name,
                                epoch_name,
                                &fam_decl.dimensions,
                                &raw_rec.scope,
                                m_name,
                            );
                            lockfile_for_deadband
                                .and_then(|l| l.evaluated_claims.get(&canonical_key))
                                .and_then(|lv| json_to_canonical(&lv.value))
                                .unwrap_or(raw_val)
                        } else {
                            raw_val
                        };
                        values.insert(m_name.clone(), (effective_val, m_decl.value_type.clone()));
                    }
                    eval_records.push(EvaluatedRecord {
                        scope: raw_rec.scope.clone(),
                        values,
                    });
                }

                epoch_records_map.insert(epoch_name.clone(), eval_records);
            }
        }
        channel.families.insert(fam_name.clone(), epoch_records_map);
    }

    // Populate flat_claims for raw metrics before evaluating derivations
    sync_family_flat_claims(&mut channel, &ir.config.families);

    // 3. Evaluate family derivations in topological order across all epochs
    // First evaluate historical epochs (`prev`, `initial`), then `current`, because `current` derivations
    // may reference `@prev` or `@initial`!
    for (fam_name, fam_decl) in &ir.config.families {
        let deriv_order = ir
            .family_derivation_order
            .get(fam_name)
            .cloned()
            .unwrap_or_default();
        let epochs: Vec<String> = channel
            .families
            .get(fam_name)
            .map(|m| {
                let mut keys: Vec<String> = m.keys().cloned().collect();
                keys.sort_by_key(|k| if k == "current" { 1 } else { 0 });
                keys
            })
            .unwrap_or_default();

        for epoch_name in epochs {
            for d_name in &deriv_order {
                let Some(d_decl) = fam_decl.derivations.get(d_name) else {
                    continue;
                };
                let Ok(expr) = parse_expr(&d_decl.expr) else {
                    continue;
                };

                let rec_len = channel
                    .families
                    .get(fam_name)
                    .and_then(|m| m.get(&epoch_name))
                    .map(|v| v.len())
                    .unwrap_or(0);

                for rec_idx in 0..rec_len {
                    let ctx = FamilyEvalContext {
                        family_name: fam_name,
                        family_decl: fam_decl,
                        epoch: &epoch_name,
                        record_idx: rec_idx,
                    };
                    let val = match eval_expr_in_channel(&expr, &channel, ir, Some(&ctx)) {
                        Ok((v, _)) => v,
                        Err(d) => {
                            // Only emit diagnostic on `@current` if not a missing optional historical metric
                            if epoch_name == "current" {
                                diagnostics.push(d);
                            }
                            CanonicalValue::Null
                        }
                    };
                    if let Some(rec) = channel
                        .families
                        .get_mut(fam_name)
                        .and_then(|m| m.get_mut(&epoch_name))
                        .and_then(|v| v.get_mut(rec_idx))
                    {
                        rec.values
                            .insert(d_name.clone(), (val, d_decl.value_type.clone()));
                    }
                    sync_family_flat_claims(&mut channel, &ir.config.families);
                }
            }
        }
    }

    // 4. Evaluate scalar claims in topological order
    for c_name in &ir.scalar_eval_order {
        let Some(c_decl) = ir.config.claims.get(c_name) else {
            continue;
        };
        match c_decl.effective_kind() {
            "const" => {
                if let Some(v_str) = &c_decl.value {
                    if let Ok(val) = CanonicalValue::parse_literal(v_str, &c_decl.value_type) {
                        channel.scalars.insert(
                            c_name.clone(),
                            (val.clone(), c_decl.value_type.clone(), None),
                        );
                        channel
                            .flat_claims
                            .insert(c_name.clone(), (val, c_decl.value_type.clone(), None));
                    }
                }
            }
            "source" => {
                let prod_id = c_decl.producer.as_deref().unwrap_or("");
                let metric_key = c_decl.metric.as_deref().unwrap_or(c_name);
                let raw_val = evidence
                    .get_manifest(prod_id, "current")
                    .and_then(|m| m.scalars.get(metric_key))
                    .cloned()
                    .unwrap_or(CanonicalValue::Null);

                let effective_val = if let Some(lock) = lockfile_for_deadband {
                    let db = c_decl.deadband_fraction().unwrap_or(0.0);
                    if let Some(locked_entry) = lock.evaluated_claims.get(c_name) {
                        if let Some(locked_val) = json_to_canonical(&locked_entry.value) {
                            if within_deadband(&raw_val, &locked_val, db) {
                                locked_val
                            } else {
                                raw_val
                            }
                        } else {
                            raw_val
                        }
                    } else {
                        raw_val
                    }
                } else {
                    raw_val
                };

                channel.scalars.insert(
                    c_name.clone(),
                    (
                        effective_val.clone(),
                        c_decl.value_type.clone(),
                        Some("current".to_string()),
                    ),
                );
                channel.flat_claims.insert(
                    c_name.clone(),
                    (
                        effective_val,
                        c_decl.value_type.clone(),
                        Some("current".to_string()),
                    ),
                );
            }
            "derive" => {
                if let Some(expr_str) = &c_decl.expr {
                    if let Ok(expr) = parse_expr(expr_str) {
                        match eval_expr_in_channel(&expr, &channel, ir, None) {
                            Ok((val, _)) => {
                                channel.scalars.insert(
                                    c_name.clone(),
                                    (val.clone(), c_decl.value_type.clone(), None),
                                );
                                channel
                                    .flat_claims
                                    .insert(c_name.clone(), (val, c_decl.value_type.clone(), None));
                            }
                            Err(d) => diagnostics.push(d),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    channel
}

fn sync_family_flat_claims(
    channel: &mut EvalChannel,
    families_decl: &IndexMap<String, FamilyDecl>,
) {
    for (fam_name, fam_decl) in families_decl {
        let Some(epoch_map) = channel.families.get(fam_name) else {
            continue;
        };
        for (epoch_name, records) in epoch_map {
            for rec in records {
                for (field_name, (val, ty)) in &rec.values {
                    if val.is_null() {
                        continue;
                    }
                    let key = make_family_claim_key(
                        fam_name,
                        epoch_name,
                        &fam_decl.dimensions,
                        &rec.scope,
                        field_name,
                    );
                    let ep_opt = if fam_decl.metrics.contains_key(field_name) {
                        Some(epoch_name.clone())
                    } else {
                        None
                    };
                    channel
                        .flat_claims
                        .insert(key, (val.clone(), ty.clone(), ep_opt));
                }
            }
        }
    }
}

pub fn make_family_claim_key(
    fam_name: &str,
    epoch_name: &str,
    dimensions: &[String],
    scope: &IndexMap<String, String>,
    field_name: &str,
) -> String {
    let selectors: Vec<Selector> = dimensions
        .iter()
        .map(|d| {
            let v = scope.get(d).cloned().unwrap_or_default();
            Selector::Point(SelectorKey::bare(v))
        })
        .collect();
    let fref = FamilyRef {
        family: fam_name.to_string(),
        epoch: if epoch_name == "current" {
            None
        } else {
            Some(epoch_name.to_string())
        },
        selectors,
        field: field_name.to_string(),
    };
    fref.canonical_string()
}

/// §6.3: Deadband decisions are made per evidence record, and if any metric of a record leaves its band,
/// the whole record and its derived cone re-render together.
fn should_hold_record(
    fam_name: &str,
    fam_decl: &FamilyDecl,
    raw_rec: &EvidenceRecord,
    lockfile: &EvidenceLockfile,
) -> bool {
    let mut any_has_deadband = false;
    for (m_name, m_decl) in &fam_decl.metrics {
        let db = m_decl.deadband_fraction().unwrap_or(0.0);
        if db > 0.0 {
            any_has_deadband = true;
        }
        let Some(fresh_val) = raw_rec.metrics.get(m_name) else {
            continue;
        };
        if fresh_val.is_null() {
            continue;
        }
        let key = make_family_claim_key(
            fam_name,
            "current",
            &fam_decl.dimensions,
            &raw_rec.scope,
            m_name,
        );
        let Some(locked_entry) = lockfile.evaluated_claims.get(&key) else {
            return false;
        };
        let Some(locked_val) = json_to_canonical(&locked_entry.value) else {
            return false;
        };
        if !within_deadband(fresh_val, &locked_val, db) {
            return false;
        }
    }
    any_has_deadband
}

pub fn within_deadband(fresh: &CanonicalValue, locked: &CanonicalValue, deadband: f64) -> bool {
    if fresh == locked {
        return true;
    }
    if deadband <= 0.0 {
        return false;
    }
    if let (Some(f), Some(l)) = (fresh.as_f64(), locked.as_f64()) {
        if l.abs() == 0.0 {
            return f.abs() == 0.0;
        }
        let rel_diff = (f - l).abs() / l.abs();
        rel_diff <= deadband + 1e-12
    } else {
        false
    }
}

pub fn json_to_canonical(v: &serde_json::Value) -> Option<CanonicalValue> {
    match v {
        serde_json::Value::Null => Some(CanonicalValue::Null),
        serde_json::Value::Bool(b) => Some(CanonicalValue::Bool(*b)),
        serde_json::Value::Number(n) => n.as_f64().map(CanonicalValue::Number),
        serde_json::Value::String(s) => Some(CanonicalValue::Text(s.clone())),
        serde_json::Value::Object(obj) => {
            let passed = obj.get("passed")?.as_u64()?;
            let total = obj.get("total")?.as_u64()?;
            Some(CanonicalValue::PassRatio { passed, total })
        }
        _ => None,
    }
}

pub struct FamilyEvalContext<'a> {
    pub family_name: &'a str,
    pub family_decl: &'a FamilyDecl,
    pub epoch: &'a str,
    pub record_idx: usize,
}

pub fn eval_expr_in_channel(
    expr: &Expr,
    channel: &EvalChannel,
    ir: &CheckedIr,
    ctx: Option<&FamilyEvalContext<'_>>,
) -> Result<(CanonicalValue, ValueType), Diagnostic> {
    match expr {
        Expr::Number(n) => Ok((CanonicalValue::Number(*n), ValueType::Float)),
        Expr::Bool(b) => Ok((CanonicalValue::Bool(*b), ValueType::Bool)),
        Expr::EpochField { epoch, field } => {
            let Some(fctx) = ctx else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2004,
                    format!("`@{epoch}.{field}` is only valid inside a family context"),
                ));
            };
            let cur_rec = &channel.families[fctx.family_name][fctx.epoch][fctx.record_idx];
            let target_records = channel
                .families
                .get(fctx.family_name)
                .and_then(|m| m.get(epoch.as_str()))
                .ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2104,
                        format!("Missing epoch `@{epoch}` on family `{}`", fctx.family_name),
                    )
                })?;
            let matched = target_records
                .iter()
                .find(|r| r.scope == cur_rec.scope)
                .ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2104,
                        format!(
                            "Missing record `{:?}` in `{}@{epoch}`",
                            cur_rec.scope, fctx.family_name
                        ),
                    )
                })?;
            matched.values.get(field).cloned().ok_or_else(|| {
                Diagnostic::error(
                    DiagnosticCode::CW2104,
                    format!("Missing field `{field}` in `{}@{epoch}`", fctx.family_name),
                )
            })
        }
        Expr::Ref(cref) => resolve_claim_ref_value(cref, channel, ir, ctx),
        Expr::UnaryNeg(inner) => {
            let (val, ty) = eval_expr_in_channel(inner, channel, ir, ctx)?;
            match val {
                CanonicalValue::Number(n) => Ok((CanonicalValue::Number(-n), ty)),
                CanonicalValue::Null => Ok((CanonicalValue::Null, ty)),
                _ => Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    "Unary `-` requires numeric value",
                )),
            }
        }
        Expr::Not(inner) => {
            let (val, _) = eval_expr_in_channel(inner, channel, ir, ctx)?;
            match val {
                CanonicalValue::Bool(b) => Ok((CanonicalValue::Bool(!b), ValueType::Bool)),
                _ => Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    "Logical `not` requires boolean value",
                )),
            }
        }
        Expr::PrefixSum { epoch, field } => {
            let Some(fctx) = ctx else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2004,
                    "`prefix_sum(...)` is only valid inside a family derivation",
                ));
            };
            let target_epoch = epoch.as_deref().unwrap_or(fctx.epoch);
            let ty = resolve_family_field_type(fctx.family_name, fctx.family_decl, field)?;
            let records = channel
                .families
                .get(fctx.family_name)
                .and_then(|m| m.get(target_epoch))
                .ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2104,
                        format!(
                            "Missing epoch `@{target_epoch}` for `prefix_sum({field})` on `{}`",
                            fctx.family_name
                        ),
                    )
                })?;

            // §4.4: Evaluates the running cumulative sum of `metric` in the manifest's record order
            let mut sum = 0.0;
            for r in records.iter().take(fctx.record_idx + 1) {
                if let Some((CanonicalValue::Number(v), _)) = r.values.get(field) {
                    sum += *v;
                }
            }
            Ok((CanonicalValue::Number(sum), ty))
        }
        Expr::Reduction { op, target } => eval_reduction(*op, target, channel, ir, ctx),
        Expr::Round {
            expr: inner,
            decimals,
        } => {
            let (val, ty) = eval_expr_in_channel(inner, channel, ir, ctx)?;
            match val {
                CanonicalValue::Number(n) => {
                    Ok((CanonicalValue::Number(round_to_decimals(n, *decimals)), ty))
                }
                other => Ok((other, ty)),
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            let (lval, lty) = eval_expr_in_channel(lhs, channel, ir, ctx)?;
            let (rval, rty) = eval_expr_in_channel(rhs, channel, ir, ctx)?;
            let out_ty = cw_ir::check_binary_op_types(*op, &lty, &rty)?;
            if lval.is_null() || rval.is_null() {
                return Ok((CanonicalValue::Null, out_ty));
            }
            match op {
                BinOp::And => match (lval, rval) {
                    (CanonicalValue::Bool(a), CanonicalValue::Bool(b)) => {
                        Ok((CanonicalValue::Bool(a && b), ValueType::Bool))
                    }
                    _ => Err(Diagnostic::error(
                        DiagnosticCode::CW2001,
                        "Expected Bool in `and`",
                    )),
                },
                BinOp::Or => match (lval, rval) {
                    (CanonicalValue::Bool(a), CanonicalValue::Bool(b)) => {
                        Ok((CanonicalValue::Bool(a || b), ValueType::Bool))
                    }
                    _ => Err(Diagnostic::error(
                        DiagnosticCode::CW2001,
                        "Expected Bool in `or`",
                    )),
                },
                BinOp::Eq => Ok((CanonicalValue::Bool(lval == rval), ValueType::Bool)),
                BinOp::Ne => Ok((CanonicalValue::Bool(lval != rval), ValueType::Bool)),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let (Some(a), Some(b)) = (lval.as_f64(), rval.as_f64()) else {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW2001,
                            "Numeric comparison requires numeric values",
                        ));
                    };
                    let res = match op {
                        BinOp::Lt => a < b,
                        BinOp::Le => a <= b,
                        BinOp::Gt => a > b,
                        BinOp::Ge => a >= b,
                        _ => unreachable!(),
                    };
                    Ok((CanonicalValue::Bool(res), ValueType::Bool))
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                    let (Some(a), Some(b)) = (lval.as_f64(), rval.as_f64()) else {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW2001,
                            "Arithmetic requires numeric values",
                        ));
                    };
                    let res = match op {
                        BinOp::Add => a + b,
                        BinOp::Sub => a - b,
                        BinOp::Mul => a * b,
                        BinOp::Div => {
                            if b == 0.0 {
                                return Ok((CanonicalValue::Null, out_ty));
                            }
                            // Note: Cycles / Duration (ns) -> Throughput (cycles/s)
                            // Because Duration is stored in nanoseconds (1e-9 s),
                            // `cycles / duration_ns` must be multiplied by `1e9` to yield `cycles/s`!
                            if lty == ValueType::Cycles && rty == ValueType::Duration {
                                (a / b) * 1e9
                            } else {
                                a / b
                            }
                        }
                        _ => unreachable!(),
                    };
                    Ok((CanonicalValue::Number(res), out_ty))
                }
            }
        }
    }
}

pub fn resolve_claim_ref_value(
    cref: &ClaimRef,
    channel: &EvalChannel,
    ir: &CheckedIr,
    ctx: Option<&FamilyEvalContext<'_>>,
) -> Result<(CanonicalValue, ValueType), Diagnostic> {
    match cref {
        ClaimRef::Scalar(ScalarRef { segments }) => {
            if segments.len() == 1 {
                if let Some(fctx) = ctx {
                    let field = &segments[0];
                    if fctx.family_decl.metrics.contains_key(field)
                        || fctx.family_decl.derivations.contains_key(field)
                    {
                        let rec = &channel.families[fctx.family_name][fctx.epoch][fctx.record_idx];
                        if let Some(v) = rec.values.get(field) {
                            return Ok(v.clone());
                        }
                        let ty =
                            resolve_family_field_type(fctx.family_name, fctx.family_decl, field)?;
                        return Ok((CanonicalValue::Null, ty));
                    }
                }
            }
            let full = segments.join(".");
            if let Some((val, ty, _)) = channel.scalars.get(&full) {
                return Ok((val.clone(), ty.clone()));
            }
            Err(Diagnostic::error(
                DiagnosticCode::CW2003,
                format!("Unresolved scalar claim or constant `{full}`"),
            ))
        }
        ClaimRef::Family(fref) => {
            let Some(target_fam) = ir.config.families.get(&fref.family) else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2003,
                    format!("Unresolved claim family `{}`", fref.family),
                ));
            };
            let ty = resolve_family_field_type(&fref.family, target_fam, &fref.field)?;
            if fref.selectors.len() != target_fam.dimensions.len() {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2003,
                    format!(
                        "Selector count mismatch for `{}`: expected {}, got {}",
                        fref.family,
                        target_fam.dimensions.len(),
                        fref.selectors.len()
                    ),
                ));
            }

            // Resolve dimension binding (§4.3: inside a family's derivations, an unquoted BareKey that equals
            // one of the enclosing family's dimension names binds to the current record's value for that dimension)
            let mut point_scope = IndexMap::new();
            for (dim_name, sel) in target_fam.dimensions.iter().zip(fref.selectors.iter()) {
                match sel {
                    Selector::Point(key) => {
                        let resolved_val = if !key.was_quoted {
                            if let Some(fctx) = ctx {
                                let cur_rec = &channel.families[fctx.family_name][fctx.epoch]
                                    [fctx.record_idx];
                                cur_rec
                                    .scope
                                    .get(&key.value)
                                    .cloned()
                                    .unwrap_or_else(|| key.value.clone())
                            } else {
                                key.value.clone()
                            }
                        } else {
                            key.value.clone()
                        };
                        point_scope.insert(dim_name.clone(), resolved_val);
                    }
                    _ => {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW2004,
                            format!(
                                "Multi-record selector `{fref}` must be wrapped in `sum(...)`, `min(...)`, or `max(...)`"
                            ),
                        ));
                    }
                }
            }

            let epoch = fref.epoch.as_deref().unwrap_or_else(|| {
                // If inside a family derivation at a historical epoch and no explicit `@epoch` was given,
                // evaluate in the current context's epoch!
                if let Some(fctx) = ctx {
                    if fctx.family_name == fref.family {
                        return fctx.epoch;
                    }
                }
                "current"
            });

            let records = channel
                .families
                .get(&fref.family)
                .and_then(|m| m.get(epoch))
                .ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2104,
                        format!("Missing epoch `@{epoch}` for family `{}`", fref.family),
                    )
                })?;

            let Some(rec) = records.iter().find(|r| r.scope == point_scope) else {
                return Ok((CanonicalValue::Null, ty));
            };

            match rec.values.get(&fref.field) {
                Some((v, t)) => Ok((v.clone(), t.clone())),
                None => Ok((CanonicalValue::Null, ty)),
            }
        }
    }
}

fn eval_reduction(
    op: ReductionOp,
    target: &FamilyRef,
    channel: &EvalChannel,
    ir: &CheckedIr,
    ctx: Option<&FamilyEvalContext<'_>>,
) -> Result<(CanonicalValue, ValueType), Diagnostic> {
    let Some(target_fam) = ir.config.families.get(&target.family) else {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2003,
            format!("Unresolved family `{}` in reduction", target.family),
        ));
    };
    let ty = resolve_family_field_type(&target.family, target_fam, &target.field)?;
    let epoch = target.epoch.as_deref().unwrap_or_else(|| {
        if let Some(fctx) = ctx {
            if fctx.family_name == target.family {
                return fctx.epoch;
            }
        }
        "current"
    });

    let records = channel
        .families
        .get(&target.family)
        .and_then(|m| m.get(epoch))
        .ok_or_else(|| {
            Diagnostic::error(
                DiagnosticCode::CW2104,
                format!("Missing epoch `@{epoch}` for family `{}`", target.family),
            )
        })?;

    let mut matched_nums = Vec::new();
    for rec in records {
        if record_matches_selectors(
            &rec.scope,
            &target_fam.dimensions,
            &target.selectors,
            ctx,
            channel,
        ) {
            // §4.4: Reductions skip records whose metric is `null` (sentinel rows such as `exact`).
            if let Some((CanonicalValue::Number(n), _)) = rec.values.get(&target.field) {
                matched_nums.push(*n);
            }
        }
    }

    // §4.4: A reduction over an empty set is `CW2104`, never a silent `0`.
    if matched_nums.is_empty() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2104,
            format!("Reduction over `{target}` matched 0 non-null records"),
        ));
    }

    let result = match op {
        ReductionOp::Sum => matched_nums.into_iter().sum::<f64>(),
        ReductionOp::Min => matched_nums
            .into_iter()
            .fold(f64::INFINITY, |a, b| a.min(b)),
        ReductionOp::Max => matched_nums
            .into_iter()
            .fold(f64::NEG_INFINITY, |a, b| a.max(b)),
    };

    Ok((CanonicalValue::Number(result), ty))
}

fn record_matches_selectors(
    scope: &IndexMap<String, String>,
    dimensions: &[String],
    selectors: &[Selector],
    ctx: Option<&FamilyEvalContext<'_>>,
    channel: &EvalChannel,
) -> bool {
    for (dim, sel) in dimensions.iter().zip(selectors.iter()) {
        let Some(rec_val) = scope.get(dim) else {
            return false;
        };
        match sel {
            Selector::Wildcard => {}
            Selector::Point(k) => {
                let target_val = resolve_selector_key(k, ctx, channel);
                if rec_val != &target_val {
                    return false;
                }
            }
            Selector::Subset(keys) => {
                let any_match = keys.iter().any(|k| {
                    let target_val = resolve_selector_key(k, ctx, channel);
                    rec_val == &target_val
                });
                if !any_match {
                    return false;
                }
            }
        }
    }
    true
}

fn resolve_selector_key(
    key: &SelectorKey,
    ctx: Option<&FamilyEvalContext<'_>>,
    channel: &EvalChannel,
) -> String {
    if !key.was_quoted {
        if let Some(fctx) = ctx {
            let cur_rec = &channel.families[fctx.family_name][fctx.epoch][fctx.record_idx];
            if let Some(bound) = cur_rec.scope.get(&key.value) {
                return bound.clone();
            }
        }
    }
    key.value.clone()
}

fn collect_expr_value_notes(expr: &Expr, channel: &EvalChannel, out: &mut Vec<String>) {
    match expr {
        Expr::Ref(cref) => {
            let key = cref.canonical_string();
            if let Some((val, ty, _)) = channel.flat_claims.get(&key) {
                if let Some(n) = val.as_f64() {
                    out.push(format!("{key} = {n} {}", ty.canonical_unit()));
                }
            }
        }
        Expr::Reduction { op, target } => {
            out.push(format!("{op:?}({target})"));
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_value_notes(lhs, channel, out);
            collect_expr_value_notes(rhs, channel, out);
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            collect_expr_value_notes(e, channel, out);
        }
        _ => {}
    }
}

/// Build the updated `EvidenceLockfile` producers & evaluated_claims sections from `EvalChannel`.
pub fn build_lockfile_claims_and_producers(
    channel: &EvalChannel,
    evidence: &LoadedEvidenceStore,
) -> (
    IndexMap<String, LockedProducer>,
    IndexMap<String, LockedClaimValue>,
) {
    let mut producers = IndexMap::new();
    for (prod_id, epoch_map) in &evidence.manifests {
        let mut epochs = IndexMap::new();
        for (ep_name, manifest) in epoch_map {
            epochs.insert(
                ep_name.clone(),
                LockedEpoch {
                    run_id: manifest.run_id.clone(),
                    git_commit: manifest.git_commit.clone(),
                    git_dirty: manifest.git_dirty,
                    input_set_digest: manifest.input_set_digest.clone(),
                },
            );
        }
        producers.insert(prod_id.clone(), LockedProducer { epochs });
    }

    let mut evaluated_claims = IndexMap::new();
    for (key, (val, ty, epoch)) in &channel.flat_claims {
        if val.is_null() {
            continue;
        }
        evaluated_claims.insert(
            key.clone(),
            LockedClaimValue {
                value: val.to_json_number_or_val(),
                unit: ty.canonical_unit(),
                epoch: epoch.clone(),
            },
        );
    }

    (producers, evaluated_claims)
}
