//! Dimensional type checker, scope validator, and topological cycle detector (`spec.md` §4.1–§4.5).

use crate::expr::{BinOp, Expr, parse_expr};
use crate::schema::{ClaimsConfig, FamilyDecl, TableMode, TableRowSpec};
use cw_syntax::{
    CanonicalValue, ClaimRef, Diagnostic, DiagnosticCode, FamilyRef, FormatPattern, ValueType,
};
use indexmap::{IndexMap, IndexSet};

#[derive(Debug, Clone)]
pub struct CheckedIr {
    pub config: ClaimsConfig,
    pub parsed_constants: IndexMap<String, (CanonicalValue, ValueType)>,
    pub family_derivation_order: IndexMap<String, Vec<String>>,
    pub scalar_eval_order: Vec<String>,
}

pub fn check_claims_config(config: &ClaimsConfig) -> Result<CheckedIr, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut parsed_constants = IndexMap::new();

    // 1. Validate constants
    for (name, decl) in &config.constants {
        match CanonicalValue::parse_literal(&decl.value, &decl.value_type) {
            Ok(val) => {
                parsed_constants.insert(name.clone(), (val, decl.value_type.clone()));
            }
            Err(d) => diagnostics.push(d),
        }
        if let Some(fmt_str) = &decl.format {
            match FormatPattern::parse(fmt_str) {
                Ok(pat) => {
                    if let Err(d) = pat.validate_types(|_| Ok(decl.value_type.clone())) {
                        diagnostics.push(d);
                    }
                }
                Err(d) => diagnostics.push(d),
            }
        }
    }

    // 2. Validate families, metrics, derivations, and topological derivation order
    let mut family_derivation_order = IndexMap::new();
    for (fam_name, fam) in &config.families {
        for (_m_name, m_decl) in &fam.metrics {
            if let Err(d) = m_decl.deadband_fraction() {
                diagnostics.push(d);
            }
            if let Some(fmt_str) = &m_decl.format {
                match FormatPattern::parse(fmt_str) {
                    Ok(pat) => {
                        if let Err(d) = pat.validate_types(|_| Ok(m_decl.value_type.clone())) {
                            diagnostics.push(d);
                        }
                    }
                    Err(d) => diagnostics.push(d),
                }
            }
        }

        let mut deriv_deps: IndexMap<String, IndexSet<String>> = IndexMap::new();
        for (d_name, d_decl) in &fam.derivations {
            if let Err(d) = d_decl.deadband_fraction() {
                diagnostics.push(d);
            }
            if let Some(fmt_str) = &d_decl.format {
                match FormatPattern::parse(fmt_str) {
                    Ok(pat) => {
                        if let Err(d) = pat.validate_types(|_| Ok(d_decl.value_type.clone())) {
                            diagnostics.push(d);
                        }
                    }
                    Err(d) => diagnostics.push(d),
                }
            }

            let mut local_deps = IndexSet::new();
            match parse_expr(&d_decl.expr) {
                Ok(expr) => {
                    collect_family_local_deps(&expr, fam_name, fam, &mut local_deps);
                    match infer_expr_type(&expr, config, Some((fam_name, fam))) {
                        Ok(inferred_ty) => {
                            if !types_compatible(&inferred_ty, &d_decl.value_type) {
                                diagnostics.push(Diagnostic::error(
                                    DiagnosticCode::CW2001,
                                    format!(
                                        "Derivation `{fam_name}.{d_name}` declared as `{}`, but expression `{}` has type `{inferred_ty}`",
                                        d_decl.value_type, d_decl.expr
                                    ),
                                ));
                            }
                        }
                        Err(d) => diagnostics.push(d),
                    }
                }
                Err(d) => diagnostics.push(d),
            }
            deriv_deps.insert(d_name.clone(), local_deps);
        }

        match topo_sort(&deriv_deps, &format!("family `{fam_name}`")) {
            Ok(order) => {
                family_derivation_order.insert(fam_name.clone(), order);
            }
            Err(d) => diagnostics.push(d),
        }
    }

    // 3. Validate scalar claims and topological evaluation order
    let mut scalar_deps: IndexMap<String, IndexSet<String>> = IndexMap::new();
    for (c_name, c_decl) in &config.claims {
        if let Err(d) = c_decl.deadband_fraction() {
            diagnostics.push(d);
        }
        if let Some(fmt_str) = &c_decl.format {
            match FormatPattern::parse(fmt_str) {
                Ok(pat) => {
                    if let Err(d) = pat.validate_types(|_| Ok(c_decl.value_type.clone())) {
                        diagnostics.push(d);
                    }
                }
                Err(d) => diagnostics.push(d),
            }
        }

        let mut deps = IndexSet::new();
        match c_decl.effective_kind() {
            "derive" => {
                if let Some(expr_str) = &c_decl.expr {
                    match parse_expr(expr_str) {
                        Ok(expr) => {
                            collect_scalar_deps(&expr, config, &mut deps);
                            match infer_expr_type(&expr, config, None) {
                                Ok(inferred) => {
                                    if !types_compatible(&inferred, &c_decl.value_type) {
                                        diagnostics.push(Diagnostic::error(
                                            DiagnosticCode::CW2001,
                                            format!(
                                                "Claim `{c_name}` declared as `{}`, but expression `{expr_str}` has type `{inferred}`",
                                                c_decl.value_type
                                            ),
                                        ));
                                    }
                                }
                                Err(d) => diagnostics.push(d),
                            }
                        }
                        Err(d) => diagnostics.push(d),
                    }
                } else {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!("Derived scalar claim `{c_name}` is missing `expr`"),
                    ));
                }
            }
            "const" => {
                if let Some(val_str) = &c_decl.value {
                    if let Err(d) = CanonicalValue::parse_literal(val_str, &c_decl.value_type) {
                        diagnostics.push(d);
                    }
                }
            }
            "source" => {
                if c_decl.producer.is_none() {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!("Source scalar claim `{c_name}` is missing `producer`"),
                    ));
                }
            }
            other => {
                diagnostics.push(Diagnostic::error(
                    DiagnosticCode::CW2003,
                    format!("Unknown scalar claim kind `{other}` on `{c_name}`"),
                ));
            }
        }
        scalar_deps.insert(c_name.clone(), deps);
    }

    let scalar_eval_order = match topo_sort(&scalar_deps, "scalar claims") {
        Ok(order) => order,
        Err(d) => {
            diagnostics.push(d);
            config.claims.keys().cloned().collect()
        }
    };

    // 4. Validate assertions (must evaluate to Bool)
    for (a_name, a_decl) in &config.assertions {
        match parse_expr(&a_decl.expr) {
            Ok(expr) => match infer_expr_type(&expr, config, None) {
                Ok(ty) => {
                    if ty != ValueType::Bool {
                        diagnostics.push(Diagnostic::error(
                            DiagnosticCode::CW2001,
                            format!(
                                "Assertion `{a_name}` must have type `Bool`, got `{ty}` in `{}`",
                                a_decl.expr
                            ),
                        ));
                    }
                }
                Err(d) => diagnostics.push(d),
            },
            Err(d) => diagnostics.push(d),
        }
    }

    // 5. Validate block lens patterns
    for (b_name, b_decl) in &config.blocks {
        for line_pat in &b_decl.lines {
            match FormatPattern::parse(line_pat) {
                Ok(pat) => {
                    if let Err(d) = pat.validate_types(|target| {
                        let Some(t) = target else {
                            return Err(Diagnostic::error(
                                DiagnosticCode::CW2003,
                                format!("Unnamed placeholder in block `{b_name}`"),
                            ));
                        };
                        let cref = ClaimRef::parse(t)?;
                        resolve_claim_ref_type(&cref, config, None, false)
                    }) {
                        diagnostics.push(d);
                    }
                }
                Err(d) => diagnostics.push(d),
            }
        }
    }

    // 6. Validate table lens patterns
    for (t_name, t_decl) in &config.tables {
        match t_decl.mode {
            TableMode::Records => {
                let Some(fam_name) = &t_decl.family else {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!("Records table lens `{t_name}` is missing `family`"),
                    ));
                    continue;
                };
                let Some(fam_decl) = config.families.get(fam_name) else {
                    diagnostics.push(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!("Table lens `{t_name}` references unknown family `{fam_name}`"),
                    ));
                    continue;
                };
                for (_col, pat_str) in &t_decl.columns {
                    validate_table_pattern(
                        pat_str,
                        config,
                        Some((fam_name, fam_decl)),
                        &mut diagnostics,
                    );
                }
                for (_row_key, overrides) in &t_decl.row_overrides {
                    for (_col, pat_str) in overrides {
                        validate_table_pattern(
                            pat_str,
                            config,
                            Some((fam_name, fam_decl)),
                            &mut diagnostics,
                        );
                    }
                }
            }
            TableMode::Transposed => {
                // Use the first column_scope's family as context if available
                let first_fam = t_decl.column_scopes.values().next().and_then(|cs| {
                    config
                        .families
                        .get(&cs.family)
                        .map(|f| (cs.family.as_str(), f))
                });
                for (_row_key, row_spec) in &t_decl.rows {
                    match row_spec {
                        TableRowSpec::Pattern(pat_str) => {
                            validate_table_pattern(pat_str, config, first_fam, &mut diagnostics);
                        }
                        TableRowSpec::Columns(cols) => {
                            for (_col, pat_str) in cols {
                                validate_table_pattern(
                                    pat_str,
                                    config,
                                    first_fam,
                                    &mut diagnostics,
                                );
                            }
                        }
                    }
                }
                for (col_hdr, overrides) in &t_decl.column_overrides {
                    let col_fam = t_decl
                        .column_scopes
                        .get(col_hdr)
                        .and_then(|cs| {
                            config
                                .families
                                .get(&cs.family)
                                .map(|f| (cs.family.as_str(), f))
                        })
                        .or(first_fam);
                    for (_row_key, pat_str) in overrides {
                        validate_table_pattern(pat_str, config, col_fam, &mut diagnostics);
                    }
                }
            }
            TableMode::Checklist => {
                let opt_fam = t_decl
                    .family
                    .as_deref()
                    .and_then(|fnm| config.families.get(fnm).map(|f| (fnm, f)));
                for (_row_key, row_spec) in &t_decl.rows {
                    match row_spec {
                        TableRowSpec::Pattern(pat_str) => {
                            validate_table_pattern(pat_str, config, opt_fam, &mut diagnostics);
                        }
                        TableRowSpec::Columns(cols) => {
                            for (_col, pat_str) in cols {
                                validate_table_pattern(pat_str, config, opt_fam, &mut diagnostics);
                            }
                        }
                    }
                }
            }
        }
        for (_k, pat_str) in &t_decl.cell_patterns {
            let opt_fam = t_decl
                .family
                .as_deref()
                .and_then(|fnm| config.families.get(fnm).map(|f| (fnm, f)));
            validate_table_pattern(pat_str, config, opt_fam, &mut diagnostics);
        }
    }

    if diagnostics.is_empty() {
        Ok(CheckedIr {
            config: config.clone(),
            parsed_constants,
            family_derivation_order,
            scalar_eval_order,
        })
    } else {
        Err(diagnostics)
    }
}

