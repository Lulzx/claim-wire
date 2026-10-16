//! Bidirectional Format Patterns (`spec.md` §4.5).

use crate::diagnostics::{Diagnostic, DiagnosticCode};
use crate::types::{CanonicalValue, ValueType};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatPattern {
    pub raw: String,
    pub segments: Vec<PatternSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternSegment {
    Literal(String),
    Placeholder(PlaceholderSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceholderSpec {
    /// Optional claim reference or field name inside `{...}` before `:`.
    pub target: Option<String>,
    pub spec: FormatSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatSpec {
    Numeric {
        grouping: bool,
        precision: Option<usize>,
        kind: Option<NumericKind>,
        /// Verbatim unit text up to `}` (whitespace significant, §4.5 rule 2). A unit in square
        /// brackets (`{:.3f [ms]}`) scales without being rendered, for tables whose header
        /// carries the unit (`Time (ms)`); see [`split_hidden_unit`].
        unit: String,
    },
    Named(NamedFormat),
    BoolMapped {
        true_str: String,
        false_str: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumericKind {
    Fixed,      // 'f'
    Scientific, // 'e'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamedFormat {
    AutoIec,
    AutoTime,
    Pow2,
    NegPow2,
    Hex8,
}

impl Default for FormatSpec {
    fn default() -> Self {
        Self::Numeric {
            grouping: false,
            precision: None,
            kind: None,
            unit: String::new(),
        }
    }
}

impl FormatPattern {
    pub fn parse(raw: &str) -> Result<Self, Diagnostic> {
        let mut segments = Vec::new();
        let mut cur_lit = String::new();
        let bytes = raw.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    cur_lit.push('{');
                    i += 2;
                    continue;
                }
                if !cur_lit.is_empty() {
                    segments.push(PatternSegment::Literal(std::mem::take(&mut cur_lit)));
                }
                // Find matching `}` (keeping track of `{a, b}` subset braces inside target selector!)
                let placeholder_start = i + 1;
                let Some(close_idx) = find_placeholder_close(raw, placeholder_start) else {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW1007,
                        format!("Unclosed `{{` in format pattern `{raw}`"),
                    ));
                };
                let inside = &raw[placeholder_start..close_idx];
                let placeholder = parse_placeholder(inside, raw)?;
                segments.push(PatternSegment::Placeholder(placeholder));
                i = close_idx + 1;
            } else if bytes[i] == b'}' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    cur_lit.push('}');
                    i += 2;
                    continue;
                }
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1007,
                    format!("Unescaped `}}` in format pattern `{raw}`"),
                ));
            } else {
                let ch = raw[i..].chars().next().unwrap();
                cur_lit.push(ch);
                i += ch.len_utf8();
            }
        }

        if !cur_lit.is_empty() {
            segments.push(PatternSegment::Literal(cur_lit));
        }

        let pat = Self {
            raw: raw.to_string(),
            segments,
        };
        pat.validate_invertibility()?;
        Ok(pat)
    }

    /// Check §4.5 rule 6 (`CW1007`): Every numeric portion must be followed (by its own unit or by literal text)
    /// by text that cannot continue the number. The reverse parser only extends a number over a digit,
    /// a `,`/`.` immediately followed by a digit, or an exponent `e`/`E` followed by an optionally signed
    /// digit, so `{n}-file` and `{passed}/{total}.` are invertible while `{a}.{b}` and `{n}5` are not.
    pub fn validate_invertibility(&self) -> Result<(), Diagnostic> {
        for idx in 0..self.segments.len() {
            let PatternSegment::Placeholder(ph) = &self.segments[idx] else {
                continue;
            };
            // Does this placeholder's own unit terminate its numeric portion?
            let unit_terminates_number = match &ph.spec {
                FormatSpec::Numeric { unit, .. } => {
                    let (_, shown) = split_hidden_unit(unit);
                    !shown.is_empty() && !literal_continues_number(shown, false)
                }
                FormatSpec::Named(NamedFormat::AutoIec | NamedFormat::AutoTime) => true,
                FormatSpec::Named(NamedFormat::Pow2 | NamedFormat::NegPow2 | NamedFormat::Hex8) => {
                    false
                }
                FormatSpec::BoolMapped { .. } => true,
            };

            if idx + 1 < self.segments.len() {
                match &self.segments[idx + 1] {
                    PatternSegment::Placeholder(_) => {
                        if !unit_terminates_number {
                            return Err(Diagnostic::error(
                                DiagnosticCode::CW1007,
                                format!(
                                    "Ambiguous (non-invertible) format pattern `{}`: adjacent placeholders without a non-numeric separator",
                                    self.raw
                                ),
                            ));
                        }
                    }
                    PatternSegment::Literal(lit) => {
                        if lit.is_empty() {
                            return Err(Diagnostic::error(
                                DiagnosticCode::CW1007,
                                format!("Ambiguous format pattern `{}`", self.raw),
                            ));
                        }
                        if !unit_terminates_number {
                            let first_ch = lit.chars().next().unwrap();
                            let followed_by_placeholder = matches!(
                                self.segments.get(idx + 2),
                                Some(PatternSegment::Placeholder(_))
                            );
                            if literal_continues_number(lit, followed_by_placeholder) {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1007,
                                    format!(
                                        "Ambiguous (non-invertible) format pattern `{}`: placeholder is followed by numeric character `{first_ch}`",
                                        self.raw
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Check §4.5 rule 1 (`CW2005`): validate that the format specification's unit is valid for `ty`.
    pub fn validate_types<F>(&self, mut resolve_type: F) -> Result<(), Diagnostic>
    where
        F: FnMut(Option<&str>) -> Result<ValueType, Diagnostic>,
    {
        for seg in &self.segments {
            if let PatternSegment::Placeholder(ph) = seg {
                let ty = resolve_type(ph.target.as_deref())?;
                validate_spec_for_type(&ph.spec, &ty, &self.raw)?;
            }
        }
        Ok(())
    }

    pub fn has_outer_decorator_chars(&self) -> bool {
        let t = self.raw.trim();
        t.starts_with('*') || t.starts_with('`') || t.ends_with('*') || t.ends_with('`')
    }

    pub fn placeholders(&self) -> impl Iterator<Item = &PlaceholderSpec> {
        self.segments.iter().filter_map(|s| match s {
            PatternSegment::Placeholder(p) => Some(p),
            PatternSegment::Literal(_) => None,
        })
    }

    /// Render the pattern using `resolve_val(target)`.
    pub fn render<F>(&self, mut resolve_val: F) -> Result<String, Diagnostic>
    where
        F: FnMut(Option<&str>) -> Result<(CanonicalValue, ValueType), Diagnostic>,
    {
        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                PatternSegment::Literal(lit) => out.push_str(lit),
                PatternSegment::Placeholder(ph) => {
                    let (val, ty) = resolve_val(ph.target.as_deref())?;
                    validate_spec_for_type(&ph.spec, &ty, &self.raw)?;
                    let rendered = render_single(&val, &ty, &ph.spec, ph.target.as_deref())?;
                    out.push_str(&rendered);
                }
            }
        }
        Ok(out)
    }

    /// Reverse-parse a rendered string back into canonical values (`cw adopt`, §4.5).
    pub fn reverse_parse<F>(
        &self,
        rendered: &str,
        resolve_type: F,
    ) -> Result<IndexMap<String, CanonicalValue>, Diagnostic>
    where
        F: FnMut(Option<&str>) -> Result<ValueType, Diagnostic>,
    {
        let (results, rest) = self.parse_prefix(rendered, resolve_type)?;
        if !rest.is_empty() {
            return Err(Diagnostic::error(
                DiagnosticCode::CW3003,
                format!(
                    "Trailing unparsed text `{rest}` when matching `{rendered}` against `{}`",
                    self.raw
                ),
            ));
        }
        Ok(results)
    }

    /// Every non-overlapping occurrence of this pattern inside `text`, as `(byte range, values)`.
    /// Used by block lenses so a pattern may name just the fragment of a line that carries the
    /// numbers; leading/trailing prose can then be reworded or rewrapped freely. A match never
    /// starts or ends inside a number (`{n} tests` does not match the `62 tests` of `362 tests`).
    pub fn find_fragments<F>(
        &self,
        text: &str,
        mut resolve_type: F,
    ) -> Vec<(std::ops::Range<usize>, IndexMap<String, CanonicalValue>)>
    where
        F: FnMut(Option<&str>) -> Result<ValueType, Diagnostic>,
    {
        let continues_number =
            |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',');
        let mut found = Vec::new();
        let mut start = 0;
        while start <= text.len() {
            let candidate = match self.segments.first() {
                Some(PatternSegment::Literal(lit)) => {
                    text[start..].find(lit.as_str()).map(|o| start + o)
                }
                _ => text[start..]
                    .char_indices()
                    .map(|(o, _)| start + o)
                    .find(|&i| !continues_number(text[..i].chars().next_back())),
            };
            let Some(at) = candidate else { break };
            match self.parse_prefix(&text[at..], &mut resolve_type) {
                Ok((values, rest)) => {
                    let end = text.len() - rest.len();
                    let ends_mid_number =
                        matches!(self.segments.last(), Some(PatternSegment::Placeholder(_)))
                            && rest.chars().next().is_some_and(|c| c.is_ascii_digit());
                    if end > at && !ends_mid_number {
                        found.push((at..end, values));
                        start = end;
                        continue;
                    }
                    start = at + text[at..].chars().next().map_or(1, char::len_utf8);
                }
                _ => start = at + text[at..].chars().next().map_or(1, char::len_utf8),
            }
        }
        found
    }

    /// Parse the pattern from the start of `rendered`, returning the unconsumed remainder.
    fn parse_prefix<'a, F>(
        &self,
        rendered: &'a str,
        mut resolve_type: F,
    ) -> Result<(IndexMap<String, CanonicalValue>, &'a str), Diagnostic>
    where
        F: FnMut(Option<&str>) -> Result<ValueType, Diagnostic>,
    {
        let mut results = IndexMap::new();
        let mut cursor = rendered;

        for (idx, seg) in self.segments.iter().enumerate() {
            match seg {
                PatternSegment::Literal(lit) => {
                    let Some(rest) = cursor.strip_prefix(lit.as_str()) else {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW3003,
                            format!(
                                "Rendered text `{rendered}` does not match literal `{lit}` in pattern `{}`",
                                self.raw
                            ),
                        ));
                    };
                    cursor = rest;
                }
                PatternSegment::Placeholder(ph) => {
                    let ty = resolve_type(ph.target.as_deref())?;
                    // Determine the literal suffix that immediately follows this placeholder, if any
                    let next_literal = if idx + 1 < self.segments.len() {
                        match &self.segments[idx + 1] {
                            PatternSegment::Literal(l) => Some(l.as_str()),
                            PatternSegment::Placeholder(_) => None,
                        }
                    } else {
                        None
                    };

                    let (val, rest) =
                        parse_single_placeholder(cursor, &ph.spec, &ty, next_literal, &self.raw)?;
                    let key = ph.target.clone().unwrap_or_else(|| format!("${idx}"));
                    results.insert(key, val);
                    cursor = rest;
                }
            }
        }

        Ok((results, cursor))
    }
}

/// Split a numeric spec's unit into `(unit used for scaling, text rendered after the number)`.
/// `[ms]` (optionally space-padded) scales by milliseconds and renders nothing.
pub fn split_hidden_unit(unit: &str) -> (&str, &str) {
    let t = unit.trim();
    match t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        Some(inner) => (inner.trim(), ""),
        None => (unit, unit),
    }
}

/// Whether `take_numeric_prefix` could extend a number into literal text `lit`.
/// `followed_by_placeholder` means a placeholder (which renders a digit) comes right after `lit`.
fn literal_continues_number(lit: &str, followed_by_placeholder: bool) -> bool {
    let b = lit.as_bytes();
    // The byte after position `i`: from the literal, or a digit from the next placeholder.
    let digit_at = |i: usize| match b.get(i) {
        Some(c) => c.is_ascii_digit(),
        None => followed_by_placeholder,
    };
    match b.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b',' | b'.') => digit_at(1),
        Some(b'e' | b'E') => match b.get(1) {
            Some(b'+' | b'-') => digit_at(2),
            _ => digit_at(1),
        },
        _ => false,
    }
}

