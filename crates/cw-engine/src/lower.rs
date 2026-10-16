//! Lowering Markdown CSTs against Evaluated IR, Sentinel & Decorator Handling,
//! and Conflict / Context Auditing (`spec.md` §5, §8.2, §8.3).

use crate::eval::{EvalChannel, resolve_claim_ref_value};
use crate::lockfile::{EvidenceLockfile, LockedLocation};
use anyhow::Result;
use cw_evidence::LoadedEvidenceStore;
use cw_ir::{
    CheckedIr, FamilyDecl, TableLensDecl, TableMode, TableRowSpec, resolve_claim_ref_type,
    resolve_family_field_type,
};
use cw_syntax::{
    ByteRange, CanonicalValue, ClaimRef, Diagnostic, DiagnosticCode, FormatPattern, GfmCell,
    GfmRow, MarkdownDocument, OuterDecorator, PatternSegment, Selector, TableLensNode, ValueType,
    blake3_prefixed, canonicalize_table_key, compute_text_context_digest, format_scope_bracketed,
    parse_row_override_key,
};
use indexmap::{IndexMap, IndexSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationKind {
    InlineSpan {
        claim_ref: String,
    },
    TableCell {
        lens_id: String,
        row_label: String,
        col_label: String,
    },
    BlockLine {
        lens_id: String,
        pattern_idx: usize,
    },
}

impl LocationKind {
    pub fn short_description(&self) -> String {
        match self {
            Self::InlineSpan { claim_ref } => format!("span {claim_ref}"),
            Self::TableCell { lens_id, .. } => format!("table {lens_id}"),
            Self::BlockLine { lens_id, .. } => format!("block {lens_id}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BoundLocation {
    /// Canonical lockfile location key (`README.md::table:readme.benchmarks::[radix-sort-u64, test]::Prove`)
    pub id: String,
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub byte_range: ByteRange,
    pub line_text: String,
    pub current_text: String,
    pub expected_text: String,
    pub context_digest: String,
    pub kind: LocationKind,
    /// Canonical claim references (`guest_bench[radix-sort-u64, test].prove_wall`) that feed this location
    pub referenced_claims: Vec<String>,
    /// True if this is a sentinel cell (`—`, `N/A`, `exact`) whose underlying metric is null
    pub is_sentinel: bool,
}

#[derive(Debug, Clone)]
pub struct LoweringResult {
    pub documents: IndexMap<String, MarkdownDocument>,
    pub locations: Vec<BoundLocation>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn discover_markdown_files(workspace_root: &Path, ir: &CheckedIr) -> Result<Vec<String>> {
    let mut files = IndexSet::new();

    if workspace_root.join("README.md").is_file() {
        files.insert("README.md".to_string());
    }
    for b in ir.config.blocks.values() {
        if workspace_root.join(&b.file).is_file() {
            files.insert(b.file.clone());
        }
    }
    for t in ir.config.tables.values() {
        if workspace_root.join(&t.file).is_file() {
            files.insert(t.file.clone());
        }
    }
    let docs_dir = workspace_root.join("docs");
    if docs_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&docs_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && entry.path().extension().and_then(|e| e.to_str()) == Some("md")
            {
                if let Ok(rel) = entry.path().strip_prefix(workspace_root) {
                    let rel_slash = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    if !files.contains(&rel_slash) {
                        if let Ok(content) = fs::read_to_string(entry.path()) {
                            if content.contains("<!-- cw:") {
                                files.insert(rel_slash);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut sorted: Vec<String> = files.into_iter().collect();
    sorted.sort_by(|a, b| {
        if a == "README.md" {
            std::cmp::Ordering::Less
        } else if b == "README.md" {
            std::cmp::Ordering::Greater
        } else {
            a.cmp(b)
        }
    });
    Ok(sorted)
}

pub fn lower_workspace(
    workspace_root: &Path,
    ir: &CheckedIr,
    display_channel: &EvalChannel,
    explicit_files: Option<&[String]>,
) -> Result<LoweringResult> {
    let md_files = match explicit_files {
        Some(list) if !list.is_empty() => list.to_vec(),
        _ => discover_markdown_files(workspace_root, ir)?,
    };

    let mut documents = IndexMap::new();
    let mut diagnostics = Vec::new();
    let mut locations = Vec::new();

    for rel_file in md_files {
        let full_path = workspace_root.join(&rel_file);
        let Ok(source) = fs::read_to_string(&full_path) else {
            continue;
        };
        let doc = MarkdownDocument::parse(&rel_file, source);
        diagnostics.extend(doc.diagnostics.iter().cloned());

        // 1. Lower Block Lenses (`<!-- cw:block <id> -->`)
        for block_node in &doc.blocks {
            lower_block_lens(
                &doc,
                block_node,
                ir,
                display_channel,
                &mut locations,
                &mut diagnostics,
            );
        }

        // 2. Lower Table Lenses (`<!-- cw:table <id> -->`)
        for table_node in &doc.tables {
            lower_table_lens(
                &doc,
                table_node,
                ir,
                display_channel,
                &mut locations,
                &mut diagnostics,
            );
        }

        // 3. Lower Inline Spans (`<!-- cw:... -->...<!-- /cw -->`)
        for span_node in &doc.inline_spans {
            lower_inline_span(
                span_node,
                ir,
                display_channel,
                &mut locations,
                &mut diagnostics,
            );
        }

        documents.insert(rel_file, doc);
    }

    Ok(LoweringResult {
        documents,
        locations,
        diagnostics,
    })
}

fn lower_inline_span(
    span_node: &cw_syntax::InlineSpanNode,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let canonical_ref = span_node.claim_ref.canonical_string();
    let loc_id = format!(
        "{}::span:{}#{}",
        span_node.file, canonical_ref, span_node.occurrence
    );

    let (val, ty) = match resolve_claim_ref_value(&span_node.claim_ref, channel, ir, None) {
        Ok(res) => res,
        Err(mut d) => {
            d.file = Some(span_node.file.clone());
            d.line = Some(span_node.line);
            d.col = Some(span_node.value_col);
            diagnostics.push(d);
            return;
        }
    };

    if val.is_null() {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW2104,
                format!("Missing evidence value for inline span `{canonical_ref}`"),
            )
            .with_location(&span_node.file, span_node.line, span_node.value_col)
            .with_snippet(
                &span_node.line_text,
                span_node.current_text.len(),
                "missing evidence value",
            ),
        );
        return;
    }

    let fmt_str = span_node
        .format_override
        .clone()
        .or_else(|| default_format_for_claim_ref(&span_node.claim_ref, ir))
        .unwrap_or_else(|| default_format_for_type(&ty));

    let pat = match FormatPattern::parse(&fmt_str) {
        Ok(p) => p,
        Err(mut d) => {
            d.file = Some(span_node.file.clone());
            d.line = Some(span_node.line);
            d.col = Some(span_node.value_col);
            diagnostics.push(d);
            return;
        }
    };

    let expected_text = match pat.render(|_| Ok((val.clone(), ty.clone()))) {
        Ok(s) => s,
        Err(mut d) => {
            d.file = Some(span_node.file.clone());
            d.line = Some(span_node.line);
            d.col = Some(span_node.value_col);
            diagnostics.push(d);
            return;
        }
    };

    locations.push(BoundLocation {
        id: loc_id,
        file: span_node.file.clone(),
        line: span_node.line,
        col: span_node.value_col,
        byte_range: span_node.value_range.clone(),
        line_text: span_node.line_text.clone(),
        current_text: span_node.current_text.clone(),
        expected_text,
        context_digest: span_node.context_digest.clone(),
        kind: LocationKind::InlineSpan {
            claim_ref: canonical_ref.clone(),
        },
        referenced_claims: vec![canonical_ref],
        is_sentinel: false,
    });
}

fn lower_block_lens(
    doc: &MarkdownDocument,
    block_node: &cw_syntax::BlockLensNode,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(block_decl) = ir.config.blocks.get(&block_node.lens_id) else {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW2003,
                format!("Unknown block lens `{}`", block_node.lens_id),
            )
            .with_location(&doc.file, block_node.directive_line, 1),
        );
        return;
    };

    for (pat_idx, pat_str) in block_decl.lines.iter().enumerate() {
        let pat = match FormatPattern::parse(pat_str) {
            Ok(p) => p,
            Err(mut d) => {
                d.file = Some(doc.file.clone());
                d.line = Some(block_node.directive_line);
                diagnostics.push(d);
                continue;
            }
        };

        // Find matching lines inside `block_node.lines`
        let mut matched_slices = Vec::new();
        for line_slice in &block_node.lines {
            let line_str = &doc.source[line_slice.start..line_slice.content_end];
            let parse_res = pat.reverse_parse(line_str, |target| {
                let Some(t) = target else {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW2003,
                        "Unnamed placeholder in block pattern",
                    ));
                };
                let cref = ClaimRef::parse(t)?;
                resolve_claim_ref_type(&cref, &ir.config, None, false)
            });
            if parse_res.is_ok() {
                matched_slices.push(line_slice);
            }
        }

        // §5.3: Each line pattern MUST match exactly one line in that block (`CW1006`).
        if matched_slices.len() != 1 {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::CW1006,
                    format!(
                        "Block lens `{}` pattern #{pat_idx} matched {} lines in block (expected exactly 1): `{pat_str}`",
                        block_node.lens_id,
                        matched_slices.len()
                    ),
                )
                .with_location(&doc.file, block_node.directive_line, 1),
            );
            continue;
        }

        let matched = matched_slices[0];
        let current_line_str = doc.source[matched.start..matched.content_end].to_string();
        let mut referenced_claims = Vec::new();

        let expected_line_str = match pat.render(|target| {
            let Some(t) = target else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2003,
                    "Unnamed placeholder in block pattern",
                ));
            };
            let cref = ClaimRef::parse(t)?;
            referenced_claims.push(cref.canonical_string());
            resolve_claim_ref_value(&cref, channel, ir, None)
        }) {
            Ok(s) => s,
            Err(mut d) => {
                d.file = Some(doc.file.clone());
                d.line = Some(matched.line_no);
                diagnostics.push(d);
                continue;
            }
        };

        // Context digest for a block line is the digest of the literal segments of the pattern
        let literal_skeleton: String = pat
            .segments
            .iter()
            .map(|seg| match seg {
                PatternSegment::Literal(l) => l.as_str(),
                PatternSegment::Placeholder(_) => "{{CW}}",
            })
            .collect();
        let ctx_digest = compute_text_context_digest(&literal_skeleton);
        let loc_id = format!("{}::block:{}#{}", doc.file, block_node.lens_id, pat_idx);

        locations.push(BoundLocation {
            id: loc_id,
            file: doc.file.clone(),
            line: matched.line_no,
            col: 1,
            byte_range: ByteRange {
                start: matched.start,
                end: matched.content_end,
            },
            line_text: current_line_str.clone(),
            current_text: current_line_str,
            expected_text: expected_line_str,
            context_digest: ctx_digest,
            kind: LocationKind::BlockLine {
                lens_id: block_node.lens_id.clone(),
                pattern_idx: pat_idx,
            },
            referenced_claims,
            is_sentinel: false,
        });
    }
}