fn validate_table_pattern(
    pat_str: &str,
    config: &ClaimsConfig,
    family_ctx: Option<(&str, &FamilyDecl)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match FormatPattern::parse(pat_str) {
        Ok(pat) => {
            if let Err(d) = pat.validate_types(|target| {
                let Some(t) = target else {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!(
                            "Placeholder in table pattern `{pat_str}` must name a metric or claim"
                        ),
                    ));
                };
                if let Some(rest) = t.strip_prefix('@') {
                    if let Some((_ep, fld)) = rest.split_once('.') {
                        if let Some((fam_name, fam)) = family_ctx {
                            return resolve_family_field_type(fam_name, fam, fld);
                        }
                    }
                }
                let cref = ClaimRef::parse(t)?;
                resolve_claim_ref_type(&cref, config, family_ctx, false)
            }) {
                diagnostics.push(d);
            }
        }
        Err(d) => diagnostics.push(d),
    }
}

pub fn infer_expr_type(
    expr: &Expr,
    config: &ClaimsConfig,
    family_ctx: Option<(&str, &FamilyDecl)>,
) -> Result<ValueType, Diagnostic> {
    match expr {
        Expr::Number(_) => Ok(ValueType::Float),
        Expr::Bool(_) => Ok(ValueType::Bool),
        Expr::Ref(cref) => resolve_claim_ref_type(cref, config, family_ctx, false),
        Expr::EpochField { field, .. } => {
            let Some((fam_name, fam)) = family_ctx else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2004,
                    format!(
                        "`@.field` shorthand (`{field}`) is only valid inside a family context"
                    ),
                ));
            };
            resolve_family_field_type(fam_name, fam, field)
        }
        Expr::UnaryNeg(inner) => infer_expr_type(inner, config, family_ctx),
        Expr::Not(inner) => {
            let ty = infer_expr_type(inner, config, family_ctx)?;
            if ty != ValueType::Bool {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    format!("Logical `not` requires `Bool`, got `{ty}`"),
                ));
            }
            Ok(ValueType::Bool)
        }
        Expr::Reduction { target, .. } => {
            resolve_claim_ref_type(&ClaimRef::Family(target.clone()), config, family_ctx, true)
        }
        Expr::PrefixSum { field, .. } => {
            let Some((fam_name, fam)) = family_ctx else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2004,
                    "`prefix_sum(...)` is only valid inside a family derivation",
                ));
            };
            resolve_family_field_type(fam_name, fam, field)
        }
        Expr::Round { expr: inner, .. } => infer_expr_type(inner, config, family_ctx),
        Expr::Binary { op, lhs, rhs } => {
            let lty = infer_expr_type(lhs, config, family_ctx)?;
            let rty = infer_expr_type(rhs, config, family_ctx)?;
            check_binary_op_types(*op, &lty, &rty)
        }
    }
}