fn find_placeholder_close(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_quotes = false;
    let mut i = start;

    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_quotes = false;
            }
        } else {
            match b {
                b'"' => in_quotes = true,
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'{' => brace_depth += 1,
                b'}' => {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    } else if bracket_depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn parse_placeholder(inside: &str, full_pattern: &str) -> Result<PlaceholderSpec, Diagnostic> {
    // Split on the first `:` that is outside `[...]` and `"..."`
    let colon_idx = find_top_level_colon(inside);
    let (target_part, spec_part) = match colon_idx {
        Some(idx) => (&inside[..idx], Some(&inside[idx + 1..])),
        None => (inside, None),
    };

    let target = {
        let t = target_part.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };

    let spec = match spec_part {
        None => FormatSpec::default(),
        Some(sp) => parse_format_spec(sp, full_pattern)?,
    };

    Ok(PlaceholderSpec { target, spec })
}

fn find_top_level_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut bracket_depth = 0usize;
    let mut in_quotes = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_quotes = false;
            }
        } else {
            match b {
                b'"' => in_quotes = true,
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b':' if bracket_depth == 0 => return Some(i),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn parse_format_spec(sp: &str, full_pattern: &str) -> Result<FormatSpec, Diagnostic> {
    match sp {
        "auto_iec" => return Ok(FormatSpec::Named(NamedFormat::AutoIec)),
        "auto_time" => return Ok(FormatSpec::Named(NamedFormat::AutoTime)),
        "pow2" => return Ok(FormatSpec::Named(NamedFormat::Pow2)),
        "neg_pow2" => return Ok(FormatSpec::Named(NamedFormat::NegPow2)),
        "hex8" => return Ok(FormatSpec::Named(NamedFormat::Hex8)),
        _ => {}
    }

    if let Some((l, r)) = sp.split_once('|') {
        return Ok(FormatSpec::BoolMapped {
            true_str: l.to_string(),
            false_str: r.to_string(),
        });
    }

    // Spec ::= Grouping? Precision? Kind? Unit?
    let mut rest = sp;
    let grouping = if let Some(r) = rest.strip_prefix(',') {
        rest = r;
        true
    } else {
        false
    };

    let precision = if let Some(r) = rest.strip_prefix('.') {
        let digit_len = r.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_len == 0 {
            return Err(Diagnostic::error(
                DiagnosticCode::CW1007,
                format!("Expected digits after `.` in format pattern `{full_pattern}`"),
            ));
        }
        let prec = r[..digit_len].parse::<usize>().map_err(|_| {
            Diagnostic::error(
                DiagnosticCode::CW1007,
                format!("Invalid precision in format pattern `{full_pattern}`"),
            )
        })?;
        rest = &r[digit_len..];
        Some(prec)
    } else {
        None
    };

    let kind = if precision.is_some() {
        if let Some(r) = rest.strip_prefix('f') {
            rest = r;
            Some(NumericKind::Fixed)
        } else if let Some(r) = rest.strip_prefix('e') {
            rest = r;
            Some(NumericKind::Scientific)
        } else {
            None
        }
    } else if rest.starts_with("e ") || rest == "e" {
        rest = &rest[1..];
        Some(NumericKind::Scientific)
    } else {
        None
    };

    let unit = rest.to_string();

    Ok(FormatSpec::Numeric {
        grouping,
        precision,
        kind,
        unit,
    })
}

pub fn validate_spec_for_type(
    spec: &FormatSpec,
    ty: &ValueType,
    full_pattern: &str,
) -> Result<(), Diagnostic> {
    match spec {
        FormatSpec::Named(named) => match (named, ty) {
            (NamedFormat::AutoIec, ValueType::Bytes) => Ok(()),
            (NamedFormat::AutoTime, ValueType::Duration) => Ok(()),
            (NamedFormat::Pow2, ValueType::Cycles | ValueType::Count) => Ok(()),
            (NamedFormat::NegPow2, ValueType::Ratio | ValueType::Float) => Ok(()),
            (NamedFormat::Hex8, ValueType::Digest | ValueType::String) => Ok(()),
            _ => Err(Diagnostic::error(
                DiagnosticCode::CW2005,
                format!(
                    "Named format `{named:?}` is not valid for type `{ty}` in `{full_pattern}`"
                ),
            )),
        },
        FormatSpec::BoolMapped { .. } => {
            if *ty == ValueType::Bool {
                Ok(())
            } else {
                Err(Diagnostic::error(
                    DiagnosticCode::CW2005,
                    format!(
                        "Boolean mapped format is not valid for type `{ty}` in `{full_pattern}`"
                    ),
                ))
            }
        }
        FormatSpec::Numeric { unit, .. } => {
            let (unit, _) = split_hidden_unit(unit);
            if ty.requires_explicit_unit() && unit.trim().is_empty() {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2005,
                    format!(
                        "Missing unit in format specification for type `{ty}` in `{full_pattern}` (units must live inside braces)"
                    ),
                ));
            }
            if ty.unit_scale_to_canonical(unit).is_none() {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW2005,
                    format!(
                        "Unit `{}` is not valid for type `{ty}` in `{full_pattern}`",
                        unit.trim()
                    ),
                ));
            }
            Ok(())
        }
    }
}

