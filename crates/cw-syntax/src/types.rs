//! Dimensional `ValueType`, `CanonicalValue`, and unit conversions (`spec.md` §4.1).

use crate::diagnostics::{Diagnostic, DiagnosticCode};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ValueType {
    Duration,
    Bytes,
    Cycles,
    Throughput,
    Count,
    PassRatio,
    Ratio,
    Float,
    Quantity(String),
    Bool,
    String,
    Digest,
}

impl ValueType {
    pub fn parse(s: &str) -> Result<Self, Diagnostic> {
        let s = s.trim();
        match s {
            "Duration" => Ok(Self::Duration),
            "Bytes" => Ok(Self::Bytes),
            "Cycles" => Ok(Self::Cycles),
            "Throughput" => Ok(Self::Throughput),
            "Count" => Ok(Self::Count),
            "PassRatio" => Ok(Self::PassRatio),
            "Ratio" => Ok(Self::Ratio),
            "Float" => Ok(Self::Float),
            "Bool" => Ok(Self::Bool),
            "String" => Ok(Self::String),
            "Digest" => Ok(Self::Digest),
            _ if s.starts_with("Quantity<") && s.ends_with('>') => {
                let unit = s["Quantity<".len()..s.len() - 1].trim().to_string();
                if unit.is_empty() {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW2001,
                        "Quantity<U> requires a non-empty unit tag",
                    ));
                }
                Ok(Self::Quantity(unit))
            }
            _ => Err(Diagnostic::error(
                DiagnosticCode::CW2001,
                format!("Unknown ValueType `{s}`"),
            )),
        }
    }

    pub fn canonical_unit(&self) -> String {
        match self {
            Self::Duration => "ns".to_string(),
            Self::Bytes => "B".to_string(),
            Self::Cycles => "cycles".to_string(),
            Self::Throughput => "cycles/s".to_string(),
            Self::Count => "count".to_string(),
            Self::PassRatio => "pass_ratio".to_string(),
            Self::Ratio | Self::Float => "ratio".to_string(),
            Self::Quantity(u) => u.clone(),
            Self::Bool => "bool".to_string(),
            Self::String => "string".to_string(),
            Self::Digest => "digest".to_string(),
        }
    }

    /// Default deadband fraction (`0.0` for deterministic metrics).
    pub fn is_deterministic_by_default(&self) -> bool {
        matches!(
            self,
            Self::Bytes | Self::Cycles | Self::Count | Self::PassRatio | Self::Digest | Self::Bool | Self::String
        )
    }

    /// Returns whether a numeric format specification MUST include an explicit unit (§4.5 rule 1).
    pub fn requires_explicit_unit(&self) -> bool {
        matches!(self, Self::Duration | Self::Bytes | Self::Throughput)
    }

    /// Returns the scaling divisor to convert from canonical storage to display unit:
    /// `display_value = canonical_value / scale_to_canonical(unit)`.
    /// Note: For `%` on `Ratio`, `scale_to_canonical("%")` is `0.01` so `display = canonical / 0.01 = canonical * 100`.
    pub fn unit_scale_to_canonical(&self, raw_unit: &str) -> Option<f64> {
        let u = raw_unit.trim();
        match self {
            Self::Duration => match u {
                "ns" => Some(1.0),
                "us" | "µs" | "μs" => Some(1e3),
                "ms" => Some(1e6),
                "s" => Some(1e9),
                "min" => Some(60.0 * 1e9),
                _ => None,
            },
            Self::Bytes => match u {
                "B" | "bytes" => Some(1.0),
                "KiB" => Some(1024.0),
                "MiB" => Some(1024.0 * 1024.0),
                "GiB" => Some(1024.0 * 1024.0 * 1024.0),
                "KB" => Some(1e3),
                "MB" => Some(1e6),
                "GB" => Some(1e9),
                _ => None,
            },
            Self::Cycles => match u {
                "" | "cycles" => Some(1.0),
                "k cycles" | "kcycles" => Some(1e3),
                "M cycles" | "Mcycles" => Some(1e6),
                _ => None,
            },
            Self::Throughput => match u {
                "cycles/s" => Some(1.0),
                "k cycles/s" | "kcycles/s" => Some(1e3),
                "M cycles/s" | "Mcycles/s" => Some(1e6),
                _ => None,
            },
            Self::Count => match u {
                "" => Some(1.0),
                _ => None,
            },
            Self::Ratio | Self::Float => match u {
                "" | "×" | "x" => Some(1.0),
                "%" => Some(0.01),
                _ => None,
            },
            Self::Quantity(expected_u) => {
                if u.is_empty() || u == expected_u {
                    Some(1.0)
                } else {
                    None
                }
            }
            Self::PassRatio => match u {
                "" => Some(1.0),
                "%" => Some(0.01),
                _ => None,
            },
            Self::Bool | Self::String | Self::Digest => {
                if u.is_empty() {
                    Some(1.0)
                } else {
                    None
                }
            }
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duration => f.write_str("Duration"),
            Self::Bytes => f.write_str("Bytes"),
            Self::Cycles => f.write_str("Cycles"),
            Self::Throughput => f.write_str("Throughput"),
            Self::Count => f.write_str("Count"),
            Self::PassRatio => f.write_str("PassRatio"),
            Self::Ratio => f.write_str("Ratio"),
            Self::Float => f.write_str("Float"),
            Self::Quantity(u) => write!(f, "Quantity<{u}>"),
            Self::Bool => f.write_str("Bool"),
            Self::String => f.write_str("String"),
            Self::Digest => f.write_str("Digest"),
        }
    }
}