pub fn resolve_claim_ref_type(
    cref: &ClaimRef,
    config: &ClaimsConfig,
    family_ctx: Option<(&str, &FamilyDecl)>,
    inside_reduction: bool,
) -> Result<ValueType, Diagnostic> {
    match cref {
        ClaimRef::Scalar(sref) => {
            // 1. If inside a family context and `sref` is a single identifier matching a metric or derivation
            if sref.segments.len() == 1 {
                if let Some((fam_name, fam)) = family_ctx {
                    let field = &sref.segments[0];
                    if fam.metrics.contains_key(field) || fam.derivations.contains_key(field) {
                        return resolve_family_field_type(fam_name, fam, field);
                    }
                }
            }
            let full = sref.canonical_string();
            if let Some(c) = config.constants.get(&full) {
                return Ok(c.value_type.clone());
            }
            if let Some(c) = config.claims.get(&full) {
                return Ok(c.value_type.clone());
            }
            Err(Diagnostic::error(
                DiagnosticCode::CW2003,
                format!("Unresolved claim or constant `{full}`"),
            ))
        }
        ClaimRef::Family(fref) => resolve_family_ref_type(fref, config, inside_reduction),
    }
}

fn resolve_family_ref_type(
    fref: &FamilyRef,
    config: &ClaimsConfig,
    inside_reduction: bool,
) -> Result<ValueType, Diagnostic> {
    let Some(fam) = config.families.get(&fref.family) else {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2003,
            format!("Unresolved claim family `{}` in `{fref}`", fref.family),
        ));
    };
    if fref.selectors.len() != fam.dimensions.len() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2003,
            format!(
                "Family `{}` expects {} dimension selectors ({:?}), but `{fref}` has {}",
                fref.family,
                fam.dimensions.len(),
                fam.dimensions,
                fref.selectors.len()
            ),
        ));
    }
    let has_multi = fref.selectors.iter().any(|s| s.is_multi());
    if has_multi && !inside_reduction {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2004,
            format!(
                "Multi-record selector `{fref}` requires an enclosing reduction (`sum`, `min`, or `max`)"
            ),
        ));
    }
    if !has_multi && inside_reduction {
        return Err(Diagnostic::error(
            DiagnosticCode::CW2004,
            format!(
                "Reduction over point selector `{fref}` is invalid: selector must be `*` or `{{k1, k2, ...}}`"
            ),
        ));
    }
    resolve_family_field_type(&fref.family, fam, &fref.field)
}

