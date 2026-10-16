//! Lossless Markdown CST, GFM-Conformant Table Row Lexer (`spec.md` §5.2.1),
//! Inline Spans (§5.1), Table Lenses (§5.2), and Block Pattern Lenses (§5.3).

use crate::addressing::ClaimRef;
use crate::blake3::blake3_prefixed;
use crate::diagnostics::{Diagnostic, DiagnosticCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSlice {
    /// 1-indexed line number in the document
    pub line_no: usize,
    /// Byte offset of the start of the line
    pub start: usize,
    /// Byte offset of the end of the line content (excluding `\r\n` or `\n`)
    pub content_end: usize,
    /// Byte offset including line ending
    pub full_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineSpanNode {
    pub file: String,
    /// 1-indexed line number
    pub line: usize,
    /// 1-indexed column of the inner value start
    pub value_col: usize,
    /// Raw claim ref (and optional inline format after `|`, or pure `ClaimRef`)
    pub raw_directive: String,
    pub claim_ref: ClaimRef,
    pub format_override: Option<String>,
    /// Occurrence index (0-based) for `file::span:<canonical_ref>#<occurrence>`
    pub occurrence: usize,
    /// Exact byte range of the inner value between `<!-- cw:... -->` and `<!-- /cw -->`
    pub value_range: ByteRange,
    /// Current rendered text inside the span
    pub current_text: String,
    /// Enclosing line text (for diagnostic snippets)
    pub line_text: String,
    /// Normalized context digest of the enclosing line/sentence outside owned spans (§8.3)
    pub context_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GfmCell {
    /// 0-indexed column index
    pub col_idx: usize,
    /// 1-indexed column number in the line where the cell content starts
    pub col_no: usize,
    /// Exact byte range in the document of the trimmed cell content (or insertion point if empty)
    pub content_range: ByteRange,
    /// Trimmed text of the cell
    pub text: String,
    /// Inline spans found inside this cell (for unmapped prose columns or headers, §5.2.4)
    pub inline_spans: Vec<InlineSpanNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GfmRow {
    pub line_no: usize,
    pub line_range: ByteRange,
    pub cells: Vec<GfmCell>,
    /// True if `CW1008` (unescaped pipe splits an open code span) was triggered on this row
    pub has_broken_code_span: bool,
    pub raw_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableLensNode {
    pub file: String,
    pub lens_id: String,
    pub directive_line: usize,
    pub header: GfmRow,
    pub delimiter_line: usize,
    pub rows: Vec<GfmRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLensNode {
    pub file: String,
    pub lens_id: String,
    pub directive_line: usize,
    pub is_fenced_code: bool,
    /// The lines inside the bound block (excluding fence delimiters if fenced code block)
    pub lines: Vec<LineSlice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownDocument {
    pub file: String,
    pub source: String,
    pub lines: Vec<LineSlice>,
    pub inline_spans: Vec<InlineSpanNode>,
    pub tables: Vec<TableLensNode>,
    pub blocks: Vec<BlockLensNode>,
    pub diagnostics: Vec<Diagnostic>,
}

impl MarkdownDocument {
    /// Parse a Markdown document losslessly, building CST nodes for inline spans, table lenses,
    /// block lenses, and checking GFM pipe-table lexing (`CW1008`).
    pub fn parse(file: impl Into<String>, source: impl Into<String>) -> Self {
        let file = file.into();
        let source = source.into();
        let lines = split_lines_lossless(&source);
        let mut diagnostics = Vec::new();
        let mut inline_spans = Vec::new();
        let mut tables = Vec::new();
        let mut blocks = Vec::new();
        let mut span_counts: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();

        let mut i = 0;
        let mut in_fence: Option<(u8, usize)> = None; // (fence_char, fence_len)

        while i < lines.len() {
            let line_slice = &lines[i];
            let line_str = &source[line_slice.start..line_slice.content_end];
            let trimmed = line_str.trim_start();

            // Check fenced code block boundary
            if let Some((f_char, f_len)) = in_fence {
                if let Some((c, len, rest)) = detect_code_fence(trimmed) {
                    if c == f_char && len >= f_len && rest.trim().is_empty() {
                        in_fence = None;
                    }
                }
                i += 1;
                continue;
            } else if let Some((c, len, _)) = detect_code_fence(trimmed) {
                in_fence = Some((c, len));
                i += 1;
                continue;
            }

            // Check if this line is a standalone `<!-- cw:table <id> -->` or `<!-- cw:block <id> -->` directive
            if let Some(directive) = parse_standalone_cw_directive(trimmed) {
                match directive {
                    StandaloneDirective::Table(lens_id) => {
                        if lens_id.is_empty() {
                            diagnostics.push(
                                Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Empty lens identifier in `<!-- cw:table -->`",
                                )
                                .with_location(&file, line_slice.line_no, 1)
                                .with_snippet(
                                    line_str,
                                    line_str.len(),
                                    "missing table lens id",
                                ),
                            );
                            i += 1;
                            continue;
                        }
                        let directive_line = line_slice.line_no;
                        i += 1;
                        // Skip blank lines if any, or expect table immediately
                        if i + 1 >= lines.len()
                            || !looks_like_pipe_row(&source[lines[i].start..lines[i].content_end])
                            || !is_gfm_delimiter_row(
                                &source[lines[i + 1].start..lines[i + 1].content_end],
                            )
                        {
                            diagnostics.push(
                                Diagnostic::error(
                                    DiagnosticCode::CW1005,
                                    format!(
                                        "Expected a valid GFM pipe table immediately following `<!-- cw:table {lens_id} -->`"
                                    ),
                                )
                                .with_location(&file, directive_line, 1)
                                .with_snippet(line_str, line_str.len(), "table directive here"),
                            );
                            continue;
                        }

                        let header_slice = &lines[i];
                        let header = lex_gfm_row(
                            &file,
                            &source,
                            header_slice,
                            None,
                            &mut span_counts,
                            &mut inline_spans,
                            &mut diagnostics,
                        );
                        let col_count = header.cells.len();
                        let delimiter_line = lines[i + 1].line_no;
                        i += 2;

                        let mut body_rows = Vec::new();
                        while i < lines.len() {
                            let row_slice = &lines[i];
                            let row_str = &source[row_slice.start..row_slice.content_end];
                            if row_str.trim().is_empty() || !looks_like_pipe_row(row_str) {
                                break;
                            }
                            let row = lex_gfm_row(
                                &file,
                                &source,
                                row_slice,
                                Some(col_count),
                                &mut span_counts,
                                &mut inline_spans,
                                &mut diagnostics,
                            );
                            body_rows.push(row);
                            i += 1;
                        }

                        tables.push(TableLensNode {
                            file: file.clone(),
                            lens_id,
                            directive_line,
                            header,
                            delimiter_line,
                            rows: body_rows,
                        });
                        continue;
                    }
                    StandaloneDirective::Block(lens_id) => {
                        if lens_id.is_empty() {
                            diagnostics.push(
                                Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Empty lens identifier in `<!-- cw:block -->`",
                                )
                                .with_location(&file, line_slice.line_no, 1)
                                .with_snippet(
                                    line_str,
                                    line_str.len(),
                                    "missing block lens id",
                                ),
                            );
                            i += 1;
                            continue;
                        }
                        let directive_line = line_slice.line_no;
                        i += 1;
                        while i < lines.len()
                            && source[lines[i].start..lines[i].content_end]
                                .trim()
                                .is_empty()
                        {
                            i += 1;
                        }
                        if i >= lines.len() {
                            diagnostics.push(
                                Diagnostic::error(
                                    DiagnosticCode::CW1006,
                                    format!(
                                        "No CommonMark block found after `<!-- cw:block {lens_id} -->`"
                                    ),
                                )
                                .with_location(&file, directive_line, 1),
                            );
                            continue;
                        }

                        let first_block_line =
                            source[lines[i].start..lines[i].content_end].trim_start();
                        let mut block_lines = Vec::new();
                        let mut is_fenced = false;

                        if let Some((f_char, f_len, _)) = detect_code_fence(first_block_line) {
                            is_fenced = true;
                            i += 1; // skip opening fence
                            while i < lines.len() {
                                let bl = &lines[i];
                                let bl_str = source[bl.start..bl.content_end].trim_start();
                                if let Some((c, len, rest)) = detect_code_fence(bl_str) {
                                    if c == f_char && len >= f_len && rest.trim().is_empty() {
                                        i += 1; // consume closing fence
                                        break;
                                    }
                                }
                                block_lines.push(bl.clone());
                                i += 1;
                            }
                        } else {
                            // Paragraph, list, or blockquote until blank line
                            while i < lines.len() {
                                let bl = &lines[i];
                                let bl_str = &source[bl.start..bl.content_end];
                                if bl_str.trim().is_empty() {
                                    break;
                                }
                                block_lines.push(bl.clone());
                                i += 1;
                            }
                        }

                        blocks.push(BlockLensNode {
                            file: file.clone(),
                            lens_id,
                            directive_line,
                            is_fenced_code: is_fenced,
                            lines: block_lines,
                        });
                        continue;
                    }
                    StandaloneDirective::Malformed(msg) => {
                        diagnostics.push(
                            Diagnostic::error(DiagnosticCode::CW1001, msg)
                                .with_location(&file, line_slice.line_no, 1)
                                .with_snippet(line_str, line_str.len(), "malformed directive"),
                        );
                        i += 1;
                        continue;
                    }
                }
            }

            // Also check unbound GFM pipe tables for CW1008 and inline spans!
            if i + 1 < lines.len()
                && looks_like_pipe_row(line_str)
                && is_gfm_delimiter_row(&source[lines[i + 1].start..lines[i + 1].content_end])
            {
                let header = lex_gfm_row(
                    &file,
                    &source,
                    &lines[i],
                    None,
                    &mut span_counts,
                    &mut inline_spans,
                    &mut diagnostics,
                );
                let col_count = header.cells.len();
                i += 2;
                while i < lines.len() {
                    let row_slice = &lines[i];
                    let row_str = &source[row_slice.start..row_slice.content_end];
                    if row_str.trim().is_empty() || !looks_like_pipe_row(row_str) {
                        break;
                    }
                    let _ = lex_gfm_row(
                        &file,
                        &source,
                        row_slice,
                        Some(col_count),
                        &mut span_counts,
                        &mut inline_spans,
                        &mut diagnostics,
                    );
                    i += 1;
                }
                continue;
            }

            // Regular prose line: scan for inline `<!-- cw:... -->...<!-- /cw -->` spans outside inline code
            scan_inline_spans_in_slice(
                &file,
                &source,
                line_slice.line_no,
                line_slice.start,
                line_slice.start,
                line_slice.content_end,
                line_str,
                &mut span_counts,
                &mut inline_spans,
                &mut diagnostics,
            );

            i += 1;
        }

        Self {
            file,
            source,
            lines,
            inline_spans,
            tables,
            blocks,
            diagnostics,
        }
    }
}

enum StandaloneDirective {
    Table(String),
    Block(String),
    Malformed(String),
}

fn parse_standalone_cw_directive(trimmed_line: &str) -> Option<StandaloneDirective> {
    let t = trimmed_line.trim_end();
    if !t.starts_with("<!--") || !t.ends_with("-->") {
        return None;
    }
    // If there are multiple comments on the line (e.g. an inline span `<!-- cw:x -->1<!-- /cw -->`), it's NOT a standalone directive!
    let inner = t[4..t.len() - 3].trim();
    if inner.contains("-->") || inner.contains("<!--") {
        return None;
    }
    let Some(after_cw) = inner.strip_prefix("cw:") else {
        return None;
    };
    if let Some(rest) = after_cw.strip_prefix("table") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(StandaloneDirective::Table(rest.trim().to_string()));
        }
    }
    if let Some(rest) = after_cw.strip_prefix("block") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(StandaloneDirective::Block(rest.trim().to_string()));
        }
    }
    Some(StandaloneDirective::Malformed(format!(
        "Unclosed or unrecognized standalone directive `<!-- {inner} -->`"
    )))
}

fn detect_code_fence(trimmed_start: &str) -> Option<(u8, usize, &str)> {
    let bytes = trimmed_start.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let first = bytes[0];
    if first != b'`' && first != b'~' {
        return None;
    }
    let len = bytes.iter().take_while(|&&b| b == first).count();
    if len >= 3 {
        Some((first, len, &trimmed_start[len..]))
    } else {
        None
    }
}

fn looks_like_pipe_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') || t.contains('|')
}