fn render_single(
    val: &CanonicalValue,
    ty: &ValueType,
    spec: &FormatSpec,
    target: Option<&str>,
) -> Result<String, Diagnostic> {
    match (val, spec) {
        (
            CanonicalValue::Bool(b),
            FormatSpec::BoolMapped {
                true_str,
                false_str,
            },
        ) => Ok(if *b {
            true_str.clone()
        } else {
            false_str.clone()
        }),
        (CanonicalValue::Bool(b), FormatSpec::Numeric { unit, .. }) if unit.is_empty() => {
            Ok(b.to_string())
        }
        (CanonicalValue::Text(s), FormatSpec::Named(NamedFormat::Hex8)) => {
            let prefix: String = s.chars().take(8).collect();
            Ok(format!("{prefix}…"))
        }
        (CanonicalValue::Text(s), FormatSpec::Numeric { unit, .. }) if unit.is_empty() => {
            Ok(s.clone())
        }
        (
            CanonicalValue::PassRatio { passed, total },
            FormatSpec::Numeric {
                precision, unit, ..
            },
        ) => {
            if target == Some("passed") {
                return Ok(passed.to_string());
            }
            if target == Some("total") {
                return Ok(total.to_string());
            }
            if unit.trim() == "%" || target == Some("rate") {
                let rate = if *total == 0 {
                    0.0
                } else {
                    (*passed as f64 / *total as f64) * 100.0
                };
                let prec = precision.unwrap_or(1);
                let num_str = format_number_half_to_even(rate, Some(prec), false, None);
                return Ok(format!("{num_str}{unit}"));
            }
            Ok(format!("{passed}/{total}"))
        }
        (CanonicalValue::Number(n), FormatSpec::Named(named)) => match named {
            NamedFormat::AutoIec => Ok(format_auto_iec(*n)),
            NamedFormat::AutoTime => Ok(format_auto_time(*n)),
            NamedFormat::Pow2 => format_pow2(*n),
            NamedFormat::NegPow2 => format_neg_pow2(*n),
            NamedFormat::Hex8 => Err(Diagnostic::error(
                DiagnosticCode::CW2005,
                "hex8 cannot format a numeric value",
            )),
        },
        (
            CanonicalValue::Number(n),
            FormatSpec::Numeric {
                grouping,
                precision,
                kind,
                unit,
            },
        ) => {
            let (scale_unit, shown) = split_hidden_unit(unit);
            let scale = ty.unit_scale_to_canonical(scale_unit).ok_or_else(|| {
                Diagnostic::error(
                    DiagnosticCode::CW2005,
                    format!("Invalid unit `{}` for type `{ty}`", scale_unit.trim()),
                )
            })?;
            let scaled = *n / scale;
            let num_str = format_number_half_to_even(scaled, *precision, *grouping, *kind);
            Ok(format!("{num_str}{shown}"))
        }
        _ => Err(Diagnostic::error(
            DiagnosticCode::CW2001,
            format!("Cannot format value `{val:?}` of type `{ty}` with spec `{spec:?}`"),
        )),
    }
}