pub fn resolve_family_field_type(
    fam_name: &str,
    fam: &FamilyDecl,
    field: &str,
) -> Result<ValueType, Diagnostic> {
    if let Some(m) = fam.metrics.get(field) {
        return Ok(m.value_type.clone());
    }
    if let Some(d) = fam.derivations.get(field) {
        return Ok(d.value_type.clone());
    }
    Err(Diagnostic::error(
        DiagnosticCode::CW2003,
        format!("Unknown metric or derivation `{field}` on family `{fam_name}`"),
    ))
}

/// Dimensional algebra (`spec.md` §4.1).
pub fn check_binary_op_types(
    op: BinOp,
    lhs: &ValueType,
    rhs: &ValueType,
) -> Result<ValueType, Diagnostic> {
    use ValueType::*;
    match op {
        BinOp::Add | BinOp::Sub => match (lhs, rhs) {
            (Duration, Duration) => Ok(Duration),
            (Bytes, Bytes) => Ok(Bytes),
            (Cycles, Cycles) => Ok(Cycles),
            (Throughput, Throughput) => Ok(Throughput),
            (Count, Count) => Ok(Count),
            (Ratio | Float, Ratio | Float) => Ok(Ratio),
            (Quantity(u1), Quantity(u2)) if u1 == u2 => Ok(Quantity(u1.clone())),
            _ => Err(Diagnostic::error(
                DiagnosticCode::CW2001,
                format!("Dimensional unit mismatch: cannot apply `{op:?}` to `{lhs}` and `{rhs}`"),
            )),
        },
        BinOp::Mul => match (lhs, rhs) {
            (Ratio | Float, Ratio | Float) => Ok(Ratio),
            (Duration, Ratio | Float) | (Ratio | Float, Duration) => Ok(Duration),
            (Bytes, Ratio | Float) | (Ratio | Float, Bytes) => Ok(Bytes),
            (Cycles, Ratio | Float) | (Ratio | Float, Cycles) => Ok(Cycles),
            (Throughput, Ratio | Float) | (Ratio | Float, Throughput) => Ok(Throughput),
            (Count, Count) => Ok(Count),
            (Count, Ratio | Float) | (Ratio | Float, Count) => Ok(Float),
            (Quantity(u), Ratio | Float) | (Ratio | Float, Quantity(u)) => Ok(Quantity(u.clone())),
            (Throughput, Duration) | (Duration, Throughput) => Ok(Cycles),
            _ => Err(Diagnostic::error(
                DiagnosticCode::CW2001,
                format!("Dimensional unit mismatch: cannot multiply `{lhs}` and `{rhs}`"),
            )),
        },
        BinOp::Div => match (lhs, rhs) {
            (Cycles, Duration) => Ok(Throughput),
            (Duration, Duration) => Ok(Ratio),
            (Bytes, Bytes) => Ok(Ratio),
            (Cycles, Cycles) => Ok(Ratio),
            (Throughput, Throughput) => Ok(Ratio),
            (Count, Count) => Ok(Ratio),
            (Ratio | Float, Ratio | Float) => Ok(Ratio),
            (Quantity(u1), Quantity(u2)) if u1 == u2 => Ok(Ratio),
            (Duration, Ratio | Float) => Ok(Duration),
            (Bytes, Ratio | Float) => Ok(Bytes),
            (Cycles, Ratio | Float) => Ok(Cycles),
            (Throughput, Ratio | Float) => Ok(Throughput),
            (Quantity(u), Ratio | Float) => Ok(Quantity(u.clone())),
            _ => Err(Diagnostic::error(
                DiagnosticCode::CW2001,
                format!("Dimensional unit mismatch: cannot divide `{lhs}` by `{rhs}`"),
            )),
        },
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            if types_compatible(lhs, rhs) {
                Ok(Bool)
            } else {
                Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    format!("Dimensional unit mismatch: cannot compare `{lhs}` with `{rhs}`"),
                ))
            }
        }
        BinOp::And | BinOp::Or => {
            if *lhs == Bool && *rhs == Bool {
                Ok(Bool)
            } else {
                Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    format!("Logical operator requires `Bool` operands, got `{lhs}` and `{rhs}`"),
                ))
            }
        }
    }
}