pub fn is_gfm_delimiter_row(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('|') || !t.contains('-') {
        return false;
    }
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cells: Vec<&str> = inner.split('|').collect();
    if cells.is_empty() {
        return false;
    }
    for cell in cells {
        let c = cell.trim();
        if c.is_empty() {
            return false;
        }
        let c = c.strip_prefix(':').unwrap_or(c);
        let c = c.strip_suffix(':').unwrap_or(c);
        if c.is_empty() || !c.bytes().all(|b| b == b'-') {
            return false;
        }
    }
    true
}

/// Normative GFM Table Row Lexer State Machine (`spec.md` §5.2.1):
/// - Strip one leading and one trailing border pipe (`|`) and surrounding whitespace.
/// - Scan UTF-8 bytes left-to-right:
///   - On `\|`: consume both bytes as part of the current cell.
///   - On any other `\` followed by a character: consume both bytes as part of the cell.
///   - On `|`: end the current cell and start the next one, **regardless of backticks**.
///   - Dollar signs (`$`) and math delimiters NEVER mask `|` delimiters.
/// - Lint `CW1008` (Pipe Splits Code Span): If a backtick run is still open when an unescaped `|`
///   ends a cell, emit `CW1008` with a suggested `\|` fix and mark `has_broken_code_span = true`.
pub fn lex_gfm_row(
    file: &str,
    source: &str,
    row_slice: &LineSlice,
    expected_cols: Option<usize>,
    span_counts: &mut indexmap::IndexMap<String, usize>,
    all_inline_spans: &mut Vec<InlineSpanNode>,
    diagnostics: &mut Vec<Diagnostic>,
) -> GfmRow {
    let line_start = row_slice.start;
    let line_end = row_slice.content_end;
    let raw_line = &source[line_start..line_end];

    // Trim outer whitespace and one leading + one trailing border pipe
    let mut scan_start = line_start;
    let mut scan_end = line_end;
    while scan_start < scan_end && source.as_bytes()[scan_start].is_ascii_whitespace() {
        scan_start += 1;
    }
    while scan_end > scan_start && source.as_bytes()[scan_end - 1].is_ascii_whitespace() {
        scan_end -= 1;
    }
    if scan_start < scan_end && source.as_bytes()[scan_start] == b'|' {
        scan_start += 1;
    }
    // Strip trailing `|` only if it is not escaped `\|`
    if scan_end > scan_start && source.as_bytes()[scan_end - 1] == b'|' {
        let mut backslashes = 0usize;
        let mut k = scan_end - 1;
        while k > scan_start && source.as_bytes()[k - 1] == b'\\' {
            backslashes += 1;
            k -= 1;
        }
        if backslashes % 2 == 0 {
            scan_end -= 1;
        }
    }

    let bytes = source.as_bytes();
    let mut raw_cell_ranges: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let mut cell_start = scan_start;
    let mut open_backtick_len: Option<(usize, usize)> = None; // (tick_len, open_byte_pos)
    let mut broken_pipe_positions: Vec<usize> = Vec::new();

    let mut idx = scan_start;
    while idx < scan_end {
        let b = bytes[idx];
        if b == b'\\' && idx + 1 < scan_end {
            idx += 2;
            continue;
        }
        if b == b'`' {
            let mut tick_len = 1;
            while idx + tick_len < scan_end && bytes[idx + tick_len] == b'`' {
                tick_len += 1;
            }
            match open_backtick_len {
                None => {
                    open_backtick_len = Some((tick_len, idx));
                }
                Some((open_len, _)) if open_len == tick_len => {
                    open_backtick_len = None;
                }
                Some(_) => {}
            }
            idx += tick_len;
            continue;
        }
        if b == b'|' {
            let split_whilst_open = open_backtick_len.map(|(_, pos)| pos);
            if split_whilst_open.is_some() {
                broken_pipe_positions.push(idx);
            }
            raw_cell_ranges.push((cell_start, idx, split_whilst_open));
            cell_start = idx + 1;
        }
        idx += 1;
    }
    raw_cell_ranges.push((cell_start, scan_end, None));

    // Only emit CW1008 if a backtick span opened before `|` actually closed later on the line (or remained open across a pipe)
    // Wait: in `04-pcs.md:245`, `` `2k/|K|` `` opens with 1 backtick, hits two `|`, and closes with 1 backtick!
    // What if a cell has a lone literal `` ` `` that never closes? In `04-pcs.md:245` it closes (`open_backtick_len` becomes `None` at the closing backtick).
    let has_broken_code_span = !broken_pipe_positions.is_empty();
    if let Some(&pipe_pos) = broken_pipe_positions.first() {
        let col_1 = (pipe_pos - line_start) + 1;
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW1008,
                "Unescaped pipe (`|`) splits a code span in a GFM table row",
            )
            .with_location(file, row_slice.line_no, col_1)
            .with_snippet(raw_line, 1, "pipe splits code span before closing backtick")
            .with_help("escape the pipe as `\\|` so GitHub does not split the cell"),
        );
    }

    let max_cols = expected_cols.unwrap_or(raw_cell_ranges.len());
    let mut cells = Vec::with_capacity(max_cols);

    for (col_idx, &(c_start, c_end, _)) in raw_cell_ranges.iter().take(max_cols).enumerate() {
        let mut t_start = c_start;
        let mut t_end = c_end;
        while t_start < t_end && bytes[t_start].is_ascii_whitespace() {
            t_start += 1;
        }
        while t_end > t_start && bytes[t_end - 1].is_ascii_whitespace() {
            t_end -= 1;
        }
        // If the cell was completely empty or whitespace, anchor insertion at the middle/start of the cell padding
        if t_start == t_end && c_start < c_end {
            t_start = c_start + 1;
            t_end = t_start;
        }
        let col_no = (t_start - line_start) + 1;
        let cell_text = source[t_start..t_end].to_string();

        let mut cell_spans = Vec::new();
        if cell_text.contains("<!--") {
            scan_inline_spans_in_slice(
                file,
                source,
                row_slice.line_no,
                line_start,
                t_start,
                t_end,
                raw_line,
                span_counts,
                &mut cell_spans,
                diagnostics,
            );
            all_inline_spans.extend(cell_spans.iter().cloned());
        }

        cells.push(GfmCell {
            col_idx,
            col_no,
            content_range: ByteRange {
                start: t_start,
                end: t_end,
            },
            text: cell_text,
            inline_spans: cell_spans,
        });
    }

    // Pad missing cells for short rows as in GFM
    while cells.len() < max_cols {
        let col_idx = cells.len();
        cells.push(GfmCell {
            col_idx,
            col_no: (scan_end - line_start) + 1,
            content_range: ByteRange {
                start: scan_end,
                end: scan_end,
            },
            text: String::new(),
            inline_spans: Vec::new(),
        });
    }

    GfmRow {
        line_no: row_slice.line_no,
        line_range: ByteRange {
            start: line_start,
            end: line_end,
        },
        cells,
        has_broken_code_span,
        raw_line: raw_line.to_string(),
    }
}