fn lower_table_lens(
    doc: &MarkdownDocument,
    table_node: &TableLensNode,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(table_decl) = ir.config.tables.get(&table_node.lens_id) else {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW2003,
                format!("Unknown table lens `{}`", table_node.lens_id),
            )
            .with_location(&doc.file, table_node.directive_line, 1),
        );
        return;
    };

    if table_node.header.has_broken_code_span {
        return;
    }

    // Build header column name -> index map
    let mut header_map: IndexMap<String, usize> = IndexMap::new();
    for cell in &table_node.header.cells {
        header_map.insert(cell.text.trim().to_string(), cell.col_idx);
    }

    // Check optional `cell_patterns` on header cells (§5.2.4)
    for (pat_key, pat_str) in &table_decl.cell_patterns {
        if let Some(hdr_col_name) = pat_key.strip_prefix("header:") {
            if let Some(cell) = table_node
                .header
                .cells
                .iter()
                .find(|c| c.text.trim().starts_with(hdr_col_name.trim()) || c.text.trim() == hdr_col_name.trim())
            {
                let opt_fam = table_decl
                    .family
                    .as_deref()
                    .and_then(|fnm| ir.config.families.get(fnm).map(|f| (fnm, f)));
                bind_single_table_cell(
                    &doc.file,
                    &table_node.lens_id,
                    "header",
                    hdr_col_name.trim(),
                    &table_node.header,
                    cell,
                    pat_str,
                    table_decl,
                    ir,
                    channel,
                    opt_fam,
                    table_decl.effective_epoch(),
                    None,
                    locations,
                    diagnostics,
                );
            }
        }
    }

    match table_decl.mode {
        TableMode::Records => lower_records_table(
            &doc.file,
            table_node,
            table_decl,
            &header_map,
            ir,
            channel,
            locations,
            diagnostics,
        ),
        TableMode::Transposed => lower_transposed_table(
            &doc.file,
            table_node,
            table_decl,
            &header_map,
            ir,
            channel,
            locations,
            diagnostics,
        ),
        TableMode::Checklist => lower_checklist_table(
            &doc.file,
            table_node,
            table_decl,
            &header_map,
            ir,
            channel,
            locations,
            diagnostics,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_records_table(
    file: &str,
    table_node: &TableLensNode,
    table_decl: &TableLensDecl,
    header_map: &IndexMap<String, usize>,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(fam_name) = &table_decl.family else {
        return;
    };
    let Some(fam_decl) = ir.config.families.get(fam_name) else {
        return;
    };
    let default_epoch = table_decl.effective_epoch();

    // Collect known keys for each dimension across all epochs of `fam_name`
    let mut known_dim_keys: IndexMap<String, Vec<String>> = IndexMap::new();
    if let Some(ep_map) = channel.families.get(fam_name) {
        for records in ep_map.values() {
            for r in records {
                for (dim, val) in &r.scope {
                    let list = known_dim_keys.entry(dim.clone()).or_default();
                    if !list.contains(val) {
                        list.push(val.clone());
                    }
                }
            }
        }
    }

    // Map dimension -> header column index
    let mut dim_to_col_idx: IndexMap<String, usize> = IndexMap::new();
    for (col_hdr, dim_name) in &table_decl.key_columns {
        if let Some(&col_idx) = find_column_index(header_map, col_hdr) {
            dim_to_col_idx.insert(dim_name.clone(), col_idx);
        } else {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::CW2003,
                    format!(
                        "Key column `{col_hdr}` not found in table `{}` headers",
                        table_node.lens_id
                    ),
                )
                .with_location(file, table_node.header.line_no, 1),
            );
            return;
        }
    }

    // Parse row_overrides keys into canonical scope vectors
    let mut parsed_row_overrides: Vec<(Vec<String>, &IndexMap<String, String>)> = Vec::new();
    for (raw_scope_key, col_map) in &table_decl.row_overrides {
        parsed_row_overrides.push((parse_row_override_key(raw_scope_key), col_map));
    }

    for row in &table_node.rows {
        if row.has_broken_code_span {
            // §5.2.1: refuses to bind any lens to a row with CW1008
            continue;
        }

        // Resolve the row's scope across `fam_decl.dimensions`
        let mut row_scope = IndexMap::new();
        let mut scope_vec = Vec::with_capacity(fam_decl.dimensions.len());
        let mut row_ok = true;

        for dim in &fam_decl.dimensions {
            if let Some(&col_idx) = dim_to_col_idx.get(dim) {
                let raw_cell_text = &row.cells[col_idx].text;
                let known = known_dim_keys.get(dim).map(|v| v.as_slice()).unwrap_or(&[]);
                let aliases = ir.config.aliases.get(dim);
                match canonicalize_table_key(raw_cell_text, known, aliases) {
                    Some(canon_key) => {
                        row_scope.insert(dim.clone(), canon_key.clone());
                        scope_vec.push(canon_key);
                    }
                    None => {
                        diagnostics.push(
                            Diagnostic::error(
                                DiagnosticCode::CW2003,
                                format!(
                                    "Unresolved key `{raw_cell_text}` for dimension `{dim}` in table `{}`",
                                    table_node.lens_id
                                ),
                            )
                            .with_location(
                                file,
                                row.line_no,
                                row.cells[col_idx].col_no,
                            )
                            .with_snippet(
                                &row.raw_line,
                                raw_cell_text.len(),
                                format!("unknown `{dim}` key"),
                            ),
                        );
                        row_ok = false;
                        break;
                    }
                }
            } else if let Some(def_val) = table_decl.default_scope.get(dim) {
                row_scope.insert(dim.clone(), def_val.clone());
                scope_vec.push(def_val.clone());
            } else {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::CW2003,
                        format!(
                            "Dimension `{dim}` is neither mapped in `key_columns` nor provided in `default_scope` for table `{}`",
                            table_node.lens_id
                        ),
                    )
                    .with_location(file, table_node.directive_line, 1),
                );
                row_ok = false;
                break;
            }
        }

        if !row_ok {
            continue;
        }

        let scope_bracketed = format_scope_bracketed(&scope_vec);
        let row_override_map = parsed_row_overrides
            .iter()
            .find(|(sv, _)| *sv == scope_vec)
            .map(|(_, m)| *m);

        // Bind each mapped column
        for (col_hdr, base_pat_str) in &table_decl.columns {
            let Some(&col_idx) = find_column_index(header_map, col_hdr) else {
                continue;
            };
            let effective_pat_str = row_override_map
                .and_then(|m| m.get(col_hdr))
                .unwrap_or(base_pat_str);
            let cell = &row.cells[col_idx];

            bind_single_table_cell(
                file,
                &table_node.lens_id,
                &scope_bracketed,
                col_hdr,
                row,
                cell,
                effective_pat_str,
                table_decl,
                ir,
                channel,
                Some((fam_name.as_str(), fam_decl)),
                default_epoch,
                Some(&row_scope),
                locations,
                diagnostics,
            );
        }

        // Also bind any column in `row_override_map` that wasn't in `table_decl.columns`
        if let Some(ov_map) = row_override_map {
            for (col_hdr, ov_pat_str) in ov_map {
                if table_decl.columns.contains_key(col_hdr) {
                    continue;
                }
                let Some(&col_idx) = find_column_index(header_map, col_hdr) else {
                    continue;
                };
                let cell = &row.cells[col_idx];
                bind_single_table_cell(
                    file,
                    &table_node.lens_id,
                    &scope_bracketed,
                    col_hdr,
                    row,
                    cell,
                    ov_pat_str,
                    table_decl,
                    ir,
                    channel,
                    Some((fam_name.as_str(), fam_decl)),
                    default_epoch,
                    Some(&row_scope),
                    locations,
                    diagnostics,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_transposed_table(
    file: &str,
    table_node: &TableLensNode,
    table_decl: &TableLensDecl,
    header_map: &IndexMap<String, usize>,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let key_col_name = table_decl.key_column.as_deref().unwrap_or("Quantity");
    let Some(&key_col_idx) = find_column_index(header_map, key_col_name) else {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW2003,
                format!(
                    "Key column `{key_col_name}` not found in transposed table `{}`",
                    table_node.lens_id
                ),
            )
            .with_location(file, table_node.header.line_no, 1),
        );
        return;
    };

    let known_row_keys: Vec<String> = table_decl.rows.keys().cloned().collect();

    for row in &table_node.rows {
        if row.has_broken_code_span {
            continue;
        }
        let raw_row_key = &row.cells[key_col_idx].text;
        let Some(row_key) = canonicalize_table_key(raw_row_key, &known_row_keys, None) else {
            continue;
        };
        let Some(row_spec) = table_decl.rows.get(&row_key) else {
            continue;
        };

        for (col_hdr, col_scope_decl) in &table_decl.column_scopes {
            let Some(&col_idx) = find_column_index(header_map, col_hdr) else {
                continue;
            };
            let Some(fam_decl) = ir.config.families.get(&col_scope_decl.family) else {
                continue;
            };

            let mut scope_map = IndexMap::new();
            for (dim, val) in fam_decl.dimensions.iter().zip(col_scope_decl.scope.iter()) {
                scope_map.insert(dim.clone(), val.clone());
            }

            let base_pat = match row_spec {
                TableRowSpec::Pattern(p) => Some(p),
                TableRowSpec::Columns(m) => m.get(col_hdr),
            };
            let override_pat = table_decl
                .column_overrides
                .get(col_hdr)
                .and_then(|m| m.get(&row_key));
            let Some(effective_pat_str) = override_pat.or(base_pat) else {
                continue;
            };

            let cell = &row.cells[col_idx];
            bind_single_table_cell(
                file,
                &table_node.lens_id,
                &row_key,
                col_hdr,
                row,
                cell,
                effective_pat_str,
                table_decl,
                ir,
                channel,
                Some((col_scope_decl.family.as_str(), fam_decl)),
                col_scope_decl.effective_epoch(),
                Some(&scope_map),
                locations,
                diagnostics,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_checklist_table(
    file: &str,
    table_node: &TableLensNode,
    table_decl: &TableLensDecl,
    header_map: &IndexMap<String, usize>,
    ir: &CheckedIr,
    channel: &EvalChannel,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let key_col_idx = table_decl
        .key_column
        .as_deref()
        .and_then(|k| find_column_index(header_map, k).copied())
        .unwrap_or(0);

    let known_row_keys: Vec<String> = table_decl.rows.keys().cloned().collect();
    let opt_fam = table_decl
        .family
        .as_deref()
        .and_then(|fnm| ir.config.families.get(fnm).map(|f| (fnm, f)));

    for row in &table_node.rows {
        if row.has_broken_code_span {
            continue;
        }
        let raw_row_key = &row.cells[key_col_idx].text;
        let Some(row_key) = canonicalize_table_key(raw_row_key, &known_row_keys, None) else {
            continue;
        };
        let Some(TableRowSpec::Columns(col_patterns)) = table_decl.rows.get(&row_key) else {
            continue;
        };

        for (col_hdr, pat_str) in col_patterns {
            let Some(&col_idx) = find_column_index(header_map, col_hdr) else {
                continue;
            };
            let cell = &row.cells[col_idx];
            bind_single_table_cell(
                file,
                &table_node.lens_id,
                &row_key,
                col_hdr,
                row,
                cell,
                pat_str,
                table_decl,
                ir,
                channel,
                opt_fam,
                table_decl.effective_epoch(),
                None,
                locations,
                diagnostics,
            );
        }
    }
}

fn find_column_index<'a>(
    header_map: &'a IndexMap<String, usize>,
    target_hdr: &str,
) -> Option<&'a usize> {
    if let Some(idx) = header_map.get(target_hdr) {
        return Some(idx);
    }
    let target_stripped: String = target_hdr
        .chars()
        .filter(|&c| c != '`' && c != '*' && c != '_')
        .collect::<String>()
        .trim()
        .to_string();
    for (k, idx) in header_map {
        let k_stripped: String = k
            .chars()
            .filter(|&c| c != '`' && c != '*' && c != '_')
            .collect::<String>()
            .trim()
            .to_string();
        if k_stripped == target_stripped {
            return Some(idx);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn bind_single_table_cell(
    file: &str,
    lens_id: &str,
    row_label: &str,
    col_label: &str,
    row: &GfmRow,
    cell: &GfmCell,
    pat_str: &str,
    table_decl: &TableLensDecl,
    ir: &CheckedIr,
    channel: &EvalChannel,
    family_ctx: Option<(&str, &FamilyDecl)>,
    default_epoch: &str,
    row_scope: Option<&IndexMap<String, String>>,
    locations: &mut Vec<BoundLocation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let pat = match FormatPattern::parse(pat_str) {
        Ok(p) => p,
        Err(mut d) => {
            d.file = Some(file.to_string());
            d.line = Some(row.line_no);
            d.col = Some(cell.col_no);
            diagnostics.push(d);
            return;
        }
    };

    let loc_id = format!("{file}::table:{lens_id}::{row_label}::{col_label}");

    // Compute row context digest from unmapped/key cells of the row
    let ctx_digest = compute_text_context_digest(&format!("{row_label}::{col_label}"));

    let mut referenced_claims = Vec::new();
    let mut any_null = false;

    let render_res = pat.render(|target| {
        let Some(t) = target else {
            return Err(Diagnostic::error(
                DiagnosticCode::CW2003,
                format!("Unnamed placeholder in table `{lens_id}` pattern `{pat_str}`"),
            ));
        };
        let (val, ty, canon_claim) = resolve_table_placeholder_value(
            t,
            family_ctx,
            default_epoch,
            row_scope,
            channel,
            ir,
        )?;
        if !canon_claim.is_empty() && !referenced_claims.contains(&canon_claim) {
            referenced_claims.push(canon_claim);
        }
        if val.is_null() {
            any_null = true;
            // Return a dummy value so we can see if any placeholder is null
            return Ok((CanonicalValue::Number(0.0), ty));
        }
        Ok((val, ty))
    });

    if any_null {
        // §5.2.3 Sentinel Cell Handling: if the underlying metric is null and the cell contains an allowed sentinel,
        // preserve it without emitting CW2104!
        if table_decl.is_sentinel(&cell.text) {
            locations.push(BoundLocation {
                id: loc_id,
                file: file.to_string(),
                line: row.line_no,
                col: cell.col_no,
                byte_range: cell.content_range.clone(),
                line_text: row.raw_line.clone(),
                current_text: cell.text.clone(),
                expected_text: cell.text.clone(),
                context_digest: ctx_digest,
                kind: LocationKind::TableCell {
                    lens_id: lens_id.to_string(),
                    row_label: row_label.to_string(),
                    col_label: col_label.to_string(),
                },
                referenced_claims,
                is_sentinel: true,
            });
        } else {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::CW2104,
                    format!(
                        "Missing evidence record for non-sentinel cell `{row_label}` / `{col_label}` in table `{lens_id}`"
                    ),
                )
                .with_location(file, row.line_no, cell.col_no)
                .with_snippet(&row.raw_line, cell.text.len(), "missing evidence record"),
            );
        }
        return;
    }

    let raw_rendered = match render_res {
        Ok(s) => s,
        Err(mut d) => {
            d.file = Some(file.to_string());
            d.line = Some(row.line_no);
            d.col = Some(cell.col_no);
            diagnostics.push(d);
            return;
        }
    };

    // §5.2.2 Step 3: Decorator Preservation on Write
    let (outer_dec, _) = OuterDecorator::detect(&cell.text);
    let expected_text = outer_dec.apply(&raw_rendered, pat.has_outer_decorator_chars());

    locations.push(BoundLocation {
        id: loc_id,
        file: file.to_string(),
        line: row.line_no,
        col: cell.col_no,
        byte_range: cell.content_range.clone(),
        line_text: row.raw_line.clone(),
        current_text: cell.text.clone(),
        expected_text,
        context_digest: ctx_digest,
        kind: LocationKind::TableCell {
            lens_id: lens_id.to_string(),
            row_label: row_label.to_string(),
            col_label: col_label.to_string(),
        },
        referenced_claims,
        is_sentinel: false,
    });
}

fn resolve_table_placeholder_value(
    target: &str,
    family_ctx: Option<(&str, &FamilyDecl)>,
    default_epoch: &str,
    row_scope: Option<&IndexMap<String, String>>,
    channel: &EvalChannel,
    ir: &CheckedIr,
) -> Result<(CanonicalValue, ValueType, String), Diagnostic> {
    // Check `@epoch.field` shorthand
    if let Some(rest) = target.strip_prefix('@') {
        if let Some((ep, fld)) = rest.split_once('.') {
            if let (Some((fam_name, fam_decl)), Some(scope)) = (family_ctx, row_scope) {
                return lookup_family_record_field(fam_name, fam_decl, ep, scope, fld, channel);
            }
        }
    }

    let cref = ClaimRef::parse(target)?;
    match &cref {
        ClaimRef::Scalar(sref) if sref.segments.len() == 1 => {
            let field = &sref.segments[0];
            if let (Some((fam_name, fam_decl)), Some(scope)) = (family_ctx, row_scope) {
                if fam_decl.metrics.contains_key(field) || fam_decl.derivations.contains_key(field)
                {
                    return lookup_family_record_field(
                        fam_name,
                        fam_decl,
                        default_epoch,
                        scope,
                        field,
                        channel,
                    );
                }
            }
            let (val, ty) = resolve_claim_ref_value(&cref, channel, ir, None)?;
            Ok((val, ty, cref.canonical_string()))
        }
        ClaimRef::Scalar(_) => {
            let (val, ty) = resolve_claim_ref_value(&cref, channel, ir, None)?;
            Ok((val, ty, cref.canonical_string()))
        }
        ClaimRef::Family(fref) => {
            let Some(target_fam) = ir.config.families.get(&fref.family) else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2003,
                    format!("Unknown family `{}`", fref.family),
                ));
            };
            let epoch = fref.epoch.as_deref().unwrap_or(default_epoch);
            let mut resolved_scope = IndexMap::new();
            for (dim, sel) in target_fam.dimensions.iter().zip(fref.selectors.iter()) {
                if let Selector::Point(k) = sel {
                    let v = if !k.was_quoted {
                        row_scope
                            .and_then(|s| s.get(&k.value))
                            .cloned()
                            .unwrap_or_else(|| k.value.clone())
                    } else {
                        k.value.clone()
                    };
                    resolved_scope.insert(dim.clone(), v);
                }
            }
            lookup_family_record_field(
                &fref.family,
                target_fam,
                epoch,
                &resolved_scope,
                &fref.field,
                channel,
            )
        }
    }
}

fn lookup_family_record_field(
    fam_name: &str,
    fam_decl: &FamilyDecl,
    epoch: &str,
    scope: &IndexMap<String, String>,
    field: &str,
    channel: &EvalChannel,
) -> Result<(CanonicalValue, ValueType, String), Diagnostic> {
    let ty = resolve_family_field_type(fam_name, fam_decl, field)?;
    let canon_key =
        crate::eval::make_family_claim_key(fam_name, epoch, &fam_decl.dimensions, scope, field);

    let Some(records) = channel.families.get(fam_name).and_then(|m| m.get(epoch)) else {
        return Ok((CanonicalValue::Null, ty, canon_key));
    };
    let Some(rec) = records.iter().find(|r| &r.scope == scope) else {
        return Ok((CanonicalValue::Null, ty, canon_key));
    };
    match rec.values.get(field) {
        Some((v, t)) => Ok((v.clone(), t.clone(), canon_key)),
        None => Ok((CanonicalValue::Null, ty, canon_key)),
    }
}

fn default_format_for_claim_ref(cref: &ClaimRef, ir: &CheckedIr) -> Option<String> {
    match cref {
        ClaimRef::Scalar(s) => {
            let name = s.canonical_string();
            if let Some(c) = ir.config.constants.get(&name) {
                return c.format.clone();
            }
            if let Some(c) = ir.config.claims.get(&name) {
                return c.format.clone();
            }
            None
        }
        ClaimRef::Family(fref) => {
            let fam = ir.config.families.get(&fref.family)?;
            if let Some(m) = fam.metrics.get(&fref.field) {
                return m.format.clone();
            }
            if let Some(d) = fam.derivations.get(&fref.field) {
                return d.format.clone();
            }
            None
        }
    }
}

fn default_format_for_type(ty: &ValueType) -> String {
    match ty {
        ValueType::Duration => "{:.2f s}".to_string(),
        ValueType::Bytes => "{:, B}".to_string(),
        ValueType::Cycles => "{:,}".to_string(),
        ValueType::Throughput => "{:.0f cycles/s}".to_string(),
        ValueType::Count => "{}".to_string(),
        ValueType::PassRatio => "{passed}/{total}".to_string(),
        ValueType::Ratio | ValueType::Float => "{:.2f}".to_string(),
        ValueType::Quantity(u) => format!("{{:.1f {u}}}"),
        ValueType::Bool => "{}".to_string(),
        ValueType::String | ValueType::Digest => "{}".to_string(),
    }
}

/// Audit all bound locations against `EvidenceLockfile` (`spec.md` §8.2, §8.3):
/// - `CW4001` (Manual Edit Conflict): `current_digest != lockfile.rendered_digest`
/// - `CW4002` (Surrounding Context Drift): `current_context_digest != lockfile.context_digest`
/// - `CW3003` (Out of Sync): `current_text != expected_text` (only when `CW4001` does not fire on this location)
pub fn audit_locations(
    locations: &[BoundLocation],
    lockfile: Option<&EvidenceLockfile>,
    ir: &CheckedIr,
    evidence: &LoadedEvidenceStore,
    emit_out_of_sync_cw3003: bool,
    force: bool,
    ack_context: bool,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for loc in locations {
        let cur_digest = blake3_prefixed(loc.current_text.as_bytes());
        let mut has_cw4001 = false;

        if let Some(lock) = lockfile {
            if let Some(locked_loc) = lock.locations.get(&loc.id) {
                if !force && locked_loc.rendered_digest != cur_digest {
                    has_cw4001 = true;
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::CW4001,
                            format!(
                                "Unapproved manual edit inside owned location `{}` (locked `{}`, found `{}`)",
                                loc.id, locked_loc.rendered, loc.current_text
                            ),
                        )
                        .with_location(&loc.file, loc.line, loc.col)
                        .with_snippet(
                            &loc.line_text,
                            loc.current_text.len(),
                            format!("manually edited (lockfile expects `{}`)", locked_loc.rendered),
                        )
                        .with_help("run `cw sync --force` to overwrite manual edit from evidence"),
                    );
                }

                if !ack_context && locked_loc.context_digest != loc.context_digest {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::CW4002,
                            format!(
                                "Surrounding prose context drifted from locked `context_digest` around `{}`",
                                loc.id
                            ),
                        )
                        .with_location(&loc.file, loc.line, loc.col)
                        .with_snippet(
                            &loc.line_text,
                            loc.current_text.len(),
                            "surrounding prose changed while claim binding remained unchanged",
                        )
                        .with_help(
                            "update the claim binding if the statement changed, or run `cw sync --ack-context` to acknowledge the prose edit",
                        ),
                    );
                }
            }
        }

        // §8.2 Precedence: When CW4001 applies, only CW4001 is reported, not CW3003.
        if emit_out_of_sync_cw3003 && !has_cw4001 && loc.current_text != loc.expected_text {
            let primary_claim = loc
                .referenced_claims
                .first()
                .cloned()
                .unwrap_or_else(|| loc.id.clone());

            let mut diag = Diagnostic::error(
                DiagnosticCode::CW3003,
                format!("Markdown value out of sync with claim `{primary_claim}`"),
            )
            .with_location(&loc.file, loc.line, loc.col)
            .with_snippet(
                &loc.line_text,
                loc.current_text.len(),
                format!("evidence says {}", loc.expected_text),
            );

            // Attach provenance note (`= evidence: om.inventory@current · run inventory-20261016 · commit f8382f3`)
            if let Some(ev_note) = format_evidence_note(&primary_claim, ir, evidence) {
                diag.notes.push(ev_note);
            }

            // Attach cross-reference note (`= also rendered at: ...`)
            let other_locs: Vec<&BoundLocation> = locations
                .iter()
                .filter(|other| {
                    other.id != loc.id
                        && other.referenced_claims.contains(&primary_claim)
                })
                .collect();
            if !other_locs.is_empty() {
                let mut lines = Vec::new();
                for (i, ol) in other_locs.iter().enumerate() {
                    let status_mark = if ol.current_text == ol.expected_text {
                        "✓"
                    } else {
                        "✗"
                    };
                    let coord = format!("{}:{}", ol.file, ol.line);
                    let desc = format!("({})", ol.kind.short_description());
                    if i == 0 {
                        lines.push(format!(
                            "also rendered at: {coord:<22} {desc:<33} {status_mark}"
                        ));
                    } else {
                        lines.push(format!(
                            "                  {coord:<22} {desc:<33} {status_mark}"
                        ));
                    }
                }
                diag.notes.push(lines.join("\n"));
            }

            diag.help = Some("run `cw sync` to rewrite this span from evidence".to_string());
            diagnostics.push(diag);
        }
    }

    diagnostics
}

