//! Diagnostic codes, structured diagnostics, and formatters (`human`, `json`, `github`, `sarif`).

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiagnosticCode {
    // CW1xxx: Syntax & CST
    CW1001, // Malformed marker
    CW1002, // Nested span
    CW1003, // Mismatched close tag
    CW1004, // Unclosed span
    CW1005, // Malformed GFM table under cw:table
    CW1006, // Unmatched cw:block line pattern
    CW1007, // Ambiguous (non-invertible) format pattern
    CW1008, // Unescaped pipe splits a code span in a GFM table

    // CW2xxx: Types & Scope
    CW2001, // Dimensional unit mismatch
    CW2002, // Dependency cycle
    CW2003, // Unresolved claim/table key or selector count mismatch
    CW2004, // Incompatible scope in derivation
    CW2005, // Missing or invalid unit in format spec
    CW2104, // Missing evidence record for non-sentinel cell / empty reduction

    // CW3xxx: Verification & Freshness
    CW3001, // Stale evidence (semantic input digest changed)
    CW3002, // Assertion failed (`assert` is `false`)
    CW3003, // Markdown value out of sync with claim
    CW3004, // Value not representable in format (e.g. pow2 of non-power-of-two)

    // CW4xxx: Conflict & Context
    CW4001, // Unapproved manual edit inside owned span/cell/block
    CW4002, // Surrounding prose context drifted from locked context_digest
}

impl DiagnosticCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CW1001 => "CW1001",
            Self::CW1002 => "CW1002",
            Self::CW1003 => "CW1003",
            Self::CW1004 => "CW1004",
            Self::CW1005 => "CW1005",
            Self::CW1006 => "CW1006",
            Self::CW1007 => "CW1007",
            Self::CW1008 => "CW1008",
            Self::CW2001 => "CW2001",
            Self::CW2002 => "CW2002",
            Self::CW2003 => "CW2003",
            Self::CW2004 => "CW2004",
            Self::CW2005 => "CW2005",
            Self::CW2104 => "CW2104",
            Self::CW3001 => "CW3001",
            Self::CW3002 => "CW3002",
            Self::CW3003 => "CW3003",
            Self::CW3004 => "CW3004",
            Self::CW4001 => "CW4001",
            Self::CW4002 => "CW4002",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::CW1001 => "Malformed Claim Wire marker",
            Self::CW1002 => "Nested Claim Wire span",
            Self::CW1003 => "Mismatched closing tag `<!-- /cw -->`",
            Self::CW1004 => "Unclosed Claim Wire span",
            Self::CW1005 => "Malformed GFM table under `cw:table` directive",
            Self::CW1006 => "Unmatched `cw:block` line pattern",
            Self::CW1007 => "Ambiguous (non-invertible) format pattern",
            Self::CW1008 => "Unescaped pipe splits a code span in a GFM table",
            Self::CW2001 => "Dimensional unit mismatch",
            Self::CW2002 => "Dependency cycle in claim graph",
            Self::CW2003 => "Unresolved claim or table key",
            Self::CW2004 => "Incompatible scope in derivation",
            Self::CW2005 => "Missing or invalid unit in format specification",
            Self::CW2104 => "Missing evidence record for non-sentinel cell",
            Self::CW3001 => "Stale evidence (semantic input digest changed)",
            Self::CW3002 => "Assertion failed",
            Self::CW3003 => "Markdown value out of sync with claim",
            Self::CW3004 => "Value not representable in target format",
            Self::CW4001 => "Manual edit conflict inside owned location",
            Self::CW4002 => "Surrounding prose context drifted from locked context_digest",
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warning => f.write_str("warning"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)
    }
}