fn scan_inline_spans_in_slice(
    file: &str,
    source: &str,
    line_no: usize,
    line_start: usize,
    slice_start: usize,
    slice_end: usize,
    full_line_str: &str,
    span_counts: &mut indexmap::IndexMap<String, usize>,
    spans_out: &mut Vec<InlineSpanNode>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let bytes = source.as_bytes();
    let mut i = slice_start;
    let mut open_span: Option<(usize, usize, String, ClaimRef, Option<String>)> = None;
    let mut span_value_ranges_on_line: Vec<(usize, usize)> = Vec::new();
    let mut temp_spans: Vec<(usize, usize, String, ClaimRef, Option<String>, usize)> = Vec::new();

    while i < slice_end {
        // Skip inline code spans `` `...` `` when not inside an open cw span (or only when outside)
        if open_span.is_none() && bytes[i] == b'`' {
            let mut tick_len = 1;
            while i + tick_len < slice_end && bytes[i + tick_len] == b'`' {
                tick_len += 1;
            }
            i += tick_len;
            // Find matching closing backtick run of exact length `tick_len`
            while i < slice_end {
                if bytes[i] == b'`' {
                    let mut close_len = 1;
                    while i + close_len < slice_end && bytes[i + close_len] == b'`' {
                        close_len += 1;
                    }
                    i += close_len;
                    if close_len == tick_len {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }

        if i + 4 <= slice_end && &bytes[i..i + 4] == b"<!--" {
            let comment_start = i;
            let Some(rel_end) = source[i + 4..slice_end].find("-->") else {
                // Unclosed HTML comment
                if source[i + 4..slice_end].trim_start().starts_with("cw:")
                    || source[i + 4..slice_end].trim_start().starts_with("/cw")
                {
                    let col = (comment_start - line_start) + 1;
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::CW1001,
                            "Unclosed `<!--` in Claim Wire marker",
                        )
                        .with_location(file, line_no, col)
                        .with_snippet(
                            full_line_str,
                            slice_end - comment_start,
                            "unclosed comment",
                        ),
                    );
                }
                break;
            };
            let comment_end = i + 4 + rel_end + 3;
            let inner = source[i + 4..i + 4 + rel_end].trim();

            if inner == "/cw" || inner.starts_with("/cw ") {
                let col = (comment_start - line_start) + 1;
                if inner != "/cw" {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!(
                                "Malformed closing tag `<!-- {inner} -->`: expected `<!-- /cw -->`"
                            ),
                        )
                        .with_location(file, line_no, col)
                        .with_snippet(
                            full_line_str,
                            comment_end - comment_start,
                            "malformed closing tag",
                        ),
                    );
                }
                match open_span.take() {
                    Some((_open_marker_start, val_start, raw_dir, claim_ref, fmt_override)) => {
                        let val_end = comment_start;
                        let canonical_key = claim_ref.canonical_string();
                        let occ = *span_counts.get(&canonical_key).unwrap_or(&0);
                        span_counts.insert(canonical_key, occ + 1);
                        span_value_ranges_on_line.push((_open_marker_start, comment_end));
                        temp_spans.push((
                            val_start,
                            val_end,
                            raw_dir,
                            claim_ref,
                            fmt_override,
                            occ,
                        ));
                    }
                    None => {
                        diagnostics.push(
                            Diagnostic::error(
                                DiagnosticCode::CW1003,
                                "Mismatched closing tag `<!-- /cw -->` with no open Claim Wire span",
                            )
                            .with_location(file, line_no, col)
                            .with_snippet(
                                full_line_str,
                                comment_end - comment_start,
                                "unexpected closing tag",
                            ),
                        );
                    }
                }
            } else if let Some(after_cw) = inner.strip_prefix("cw:") {
                let col = (comment_start - line_start) + 1;
                if open_span.is_some() {
                    diagnostics.push(
                        Diagnostic::error(
                            DiagnosticCode::CW1002,
                            "Nested `<!-- cw:... -->` spans are not allowed",
                        )
                        .with_location(file, line_no, col)
                        .with_snippet(
                            full_line_str,
                            comment_end - comment_start,
                            "nested span opened before closing previous span",
                        ),
                    );
                } else {
                    let raw_dir = after_cw.trim().to_string();
                    // Check for optional format specification separated by whitespace + `format=`
                    let (ref_str, fmt_override) =
                        if let Some((r, f)) = raw_dir.split_once(" format=") {
                            let f_unquoted = crate::yaml::unquote_str(f.trim())
                                .unwrap_or_else(|_| f.trim().to_string());
                            (r.trim(), Some(f_unquoted))
                        } else {
                            (raw_dir.as_str(), None)
                        };
                    match ClaimRef::parse(ref_str) {
                        Ok(claim_ref) => {
                            open_span = Some((
                                comment_start,
                                comment_end,
                                raw_dir,
                                claim_ref,
                                fmt_override,
                            ));
                        }
                        Err(mut diag) => {
                            diag.file = Some(file.to_string());
                            diag.line = Some(line_no);
                            diag.col = Some(col);
                            diag.snippet_line = Some(full_line_str.to_string());
                            diag.span_len = Some(comment_end - comment_start);
                            diagnostics.push(diag);
                        }
                    }
                }
            }
            i = comment_end;
            continue;
        }
        i += 1;
    }

    if let Some((open_marker_start, val_start, raw_dir, _, _)) = open_span {
        let col = (open_marker_start - line_start) + 1;
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::CW1004,
                format!("Unclosed Claim Wire span `<!-- cw:{raw_dir} -->`"),
            )
            .with_location(file, line_no, col)
            .with_snippet(
                full_line_str,
                val_start - open_marker_start,
                "span opened here and never closed with `<!-- /cw -->`",
            ),
        );
    }

    if !temp_spans.is_empty() {
        let ctx_digest =
            compute_line_context_digest(source, slice_start, slice_end, &span_value_ranges_on_line);
        for (val_start, val_end, raw_dir, claim_ref, fmt_override, occ) in temp_spans {
            let value_col = (val_start - line_start) + 1;
            spans_out.push(InlineSpanNode {
                file: file.to_string(),
                line: line_no,
                value_col,
                raw_directive: raw_dir,
                claim_ref,
                format_override: fmt_override,
                occurrence: occ,
                value_range: ByteRange {
                    start: val_start,
                    end: val_end,
                },
                current_text: source[val_start..val_end].to_string(),
                line_text: full_line_str.to_string(),
                context_digest: ctx_digest.clone(),
            });
        }
    }
}