pub fn types_compatible(a: &ValueType, b: &ValueType) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (a, b),
        (ValueType::Ratio, ValueType::Float)
            | (ValueType::Float, ValueType::Ratio)
            | (ValueType::Count, ValueType::Float)
            | (ValueType::Float, ValueType::Count)
    )
}

fn collect_family_local_deps(
    expr: &Expr,
    fam_name: &str,
    fam: &FamilyDecl,
    deps: &mut IndexSet<String>,
) {
    match expr {
        Expr::Ref(ClaimRef::Scalar(s)) if s.segments.len() == 1 => {
            let fld = &s.segments[0];
            if fam.derivations.contains_key(fld) {
                deps.insert(fld.clone());
            }
        }
        Expr::Ref(ClaimRef::Family(fref))
            if fref.family == fam_name && fref.effective_epoch() == "current" =>
        {
            if fam.derivations.contains_key(&fref.field) {
                deps.insert(fref.field.clone());
            }
        }
        Expr::PrefixSum { epoch: None, field } => {
            if fam.derivations.contains_key(field) {
                deps.insert(field.clone());
            }
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            collect_family_local_deps(e, fam_name, fam, deps);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_family_local_deps(lhs, fam_name, fam, deps);
            collect_family_local_deps(rhs, fam_name, fam, deps);
        }
        Expr::Reduction { target, .. } => {
            if target.family == fam_name
                && target.effective_epoch() == "current"
                && fam.derivations.contains_key(&target.field)
            {
                deps.insert(target.field.clone());
            }
        }
        _ => {}
    }
}

