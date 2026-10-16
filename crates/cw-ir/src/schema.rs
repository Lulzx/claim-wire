//! Schema definitions for `.claimwire/claims.yaml`.

use cw_syntax::{Diagnostic, DiagnosticCode, ValueType};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimsConfig {
    pub schema: String,
    #[serde(default)]
    pub aliases: IndexMap<String, IndexMap<String, Vec<String>>>,
    #[serde(default)]
    pub constants: IndexMap<String, ConstantDecl>,
    #[serde(default)]
    pub families: IndexMap<String, FamilyDecl>,
    #[serde(default)]
    pub claims: IndexMap<String, ScalarClaimDecl>,
    #[serde(default)]
    pub assertions: IndexMap<String, AssertionDecl>,
    #[serde(default)]
    pub blocks: IndexMap<String, BlockLensDecl>,
    #[serde(default)]
    pub tables: IndexMap<String, TableLensDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantDecl {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    pub value: String,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyDecl {
    pub producer: String,
    pub dimensions: Vec<String>,
    #[serde(default)]
    pub metrics: IndexMap<String, MetricDecl>,
    #[serde(default)]
    pub derivations: IndexMap<String, DerivationDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDecl {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub deadband: Option<String>,
}

impl MetricDecl {
    pub fn deadband_fraction(&self) -> Result<f64, Diagnostic> {
        parse_deadband(self.deadband.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivationDecl {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    pub expr: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub deadband: Option<String>,
}

impl DerivationDecl {
    pub fn deadband_fraction(&self) -> Result<f64, Diagnostic> {
        parse_deadband(self.deadband.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalarClaimDecl {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub producer: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(rename = "type")]
    pub value_type: ValueType,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub deadband: Option<String>,
}

impl ScalarClaimDecl {
    pub fn effective_kind(&self) -> &str {
        if let Some(k) = &self.kind {
            return k.as_str();
        }
        if self.expr.is_some() {
            "derive"
        } else if self.value.is_some() {
            "const"
        } else {
            "source"
        }
    }

    pub fn deadband_fraction(&self) -> Result<f64, Diagnostic> {
        parse_deadband(self.deadband.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionDecl {
    pub expr: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockLensDecl {
    pub file: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TableMode {
    #[default]
    Records,
    Transposed,
    Checklist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnScopeDecl {
    pub family: String,
    #[serde(default)]
    pub epoch: Option<String>,
    pub scope: Vec<String>,
}

impl ColumnScopeDecl {
    pub fn effective_epoch(&self) -> &str {
        self.epoch.as_deref().unwrap_or("current")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TableRowSpec {
    /// Used in `transposed` mode where each row key maps to a format pattern string
    Pattern(String),
    /// Used in `checklist` mode where each row key maps to `{ ColumnHeader: FormatPattern }`
    Columns(IndexMap<String, String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableLensDecl {
    pub file: String,
    #[serde(default)]
    pub mode: TableMode,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub epoch: Option<String>,
    #[serde(default)]
    pub key_columns: IndexMap<String, String>,
    #[serde(default)]
    pub default_scope: IndexMap<String, String>,
    #[serde(default)]
    pub columns: IndexMap<String, String>,
    #[serde(default)]
    pub row_overrides: IndexMap<String, IndexMap<String, String>>,
    #[serde(default)]
    pub key_column: Option<String>,
    #[serde(default)]
    pub column_scopes: IndexMap<String, ColumnScopeDecl>,
    #[serde(default)]
    pub rows: IndexMap<String, TableRowSpec>,
    #[serde(default)]
    pub column_overrides: IndexMap<String, IndexMap<String, String>>,
    #[serde(default = "default_sentinels")]
    pub sentinels: Vec<String>,
    #[serde(default)]
    pub cell_patterns: IndexMap<String, String>,
}

fn default_sentinels() -> Vec<String> {
    vec!["—".to_string(), "N/A".to_string()]
}

impl TableLensDecl {
    pub fn effective_epoch(&self) -> &str {
        self.epoch.as_deref().unwrap_or("current")
    }

    pub fn is_sentinel(&self, cell_text: &str) -> bool {
        let t = cell_text.trim();
        let stripped = t.trim_matches('`').trim_matches('*').trim();
        self.sentinels
            .iter()
            .any(|s| s.trim() == t || s.trim() == stripped)
    }
}

pub fn parse_deadband(raw: Option<&str>) -> Result<f64, Diagnostic> {
    let Some(s) = raw else {
        return Ok(0.0);
    };
    let s = s.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let v = pct.trim().parse::<f64>().map_err(|_| {
            Diagnostic::error(
                DiagnosticCode::CW2001,
                format!("Invalid deadband percentage `{s}`"),
            )
        })?;
        Ok(v / 100.0)
    } else {
        let v = s.parse::<f64>().map_err(|_| {
            Diagnostic::error(DiagnosticCode::CW2001, format!("Invalid deadband `{s}`"))
        })?;
        Ok(v)
    }
}