/// Compute normalized `context_digest` for an inline span's enclosing slice,
/// replacing all owned `<!-- cw:... -->...<!-- /cw -->` ranges with `{{CW}}` and normalizing whitespace.
pub fn compute_line_context_digest(
    source: &str,
    slice_start: usize,
    slice_end: usize,
    owned_ranges: &[(usize, usize)],
) -> String {
    let mut normalized = String::new();
    let mut cursor = slice_start;
    for &(r_start, r_end) in owned_ranges {
        if r_start > cursor {
            normalized.push_str(&source[cursor..r_start]);
        }
        normalized.push_str("{{CW}}");
        cursor = r_end;
    }
    if cursor < slice_end {
        normalized.push_str(&source[cursor..slice_end]);
    }
    let collapsed: Vec<&str> = normalized.split_whitespace().collect();
    blake3_prefixed(collapsed.join(" ").as_bytes())
}

pub fn compute_text_context_digest(text: &str) -> String {
    let collapsed: Vec<&str> = text.split_whitespace().collect();
    blake3_prefixed(collapsed.join(" ").as_bytes())
}

pub fn split_lines_lossless(source: &str) -> Vec<LineSlice> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_no = 1usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let content_end = if i > start && bytes[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            lines.push(LineSlice {
                line_no,
                start,
                content_end,
                full_end: i + 1,
            });
            start = i + 1;
            line_no += 1;
        }
        i += 1;
    }
    if start < bytes.len() {
        lines.push(LineSlice {
            line_no,
            start,
            content_end: bytes.len(),
            full_end: bytes.len(),
        });
    }
    lines
}