fn collect_scalar_deps(expr: &Expr, config: &ClaimsConfig, deps: &mut IndexSet<String>) {
    match expr {
        Expr::Ref(ClaimRef::Scalar(s)) => {
            let name = s.canonical_string();
            if config.claims.contains_key(&name) {
                deps.insert(name);
            }
        }
        Expr::UnaryNeg(e) | Expr::Not(e) | Expr::Round { expr: e, .. } => {
            collect_scalar_deps(e, config, deps);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_scalar_deps(lhs, config, deps);
            collect_scalar_deps(rhs, config, deps);
        }
        _ => {}
    }
}

fn topo_sort(
    deps_map: &IndexMap<String, IndexSet<String>>,
    context: &str,
) -> Result<Vec<String>, Diagnostic> {
    let mut visited = IndexMap::new(); // 0 = visiting, 1 = visited
    let mut order = Vec::with_capacity(deps_map.len());

    fn dfs(
        node: &str,
        deps_map: &IndexMap<String, IndexSet<String>>,
        visited: &mut IndexMap<String, u8>,
        order: &mut Vec<String>,
        context: &str,
    ) -> Result<(), Diagnostic> {
        match visited.get(node) {
            Some(1) => return Ok(()),
            Some(0) => {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2002,
                    format!("Dependency cycle detected involving `{node}` in {context}"),
                ));
            }
            _ => {}
        }
        visited.insert(node.to_string(), 0);
        if let Some(deps) = deps_map.get(node) {
            for dep in deps {
                dfs(dep, deps_map, visited, order, context)?;
            }
        }
        visited.insert(node.to_string(), 1);
        order.push(node.to_string());
        Ok(())
    }

    for key in deps_map.keys() {
        dfs(key, deps_map, &mut visited, &mut order, context)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cw_syntax::from_yaml_str;

    #[test]
    fn test_dimensional_mismatch_cw2001_and_cycle_cw2002() {
        let bad_yaml = r#"
schema: claimwire/v1
claims:
  a: { kind: derive, expr: "b + 1", type: Duration }
  b: { kind: derive, expr: "a + 1", type: Count }
"#;
        let (cfg, _) = from_yaml_str::<ClaimsConfig>(bad_yaml).unwrap();
        let errs = check_claims_config(&cfg).unwrap_err();
        assert!(errs.iter().any(|d| d.code == DiagnosticCode::CW2001));
        assert!(errs.iter().any(|d| d.code == DiagnosticCode::CW2002));
    }
}
