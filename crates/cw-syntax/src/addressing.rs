//! Claim Addressing Grammar (`spec.md` §4.3) and Two-Step Key Canonicalization (§5.2.2).

use crate::diagnostics::{Diagnostic, DiagnosticCode};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClaimRef {
    Scalar(ScalarRef),
    Family(FamilyRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScalarRef {
    pub segments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FamilyRef {
    pub family: String,
    pub epoch: Option<String>,
    pub selectors: Vec<Selector>,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Selector {
    Wildcard,
    Point(SelectorKey),
    Subset(Vec<SelectorKey>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SelectorKey {
    pub value: String,
    pub was_quoted: bool,
}

impl SelectorKey {
    pub fn bare(val: impl Into<String>) -> Self {
        Self {
            value: val.into(),
            was_quoted: false,
        }
    }

    pub fn quoted(val: impl Into<String>) -> Self {
        Self {
            value: val.into(),
            was_quoted: true,
        }
    }

    pub fn needs_quotes(s: &str) -> bool {
        if s.is_empty() {
            return true;
        }
        let bytes = s.as_bytes();
        if !(bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_') {
            return true;
        }
        for &b in &bytes[1..] {
            if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'+' || b == b'-') {
                return true;
            }
        }
        false
    }

    pub fn canonical_string(&self) -> String {
        if Self::needs_quotes(&self.value) {
            let mut out = String::with_capacity(self.value.len() + 2);
            out.push('"');
            for c in self.value.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            out
        } else {
            self.value.clone()
        }
    }
}

impl fmt::Display for SelectorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_string())
    }
}

impl Selector {
    pub fn is_multi(&self) -> bool {
        matches!(self, Self::Wildcard | Self::Subset(_))
    }

    pub fn canonical_string(&self) -> String {
        match self {
            Self::Wildcard => "*".to_string(),
            Self::Point(k) => k.canonical_string(),
            Self::Subset(keys) => {
                let parts: Vec<String> = keys.iter().map(|k| k.canonical_string()).collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_string())
    }
}

impl ScalarRef {
    pub fn canonical_string(&self) -> String {
        self.segments.join(".")
    }
}

impl fmt::Display for ScalarRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_string())
    }
}

impl FamilyRef {
    pub fn effective_epoch(&self) -> &str {
        self.epoch.as_deref().unwrap_or("current")
    }

    pub fn canonical_string(&self) -> String {
        let sels: Vec<String> = self
            .selectors
            .iter()
            .map(|s| s.canonical_string())
            .collect();
        match &self.epoch {
            Some(ep) if ep != "current" => {
                format!("{}@{}[{}].{}", self.family, ep, sels.join(", "), self.field)
            }
            _ => format!("{}[{}].{}", self.family, sels.join(", "), self.field),
        }
    }
}

impl fmt::Display for FamilyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_string())
    }
}

impl ClaimRef {
    pub fn canonical_string(&self) -> String {
        match self {
            Self::Scalar(s) => s.canonical_string(),
            Self::Family(f) => f.canonical_string(),
        }
    }

    pub fn parse(input: &str) -> Result<Self, Diagnostic> {
        let s = input.trim();
        if s.is_empty() {
            return Err(Diagnostic::error(
                DiagnosticCode::CW1001,
                "Empty claim reference",
            ));
        }
        if let Some(bracket_start) = s.find('[') {
            // FamilyRef: Identifier ("@" EpochId)? "[" SelectorList "]" "." Identifier
            let prefix = &s[..bracket_start];
            let (family, epoch) = if let Some((fam, ep)) = prefix.split_once('@') {
                validate_identifier(fam, "family name")?;
                validate_identifier(ep, "epoch identifier")?;
                (fam.to_string(), Some(ep.to_string()))
            } else {
                validate_identifier(prefix, "family name")?;
                (prefix.to_string(), None)
            };

            let Some(bracket_end) = find_closing_bracket(s, bracket_start) else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1001,
                    format!("Unclosed `[` in family reference `{s}`"),
                ));
            };
            let sel_str = &s[bracket_start + 1..bracket_end];
            let selectors = parse_selector_list(sel_str)?;

            let suffix = &s[bracket_end + 1..];
            let Some(field) = suffix.strip_prefix('.') else {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1001,
                    format!("Expected `.field` after `]` in family reference `{s}`"),
                ));
            };
            validate_identifier(field, "metric/derivation field")?;

            Ok(Self::Family(FamilyRef {
                family,
                epoch,
                selectors,
                field: field.to_string(),
            }))
        } else {
            // ScalarRef: Identifier ("." Identifier)*
            let segments: Vec<String> = s.split('.').map(|seg| seg.to_string()).collect();
            for seg in &segments {
                validate_identifier(seg, "scalar identifier segment")?;
            }
            if matches!(
                segments.first().map(|x| x.as_str()),
                Some("table" | "block")
            ) {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1001,
                    format!(
                        "`{}` is a reserved lens keyword and cannot be the first segment of a scalar claim reference",
                        segments[0]
                    ),
                ));
            }
            Ok(Self::Scalar(ScalarRef { segments }))
        }
    }
}