fn format_evidence_note(
    claim_key: &str,
    ir: &CheckedIr,
    evidence: &LoadedEvidenceStore,
) -> Option<String> {
    let cref = ClaimRef::parse(claim_key).ok()?;
    let (prod_id, epoch) = match cref {
        ClaimRef::Scalar(s) => {
            let name = s.canonical_string();
            let decl = ir.config.claims.get(&name)?;
            (decl.producer.clone()?, "current".to_string())
        }
        ClaimRef::Family(fref) => {
            let fam = ir.config.families.get(&fref.family)?;
            (
                fam.producer.clone(),
                fref.effective_epoch().to_string(),
            )
        }
    };
    let manifest = evidence.get_manifest(&prod_id, &epoch)?;
    Some(format!(
        "evidence: {prod_id}@{epoch} · run {} · commit {}",
        manifest.run_id, manifest.git_commit
    ))
}

pub fn build_locked_locations(
    locations: &[BoundLocation],
    use_expected_text: bool,
) -> IndexMap<String, LockedLocation> {
    let mut map = IndexMap::new();
    for loc in locations {
        let text = if use_expected_text {
            &loc.expected_text
        } else {
            &loc.current_text
        };
        map.insert(
            loc.id.clone(),
            LockedLocation {
                rendered: text.clone(),
                rendered_digest: blake3_prefixed(text.as_bytes()),
                context_digest: loc.context_digest.clone(),
            },
        );
    }
    map
}