/// Apply non-overlapping byte patches to `source`, preserving all untouched bytes byte-for-byte
/// (Invariant 2: Non-Interference).
pub fn apply_byte_patches(source: &str, mut patches: Vec<(ByteRange, String)>) -> String {
    if patches.is_empty() {
        return source.to_string();
    }
    patches.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for (range, replacement) in patches {
        if range.start < cursor {
            continue;
        }
        out.push_str(&source[cursor..range.start]);
        out.push_str(&replacement);
        cursor = range.end;
    }
    out.push_str(&source[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gfm_table_cw1008_lint() {
        let md = r#"# PCS

| term | bound | source |
|---|---|---|
| sum-check, `k` rounds, degree 2 | `2k/|K|` (reported as `127 − log2 k` bits) | 03, Schwartz–Zippel |
"#;
        let doc = MarkdownDocument::parse("04-pcs.md", md);
        assert_eq!(doc.diagnostics.len(), 1);
        assert_eq!(doc.diagnostics[0].code, DiagnosticCode::CW1008);
        assert_eq!(doc.diagnostics[0].line, Some(5));
    }

    #[test]
    fn test_lossless_inline_span_and_fenced_code_ignoring() {
        let md = "Prose <!-- cw:inventory.python_tests -->192<!-- /cw --> tests.\n```sh\n<!-- cw:ignored.in_code -->999<!-- /cw -->\n```\n";
        let doc = MarkdownDocument::parse("test.md", md);
        assert!(doc.diagnostics.is_empty());
        assert_eq!(doc.inline_spans.len(), 1);
        assert_eq!(doc.inline_spans[0].current_text, "192");

        let patched = apply_byte_patches(
            &doc.source,
            vec![(doc.inline_spans[0].value_range.clone(), "227".to_string())],
        );
        assert_eq!(
            patched,
            "Prose <!-- cw:inventory.python_tests -->227<!-- /cw --> tests.\n```sh\n<!-- cw:ignored.in_code -->999<!-- /cw -->\n```\n"
        );
    }
}