impl fmt::Display for ClaimRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_string())
    }
}

pub fn validate_identifier(s: &str, context: &str) -> Result<(), Diagnostic> {
    if s.is_empty() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!("Empty {context}"),
        ));
    }
    let bytes = s.as_bytes();
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!("Invalid {context} `{s}`: must start with [A-Za-z_]"),
        ));
    }
    for &b in &bytes[1..] {
        if !(b.is_ascii_alphanumeric() || b == b'_') {
            return Err(Diagnostic::error(
                DiagnosticCode::CW1001,
                format!("Invalid character in {context} `{s}`"),
            ));
        }
    }
    Ok(())
}

fn find_closing_bracket(s: &str, open_idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_quotes = false;
    let mut i = open_idx + 1;
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
            if b == b'"' {
                in_quotes = true;
            } else if b == b']' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

pub fn parse_selector_list(input: &str) -> Result<Vec<Selector>, Diagnostic> {
    let s = input.trim();
    if s.is_empty() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            "Selector list inside `[...]` cannot be empty",
        ));
    }
    let parts = split_selector_commas(s)?;
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let p = part.trim();
        if p == "*" {
            out.push(Selector::Wildcard);
        } else if p.starts_with('{') && p.ends_with('}') {
            let inner = p[1..p.len() - 1].trim();
            if inner.is_empty() {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1001,
                    "Subset selector `{...}` cannot be empty",
                ));
            }
            let sub_parts = split_selector_commas(inner)?;
            let mut keys = Vec::with_capacity(sub_parts.len());
            for sp in sub_parts {
                keys.push(parse_selector_key(sp.trim())?);
            }
            out.push(Selector::Subset(keys));
        } else {
            out.push(Selector::Point(parse_selector_key(p)?));
        }
    }
    Ok(out)
}

fn split_selector_commas(s: &str) -> Result<Vec<&str>, Diagnostic> {
    let bytes = s.as_bytes();
    let mut in_quotes = false;
    let mut brace_depth = 0usize;
    let mut start = 0usize;
    let mut out = Vec::new();
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
                b'{' => brace_depth += 1,
                b'}' => {
                    if brace_depth == 0 {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!("Unmatched `}}` in selector `{s}`"),
                        ));
                    }
                    brace_depth -= 1;
                }
                b',' if brace_depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if in_quotes || brace_depth != 0 {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!("Unclosed quote or brace in selector `{s}`"),
        ));
    }
    out.push(&s[start..]);
    Ok(out)
}

pub fn parse_selector_key(s: &str) -> Result<SelectorKey, Diagnostic> {
    if s.is_empty() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            "Empty selector key",
        ));
    }
    if s.contains('|') {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!("Pipes (`|`) are not allowed in selector keys: `{s}`"),
        ));
    }
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        let mut unescaped = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => unescaped.push('"'),
                    Some('\\') => unescaped.push('\\'),
                    Some(other) => {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!("Invalid escape `\\{other}` in quoted key `{s}`"),
                        ));
                    }
                    None => {
                        return Err(Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!("Trailing backslash in quoted key `{s}`"),
                        ));
                    }
                }
            } else {
                unescaped.push(c);
            }
        }
        return Ok(SelectorKey {
            value: unescaped,
            was_quoted: true,
        });
    }

    // BareKey or Integer: [A-Za-z0-9_] [A-Za-z0-9_.+-]*
    if SelectorKey::needs_quotes(s) {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!(
                "Selector key `{s}` contains special characters and must be quoted as `\"{s}\"`"
            ),
        ));
    }
    Ok(SelectorKey {
        value: s.to_string(),
        was_quoted: false,
    })
}