/// Banker's rounding (`half_to_even`, Invariant 4: Single-Rounding Determinism).
pub fn round_half_to_even_f64(val: f64) -> f64 {
    if !val.is_finite() {
        return val;
    }
    let sign = if val < 0.0 { -1.0 } else { 1.0 };
    let abs = val.abs();
    let fl = abs.floor();
    let diff = abs - fl;
    // Use a small epsilon around 0.5 to handle decimal representation in binary float (e.g. 0.125 * 100)
    let rounded = if (diff - 0.5).abs() < 1e-9 {
        let fl_int = fl as u64;
        if fl_int % 2 == 0 { fl } else { fl + 1.0 }
    } else if diff > 0.5 {
        fl + 1.0
    } else {
        fl
    };
    sign * rounded
}

pub fn round_to_decimals(val: f64, decimals: usize) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    round_half_to_even_f64(val * factor) / factor
}

pub fn format_number_half_to_even(
    val: f64,
    precision: Option<usize>,
    grouping: bool,
    kind: Option<NumericKind>,
) -> String {
    if matches!(kind, Some(NumericKind::Scientific)) {
        let prec = precision.unwrap_or(3);
        return format!("{val:.prec$e}");
    }

    let prec = precision.unwrap_or(0);
    let factor = 10f64.powi(prec as i32);
    let scaled_rounded = round_half_to_even_f64(val * factor);
    let is_neg = scaled_rounded < 0.0;
    let abs_scaled = scaled_rounded.abs().round() as u128;
    let divisor = 10u128.pow(prec as u32);
    let int_part = abs_scaled / divisor;
    let frac_part = abs_scaled % divisor;

    let int_str = if grouping {
        insert_commas(int_part)
    } else {
        int_part.to_string()
    };

    let sign_str = if is_neg && (int_part > 0 || frac_part > 0) {
        "-"
    } else {
        ""
    };

    if prec == 0 {
        format!("{sign_str}{int_str}")
    } else {
        format!("{sign_str}{int_str}.{frac_part:0>width$}", width = prec)
    }
}