impl Serialize for ValueType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ValueType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        ValueType::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CanonicalValue {
    Null,
    Bool(bool),
    Number(f64),
    PassRatio { passed: u64, total: u64 },
    Text(String),
}

impl CanonicalValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::PassRatio { passed, total } => {
                if *total == 0 {
                    None
                } else {
                    Some(*passed as f64 / *total as f64)
                }
            }
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn to_json_number_or_val(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Number(n) => {
                if n.fract() == 0.0 && *n >= (i64::MIN as f64) && *n <= (i64::MAX as f64) {
                    serde_json::Value::Number((*n as i64).into())
                } else {
                    serde_json::Number::from_f64(*n)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null)
                }
            }
            Self::PassRatio { passed, total } => {
                serde_json::json!({ "passed": passed, "total": total })
            }
            Self::Text(s) => serde_json::Value::String(s.clone()),
        }
    }

    /// Parse a typed literal (e.g. `"8.5487s"`, `"599 KiB"`, `"120.0 bits"`, `"4194304"`, `"83/83"`) into canonical storage.
    pub fn parse_literal(raw: &str, ty: &ValueType) -> Result<Self, Diagnostic> {
        let trimmed = raw.trim();
        if trimmed == "null" || trimmed == "~" || trimmed == "exact" || trimmed == "—" {
            return Ok(Self::Null);
        }
        match ty {
            ValueType::Bool => match trimmed {
                "true" | "Yes" | "Accepted" => Ok(Self::Bool(true)),
                "false" | "No" | "Rejected" => Ok(Self::Bool(false)),
                _ => Err(Diagnostic::error(
                    DiagnosticCode::CW2001,
                    format!("Invalid Bool literal `{trimmed}`"),
                )),
            },
            ValueType::String | ValueType::Digest => Ok(Self::Text(trimmed.to_string())),
            ValueType::PassRatio => {
                if let Some((l, r)) = trimmed.split_once('/') {
                    let passed = l.trim().replace(',', "").parse::<u64>().map_err(|_| {
                        Diagnostic::error(
                            DiagnosticCode::CW2001,
                            format!("Invalid PassRatio numerator in `{trimmed}`"),
                        )
                    })?;
                    let total = r.trim().replace(',', "").parse::<u64>().map_err(|_| {
                        Diagnostic::error(
                            DiagnosticCode::CW2001,
                            format!("Invalid PassRatio denominator in `{trimmed}`"),
                        )
                    })?;
                    Ok(Self::PassRatio { passed, total })
                } else {
                    Err(Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Expected `passed/total` for PassRatio, got `{trimmed}`"),
                    ))
                }
            }
            ValueType::Count => {
                let clean = trimmed.replace(',', "");
                let val = clean.parse::<f64>().map_err(|_| {
                    Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Invalid Count literal `{trimmed}`"),
                    )
                })?;
                Ok(Self::Number(val))
            }
            ValueType::Cycles => {
                if let Some(exp_str) = trimmed.strip_prefix("2^") {
                    if let Ok(exp) = exp_str.trim().parse::<u32>() {
                        return Ok(Self::Number(2f64.powi(exp as i32)));
                    }
                }
                let (num_str, unit_str) = split_numeric_and_unit(trimmed);
                let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                    Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Invalid Cycles number in `{trimmed}`"),
                    )
                })?;
                let scale = ty.unit_scale_to_canonical(unit_str).ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2005,
                        format!("Invalid unit `{unit_str}` for type `Cycles` in `{trimmed}`"),
                    )
                })?;
                Ok(Self::Number(num * scale))
            }
            ValueType::Ratio | ValueType::Float => {
                if let Some(exp_str) = trimmed.strip_prefix("2^-") {
                    if let Ok(exp) = exp_str.trim().parse::<i32>() {
                        return Ok(Self::Number(2f64.powi(-exp)));
                    }
                }
                let (num_str, unit_str) = split_numeric_and_unit(trimmed);
                let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                    Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Invalid Ratio/Float number in `{trimmed}`"),
                    )
                })?;
                let scale = ty.unit_scale_to_canonical(unit_str).ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2005,
                        format!("Invalid unit `{unit_str}` for type `{ty}` in `{trimmed}`"),
                    )
                })?;
                Ok(Self::Number(num * scale))
            }
            ValueType::Duration
            | ValueType::Bytes
            | ValueType::Throughput
            | ValueType::Quantity(_) => {
                let (num_str, unit_str) = split_numeric_and_unit(trimmed);
                if unit_str.trim().is_empty() {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW2005,
                        format!("Missing unit for type `{ty}` in `{trimmed}`"),
                    ));
                }
                let num = num_str.replace(',', "").parse::<f64>().map_err(|_| {
                    Diagnostic::error(
                        DiagnosticCode::CW2001,
                        format!("Invalid numeric literal `{num_str}` in `{trimmed}`"),
                    )
                })?;
                let scale = ty.unit_scale_to_canonical(unit_str).ok_or_else(|| {
                    Diagnostic::error(
                        DiagnosticCode::CW2005,
                        format!("Invalid unit `{}` for type `{ty}`", unit_str.trim()),
                    )
                })?;
                Ok(Self::Number(num * scale))
            }
        }
    }
}

/// Splits `"8.5487s"` or `"599 KiB"` or `"-120.0 bits"` into `("8.5487", "s")`.
pub fn split_numeric_and_unit(s: &str) -> (&str, &str) {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b',' || bytes[i] == b'.') {
        i += 1;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let exp_start = i + 1;
        let mut j = exp_start;
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
    (&s[..i], &s[i..])
}