/// Format a scope tuple `[k1, k2]` canonically as `"[radix-sort-u64, test]"` or `"[fib(19), test]"`
pub fn format_scope_bracketed(scope: &[String]) -> String {
    let parts: Vec<String> = scope
        .iter()
        .map(|k| SelectorKey::bare(k.clone()).canonical_string())
        .collect();
    format!("[{}]", parts.join(", "))
}

/// Parse a `row_overrides` key like `"[radix-sort-u64, test]"` or `"[fib(19), test]"` into scope strings.
pub fn parse_row_override_key(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    inner
        .split(',')
        .map(|part| {
            let p = part.trim();
            if p.starts_with('"') && p.ends_with('"') && p.len() >= 2 {
                p[1..p.len() - 1]
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\")
            } else {
                p.to_string()
            }
        })
        .collect()
}

/// Two-step key canonicalization (`spec.md` §5.2.2):
/// 1. Strip all inline Markdown emphasis/code characters (`` ` ``, `*`, `_`) and trim whitespace.
///    Match against `known_keys` and `aliases` (map of `canonical_key -> Vec<alias>`).
/// 2. If still unresolved, strip trailing parenthetical qualifiers preceded by at least one whitespace
///    character (`\s+\([^)]*\)$`), preserving function-call identifiers like `fib(19)` that have no space before `(`,
///    then match against `known_keys` and `aliases`.
pub fn canonicalize_table_key(
    raw_cell: &str,
    known_keys: &[String],
    aliases: Option<&IndexMap<String, Vec<String>>>,
) -> Option<String> {
    // Also try matching raw_cell trimmed directly (in case an alias includes backticks)
    let raw_trimmed = raw_cell.trim();
    if let Some(matched) = match_against_known_and_aliases(raw_trimmed, known_keys, aliases) {
        return Some(matched);
    }

    // Step 1: Decorator strip
    let step1: String = raw_trimmed
        .chars()
        .filter(|&c| c != '`' && c != '*' && c != '_')
        .collect::<String>()
        .trim()
        .to_string();

    if let Some(matched) = match_against_known_and_aliases(&step1, known_keys, aliases) {
        return Some(matched);
    }

    // Step 2: Parenthetical qualifier strip (`\s+\([^)]*\)$`, potentially repeated if multiple trailing parentheticals)
    let mut step2 = step1.clone();
    while let Some(stripped) = strip_trailing_whitespace_paren(&step2) {
        step2 = stripped.to_string();
        if let Some(matched) = match_against_known_and_aliases(&step2, known_keys, aliases) {
            return Some(matched);
        }
    }

    // If known_keys is empty (e.g. open matching), return step2
    if known_keys.is_empty() && aliases.is_none() {
        return Some(step2);
    }

    None
}

fn match_against_known_and_aliases(
    candidate: &str,
    known_keys: &[String],
    aliases: Option<&IndexMap<String, Vec<String>>>,
) -> Option<String> {
    for k in known_keys {
        if k == candidate {
            return Some(k.clone());
        }
        // Also check decorator-stripped known key
        let k_stripped: String = k
            .chars()
            .filter(|&c| c != '`' && c != '*' && c != '_')
            .collect::<String>()
            .trim()
            .to_string();
        if k_stripped == candidate {
            return Some(k.clone());
        }
    }
    if let Some(alias_map) = aliases {
        for (canon, alias_list) in alias_map {
            if canon == candidate {
                return Some(canon.clone());
            }
            for al in alias_list {
                let al_stripped: String = al
                    .chars()
                    .filter(|&c| c != '`' && c != '*' && c != '_')
                    .collect::<String>()
                    .trim()
                    .to_string();
                if al == candidate || al_stripped == candidate {
                    return Some(canon.clone());
                }
            }
        }
    }
    None
}