fn insert_commas(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut groups = Vec::new();
    while n > 0 {
        groups.push((n % 1000) as u16);
        n /= 1000;
    }
    let mut out = String::new();
    for (idx, g) in groups.iter().rev().enumerate() {
        if idx == 0 {
            out.push_str(&g.to_string());
        } else {
            out.push_str(&format!(",{g:03}"));
        }
    }
    out
}

/// §4.5 rule 4: `auto_iec` picks the largest IEC unit with magnitude >= 1 and renders
/// two significant digits without ever truncating the integer part (`16 MiB`, `8.3 GiB`, `112 MiB`).
pub fn format_auto_iec(bytes: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let abs = bytes.abs();
    let (scaled, unit) = if abs >= GIB {
        (bytes / GIB, " GiB")
    } else if abs >= MIB {
        (bytes / MIB, " MiB")
    } else if abs >= KIB {
        (bytes / KIB, " KiB")
    } else {
        return format!("{} B", round_half_to_even_f64(bytes) as i64);
    };

    let prec = if scaled.abs() < 10.0 { 1 } else { 0 };
    let num = format_number_half_to_even(scaled, Some(prec), false, Some(NumericKind::Fixed));
    format!("{num}{unit}")
}

pub fn format_auto_time(ns: f64) -> String {
    let abs = ns.abs();
    let (scaled, unit) = if abs >= 60.0 * 1e9 {
        (ns / (60.0 * 1e9), " min")
    } else if abs >= 1e9 {
        (ns / 1e9, " s")
    } else if abs >= 1e6 {
        (ns / 1e6, " ms")
    } else if abs >= 1e3 {
        (ns / 1e3, " µs")
    } else {
        return format!("{} ns", round_half_to_even_f64(ns) as i64);
    };

    let prec = if scaled.abs() < 10.0 { 2 } else { 0 };
    let num = format_number_half_to_even(scaled, Some(prec), false, Some(NumericKind::Fixed));
    format!("{num}{unit}")
}