impl Diagnostic {
    pub fn error(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            file: None,
            line: None,
            col: None,
            span_len: None,
            snippet_line: None,
            label: None,
            notes: Vec::new(),
            help: None,
        }
    }

    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            file: None,
            line: None,
            col: None,
            span_len: None,
            snippet_line: None,
            label: None,
            notes: Vec::new(),
            help: None,
        }
    }

    pub fn with_location(mut self, file: impl Into<String>, line: usize, col: usize) -> Self {
        self.file = Some(file.into());
        self.line = Some(line);
        self.col = Some(col);
        self
    }

    pub fn with_snippet(
        mut self,
        snippet_line: impl Into<String>,
        span_len: usize,
        label: impl Into<String>,
    ) -> Self {
        self.snippet_line = Some(snippet_line.into());
        self.span_len = Some(span_len.max(1));
        self.label = Some(label.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn format_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{}[{}]: {}\n", self.severity, self.code, self.message));
        if let Some(file) = &self.file {
            let line = self.line.unwrap_or(1);
            let col = self.col.unwrap_or(1);
            let gutter_width = line.to_string().len().max(2);
            let pad = " ".repeat(gutter_width);
            out.push_str(&format!("{pad}--> {file}:{line}:{col}\n"));
            if let Some(snippet) = &self.snippet_line {
                out.push_str(&format!("{pad} |\n"));
                out.push_str(&format!("{line:>gutter_width$} | {snippet}\n"));
                let carets = "^".repeat(self.span_len.unwrap_or(1).max(1));
                let col_pad = " ".repeat(col.saturating_sub(1));
                if let Some(label) = &self.label {
                    out.push_str(&format!("{pad} | {col_pad}{carets} {label}\n"));
                } else {
                    out.push_str(&format!("{pad} | {col_pad}{carets}\n"));
                }
                if !self.notes.is_empty() || self.help.is_some() {
                    out.push_str(&format!("{pad} |\n"));
                }
            }
            for note in &self.notes {
                let mut lines = note.lines();
                if let Some(first) = lines.next() {
                    out.push_str(&format!("{pad} = {first}\n"));
                    for cont in lines {
                        out.push_str(&format!("{pad}   {cont}\n"));
                    }
                }
            }
            if let Some(help) = &self.help {
                out.push_str(&format!("{pad} = help: {help}\n"));
            }
        } else {
            for note in &self.notes {
                out.push_str(&format!("   = {note}\n"));
            }
            if let Some(help) = &self.help {
                out.push_str(&format!("   = help: {help}\n"));
            }
        }
        out
    }

    pub fn format_github(&self) -> String {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut params = Vec::new();
        if let Some(file) = &self.file {
            params.push(format!("file={file}"));
        }
        if let Some(line) = self.line {
            params.push(format!("line={line}"));
        }
        if let Some(col) = self.col {
            params.push(format!("col={col}"));
        }
        params.push(format!("title={}", self.code));
        format!("::{level} {}::{}", params.join(","), self.message)
    }
}

pub fn format_diagnostics_sarif(diagnostics: &[Diagnostic]) -> String {
    let rules: Vec<serde_json::Value> = {
        let mut seen = Vec::new();
        for d in diagnostics {
            if !seen.contains(&d.code) {
                seen.push(d.code);
            }
        }
        seen.into_iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.as_str(),
                    "shortDescription": { "text": c.title() }
                })
            })
            .collect()
    };

    let results: Vec<serde_json::Value> = diagnostics
        .iter()
        .map(|d| {
            let level = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            let mut res = serde_json::json!({
                "ruleId": d.code.as_str(),
                "level": level,
                "message": { "text": d.message }
            });
            if let Some(file) = &d.file {
                res["locations"] = serde_json::json!([{
                    "physicalLocation": {
                        "artifactLocation": { "uri": file },
                        "region": {
                            "startLine": d.line.unwrap_or(1),
                            "startColumn": d.col.unwrap_or(1)
                        }
                    }
                }]);
            }
            res
        })
        .collect();

    let sarif = serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "cw",
                    "version": env!("CARGO_PKG_VERSION"),
                    "rules": rules
                }
            },
            "results": results
        }]
    });
    serde_json::to_string_pretty(&sarif).unwrap_or_default()
}