fn strip_trailing_whitespace_paren(s: &str) -> Option<&str> {
    if !s.ends_with(')') {
        return None;
    }
    let open_idx = s.rfind('(')?;
    if open_idx == 0 {
        return None;
    }
    // Check that the character immediately before `(` is whitespace
    let before = &s[..open_idx];
    if !before.ends_with(char::is_whitespace) {
        return None;
    }
    // Ensure no `)` inside `(`..`)`
    let inside = &s[open_idx + 1..s.len() - 1];
    if inside.contains(')') {
        return None;
    }
    Some(before.trim_end())
}

/// Outer decorator on a table cell (`**...**` or `` `...` ``), per §5.2.2 step 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OuterDecorator {
    None,
    Bold, // **...**
    Code, // `...`
}

impl OuterDecorator {
    /// Detect if `cell_text` is completely wrapped in outer `**...**` or `` `...` ``.
    /// Note: `**8.55 s**` is `Bold` because there is no `**` inside `8.55 s`.
    /// By contrast, `**8.82 s** (≈ 476 k cycles/s)` is NOT wrapped in outer bold because it ends with `)`.
    pub fn detect(cell_text: &str) -> (Self, &str) {
        let t = cell_text.trim();
        if t.len() >= 4 && t.starts_with("**") && t.ends_with("**") {
            let inner = &t[2..t.len() - 2];
            if !inner.contains("**") {
                return (Self::Bold, inner);
            }
        }
        if t.len() >= 2 && t.starts_with('`') && t.ends_with('`') {
            let inner = &t[1..t.len() - 1];
            if !inner.contains('`') {
                return (Self::Code, inner);
            }
        }
        (Self::None, t)
    }

    pub fn apply(&self, rendered: &str, pattern_has_decorator: bool) -> String {
        if pattern_has_decorator {
            return rendered.to_string();
        }
        match self {
            Self::None => rendered.to_string(),
            Self::Bold => format!("**{rendered}**"),
            Self::Code => format!("`{rendered}`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_claim_refs() {
        let r1 = ClaimRef::parse("inventory.python_tests").unwrap();
        assert_eq!(r1.canonical_string(), "inventory.python_tests");

        let r2 = ClaimRef::parse("guest_bench[radix-sort-u64, test].throughput").unwrap();
        assert_eq!(
            r2.canonical_string(),
            "guest_bench[radix-sort-u64, test].throughput"
        );

        let r3 = ClaimRef::parse("guest_bench@initial[\"fib(19)\", test].prove_wall").unwrap();
        assert_eq!(
            r3.canonical_string(),
            "guest_bench@initial[\"fib(19)\", test].prove_wall"
        );

        let r4 = ClaimRef::parse("phase_bench[{register_twist, ram_twist}].duration").unwrap();
        assert_eq!(
            r4.canonical_string(),
            "phase_bench[{register_twist, ram_twist}].duration"
        );

        // Reserved words rejected
        assert!(ClaimRef::parse("table.foo").is_err());
        assert!(ClaimRef::parse("block.bar").is_err());
    }

    #[test]
    fn test_two_step_key_canonicalization() {
        let mut aliases = IndexMap::new();
        aliases.insert(
            "fib(19)".to_string(),
            vec!["fibonacci, fib(19) = 4181".to_string()],
        );
        aliases.insert(
            "sha256".to_string(),
            vec![
                "sha256(\"abc\")".to_string(),
                "sha256 of \"abc\"".to_string(),
            ],
        );
        let known = vec![
            "fib(19)".to_string(),
            "sha256".to_string(),
            "radix-sort-u64".to_string(),
        ];

        assert_eq!(
            canonicalize_table_key("`fib(19)`", &known, Some(&aliases)),
            Some("fib(19)".to_string())
        );
        assert_eq!(
            canonicalize_table_key("fibonacci, fib(19) = 4181", &known, Some(&aliases)),
            Some("fib(19)".to_string())
        );
        assert_eq!(
            canonicalize_table_key("`radix-sort-u64` (`A = 17`)", &known, Some(&aliases)),
            Some("radix-sort-u64".to_string())
        );
        assert_eq!(
            canonicalize_table_key("sha256 of `\"abc\"`", &known, Some(&aliases)),
            Some("sha256".to_string())
        );
    }
}