/// §4.5 rule 4: `pow2` renders `2^k` and raises `CW3004` if the value is not an exact power of two.
pub fn format_pow2(val: f64) -> Result<String, Diagnostic> {
    if val <= 0.0 || val.fract() != 0.0 {
        return Err(Diagnostic::error(
            DiagnosticCode::CW3004,
            format!("Value `{val}` is not an exact power of two for `:pow2` format"),
        ));
    }
    let u = val as u64;
    if !u.is_power_of_two() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW3004,
            format!("Value `{u}` is not an exact power of two for `:pow2` format"),
        ));
    }
    Ok(format!("2^{}", u.trailing_zeros()))
}

pub fn format_neg_pow2(val: f64) -> Result<String, Diagnostic> {
    if val <= 0.0 {
        return Err(Diagnostic::error(
            DiagnosticCode::CW3004,
            format!("Value `{val}` is not representable as `:neg_pow2`"),
        ));
    }
    let log2 = (-val.log2()).round();
    let reconstructed = 2f64.powf(-log2);
    if ((val - reconstructed) / reconstructed).abs() > 1e-9 {
        return Err(Diagnostic::error(
            DiagnosticCode::CW3004,
            format!("Value `{val}` is not an exact negative power of two for `:neg_pow2`"),
        ));
    }
    Ok(format!("2^-{}", log2 as i64))
}

fn parse_single_placeholder<'a>(
    cursor: &'a str,
    spec: &FormatSpec,
    ty: &ValueType,
    next_literal: Option<&str>,
    full_pattern: &str,
) -> Result<(CanonicalValue, &'a str), Diagnostic> {
    match spec {
        FormatSpec::BoolMapped {
            true_str,
            false_str,
        } => {
            if let Some(rest) = cursor.strip_prefix(true_str.as_str()) {
                return Ok((CanonicalValue::Bool(true), rest));
            }
            if let Some(rest) = cursor.strip_prefix(false_str.as_str()) {
                return Ok((CanonicalValue::Bool(false), rest));
            }
            Err(Diagnostic::error(
                DiagnosticCode::CW3003,
                format!("Expected `{true_str}` or `{false_str}` in `{cursor}`"),
            ))
        }
        FormatSpec::Named(named) => match named {
            NamedFormat::Pow2 => {
                let Some(after_prefix) = cursor.strip_prefix("2^") else {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW3003,
                        format!("Expected `2^k` in `{cursor}`"),
                    ));
                };
                let digits_len = after_prefix
                    .bytes()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                if digits_len == 0 {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW3003,
                        format!("Expected exponent digits after `2^` in `{cursor}`"),
                    ));
                }
                let exp: u32 = after_prefix[..digits_len].parse().unwrap_or(0);
                Ok((
                    CanonicalValue::Number(2f64.powi(exp as i32)),
                    &after_prefix[digits_len..],
                ))
            }
            NamedFormat::NegPow2 => {
                let Some(after_prefix) = cursor.strip_prefix("2^-") else {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW3003,
                        format!("Expected `2^-k` in `{cursor}`"),
                    ));
                };
                let digits_len = after_prefix
                    .bytes()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                if digits_len == 0 {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW3003,
                        format!("Expected exponent digits after `2^-` in `{cursor}`"),
                    ));
                }
                let exp: i32 = after_prefix[..digits_len].parse().unwrap_or(0);
                Ok((
                    CanonicalValue::Number(2f64.powi(-exp)),
                    &after_prefix[digits_len..],
                ))
            }
            NamedFormat::Hex8 => {
                if let Some(idx) = cursor.find('…') {
                    let hex_part = &cursor[..idx];
                    let rest = &cursor[idx + '…'.len_utf8()..];
                    Ok((CanonicalValue::Text(hex_part.to_string()), rest))
                } else {
                    Err(Diagnostic::error(
                        DiagnosticCode::CW3003,
                        format!("Expected `…` suffix for `:hex8` in `{cursor}`"),
                    ))
                }
            }
            NamedFormat::AutoIec => {
                let (num_str, after_num) = take_numeric_prefix(cursor)?;
                let units = [" GiB", " MiB", " KiB", " B", "GiB", "MiB", "KiB", "B"];
                for u in units {
                    if let Some(rest) = after_num.strip_prefix(u) {
                        let scale = ValueType::Bytes.unit_scale_to_canonical(u).unwrap();
                        let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                            Diagnostic::error(
                                DiagnosticCode::CW3003,
                                format!("Invalid number `{num_str}` in `{cursor}`"),
                            )
                        })?;
                        return Ok((CanonicalValue::Number(num * scale), rest));
                    }
                }
                Err(Diagnostic::error(
                    DiagnosticCode::CW3003,
                    format!("Expected IEC byte unit after `{num_str}` in `{cursor}`"),
                ))
            }
            NamedFormat::AutoTime => {
                let (num_str, after_num) = take_numeric_prefix(cursor)?;
                let units = [" min", " ms", " µs", " us", " ns", " s"];
                for u in units {
                    if let Some(rest) = after_num.strip_prefix(u) {
                        let scale = ValueType::Duration.unit_scale_to_canonical(u).unwrap();
                        let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                            Diagnostic::error(
                                DiagnosticCode::CW3003,
                                format!("Invalid number `{num_str}` in `{cursor}`"),
                            )
                        })?;
                        return Ok((CanonicalValue::Number(num * scale), rest));
                    }
                }
                Err(Diagnostic::error(
                    DiagnosticCode::CW3003,
                    format!("Expected time unit after `{num_str}` in `{cursor}`"),
                ))
            }
        },
        FormatSpec::Numeric { unit, .. } => {
            if matches!(ty, ValueType::String | ValueType::Digest) {
                if let Some(nl) = next_literal {
                    if let Some(idx) = cursor.find(nl) {
                        return Ok((
                            CanonicalValue::Text(cursor[..idx].to_string()),
                            &cursor[idx..],
                        ));
                    }
                }
                return Ok((CanonicalValue::Text(cursor.to_string()), ""));
            }
            if *ty == ValueType::Bool {
                if let Some(rest) = cursor.strip_prefix("true") {
                    return Ok((CanonicalValue::Bool(true), rest));
                }
                if let Some(rest) = cursor.strip_prefix("false") {
                    return Ok((CanonicalValue::Bool(false), rest));
                }
            }
            let (scale_unit, shown) = split_hidden_unit(unit);
            let (num_str, after_num) = take_numeric_prefix(cursor)?;
            let Some(rest) = after_num.strip_prefix(shown) else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW3003,
                    format!(
                        "Expected unit `{unit}` after `{num_str}` in `{cursor}` (pattern `{full_pattern}`)"
                    ),
                ));
            };
            let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                Diagnostic::error(
                    DiagnosticCode::CW3003,
                    format!("Invalid number `{num_str}` in `{cursor}`"),
                )
            })?;
            let scale = ty.unit_scale_to_canonical(scale_unit).unwrap_or(1.0);
            Ok((CanonicalValue::Number(num * scale), rest))
        }
    }
}

fn take_numeric_prefix(s: &str) -> Result<(&str, &str), Diagnostic> {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    // A `,` or `.` is part of the number only when a digit follows it, so trailing punctuation
    // (`83/83.`) stays literal text.
    while i < bytes.len()
        && (bytes[i].is_ascii_digit()
            || ((bytes[i] == b',' || bytes[i] == b'.')
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)))
    {
        i += 1;
    }
    if i == digits_start {
        return Err(Diagnostic::error(
            DiagnosticCode::CW3003,
            format!("Expected numeric value at start of `{s}`"),
        ));
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    Ok((&s[..i], &s[i..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_match_inside_lines_but_never_inside_numbers() {
        let count = |_: Option<&str>| Ok(ValueType::Count);
        let p = FormatPattern::parse("passed {p}/{t} with zero skips").unwrap();
        let line = "- The reviewed denominator passed 185/185 with zero skips on";
        let found = p.find_fragments(line, count);
        assert_eq!(found.len(), 1);
        assert_eq!(&line[found[0].0.clone()], "passed 185/185 with zero skips");
        assert_eq!(found[0].1["p"].as_f64(), Some(185.0));

        // Leading placeholder: `{n} tests` must not claim the `62` of `362`.
        let p = FormatPattern::parse("{n} tests").unwrap();
        let found = p.find_fragments("All 362 tests and 4 tests", count);
        let got: Vec<f64> = found
            .iter()
            .map(|(_, v)| v["n"].as_f64().unwrap())
            .collect();
        assert_eq!(got, vec![362.0, 4.0]);

        // Trailing placeholder: `took {n}` must not stop inside `1234`.
        let p = FormatPattern::parse("took {n}").unwrap();
        let found = p.find_fragments("it took 1234 ms", count);
        assert_eq!(found[0].1["n"].as_f64(), Some(1234.0));

        assert!(
            FormatPattern::parse("passes {p}/{t};")
                .unwrap()
                .find_fragments("no match here", count)
                .is_empty()
        );
    }

    #[test]
    fn hidden_units_scale_without_rendering() {
        let dur = |_: Option<&str>| Ok(ValueType::Duration);
        let p = FormatPattern::parse("{t:.3f [ms]}").unwrap();
        let ns = 406_265_917.0;
        let out = p
            .render(|_| Ok((CanonicalValue::Number(ns), ValueType::Duration)))
            .unwrap();
        assert_eq!(out, "406.266");
        let back = p.reverse_parse("406.266", dur).unwrap();
        assert_eq!(back["t"].as_f64(), Some(406_266_000.0));
        // Still type-checked: a hidden unit must be valid for the type.
        assert!(p.validate_types(|_| Ok(ValueType::Duration)).is_ok());
        assert_eq!(
            p.validate_types(|_| Ok(ValueType::Bytes)).unwrap_err().code,
            DiagnosticCode::CW2005
        );
        // And a hidden unit does not terminate the number.
        assert_eq!(
            FormatPattern::parse("{a:.1f [ms]}{b:.1f [ms]}")
                .unwrap_err()
                .code,
            DiagnosticCode::CW1007
        );
    }

    #[test]
    fn invertibility_follows_the_reverse_parser() {
        let count = |_: Option<&str>| Ok(ValueType::Count);
        // Punctuation that cannot continue a number is a valid separator.
        for (pat, text, want) in [
            (
                "reviewed {n}-file matrix",
                "reviewed 52-file matrix",
                vec![52.0],
            ),
            (
                "passes {p}/{t}. Next",
                "passes 83/83. Next",
                vec![83.0, 83.0],
            ),
            ("passes {p}/{t}.", "passes 83/83.", vec![83.0, 83.0]),
            ("{n}, then", "1,234, then", vec![1234.0]),
            ("{n}e", "7e", vec![7.0]),
        ] {
            let p = FormatPattern::parse(pat).unwrap_or_else(|d| panic!("{pat}: {}", d.message));
            let got: Vec<f64> = p
                .reverse_parse(text, count)
                .unwrap_or_else(|d| panic!("{pat}: {}", d.message))
                .values()
                .map(|v| v.as_f64().unwrap())
                .collect();
            assert_eq!(got, want, "{pat}");
        }
        // Literals that a number could absorb stay ambiguous.
        for pat in [
            "{a}.{b}",
            "{a},{b}",
            "{n}5 items",
            "{a}e{b}",
            "{a}.5",
            "{a}e-3",
        ] {
            let err = FormatPattern::parse(pat).expect_err(pat);
            assert_eq!(err.code, DiagnosticCode::CW1007, "{pat}");
        }
    }

    #[test]
    fn test_format_patterns_rendering_and_invertibility() {
        // Ambiguous pattern `{a}{b}` -> CW1007
        let err = FormatPattern::parse("{a}{b}").unwrap_err();
        assert_eq!(err.code, DiagnosticCode::CW1007);

        // Valid composite pattern
        let p = FormatPattern::parse("{verify_wall:.0f ms} ({verify_rss:.0f MiB})").unwrap();
        let rendered = p
            .render(|target| match target {
                Some("verify_wall") => Ok((CanonicalValue::Number(10.4e6), ValueType::Duration)),
                Some("verify_rss") => Ok((
                    CanonicalValue::Number(26.8 * 1024.0 * 1024.0),
                    ValueType::Bytes,
                )),
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(rendered, "10 ms (27 MiB)");

        // Reverse parse
        let parsed = p
            .reverse_parse("10 ms (27 MiB)", |target| match target {
                Some("verify_wall") => Ok(ValueType::Duration),
                Some("verify_rss") => Ok(ValueType::Bytes),
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(parsed["verify_wall"], CanonicalValue::Number(10e6));
        assert_eq!(
            parsed["verify_rss"],
            CanonicalValue::Number(27.0 * 1024.0 * 1024.0)
        );
    }

    #[test]
    fn test_whitespace_significant_units() {
        let p_readme = FormatPattern::parse("~{:.0fk cycles/s}").unwrap();
        let p_bench = FormatPattern::parse("≈ {:.0f k cycles/s}").unwrap();

        let r1 = p_readme
            .render(|_| Ok((CanonicalValue::Number(490636.47), ValueType::Throughput)))
            .unwrap();
        let r2 = p_bench
            .render(|_| Ok((CanonicalValue::Number(490636.47), ValueType::Throughput)))
            .unwrap();

        assert_eq!(r1, "~491k cycles/s");
        assert_eq!(r2, "≈ 491 k cycles/s");
    }

    #[test]
    fn test_auto_iec_and_pow2() {
        assert_eq!(format_auto_iec(16.0 * 1024.0 * 1024.0), "16 MiB");
        assert_eq!(format_auto_iec(112.0 * 1024.0 * 1024.0), "112 MiB");
        assert_eq!(format_auto_iec(8.3 * 1024.0 * 1024.0 * 1024.0), "8.3 GiB");
        assert_eq!(format_pow2(4194304.0).unwrap(), "2^22");
        assert_eq!(
            format_pow2(4194305.0).unwrap_err().code,
            DiagnosticCode::CW3004
        );
    }
}
