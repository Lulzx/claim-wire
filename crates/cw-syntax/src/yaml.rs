//! Span-aware YAML 1.2 parser and serializer for `.claimwire/*.yaml`.

use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceSpan {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct YamlNode {
    pub span: SourceSpan,
    pub value: YamlValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum YamlValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<YamlNode>),
    Mapping(IndexMap<String, YamlNode>),
}

impl YamlNode {
    pub fn to_json_value(&self) -> serde_json::Value {
        match &self.value {
            YamlValue::Null => serde_json::Value::Null,
            YamlValue::Bool(b) => serde_json::Value::Bool(*b),
            YamlValue::Int(i) => serde_json::Value::Number((*i).into()),
            YamlValue::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            YamlValue::String(s) => serde_json::Value::String(s.clone()),
            YamlValue::Sequence(seq) => {
                serde_json::Value::Array(seq.iter().map(|n| n.to_json_value()).collect())
            }
            YamlValue::Mapping(map) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map {
                    obj.insert(k.clone(), v.to_json_value());
                }
                serde_json::Value::Object(obj)
            }
        }
    }

    pub fn get_path_span(&self, path: &[&str]) -> Option<SourceSpan> {
        let mut cur = self;
        for &seg in path {
            match &cur.value {
                YamlValue::Mapping(m) => {
                    cur = m.get(seg)?;
                }
                _ => return None,
            }
        }
        Some(cur.span)
    }
}

pub fn from_yaml_str<T: DeserializeOwned>(input: &str) -> Result<(T, YamlNode)> {
    let node = parse_yaml(input)?;
    let json_val = node.to_json_value();
    let parsed: T = serde_json::from_value(json_val)
        .map_err(|e| anyhow!("YAML schema deserialization error: {e}"))?;
    Ok((parsed, node))
}

#[derive(Debug, Clone)]
struct Line<'a> {
    line_no: usize,
    indent: usize,
    content: &'a str, // trimmed of leading indent and trailing comment/whitespace
}

/// Strip YAML comment `# ...` outside single or double quotes.
/// Note: In YAML, `#` starts a comment only when preceded by whitespace or at start of line.
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
        } else if in_double {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
        } else {
            if b == b'\'' {
                in_single = true;
            } else if b == b'"' {
                in_double = true;
            } else if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                return raw[..i].trim_end();
            }
        }
        i += 1;
    }
    raw.trim_end()
}

pub fn parse_yaml(input: &str) -> Result<YamlNode> {
    let mut lines = Vec::new();
    for (idx, raw_line) in input.lines().enumerate() {
        let stripped = strip_comment(raw_line);
        let trimmed = stripped.trim_start_matches(' ');
        if trimmed.is_empty() || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = stripped.len() - trimmed.len();
        lines.push(Line {
            line_no: idx + 1,
            indent,
            content: trimmed,
        });
    }

    if lines.is_empty() {
        return Ok(YamlNode {
            span: SourceSpan { line: 1, col: 1 },
            value: YamlValue::Mapping(IndexMap::new()),
        });
    }

    let mut pos = 0;
    parse_block(&lines, &mut pos, lines[0].indent)
}

fn parse_block(lines: &[Line<'_>], pos: &mut usize, min_indent: usize) -> Result<YamlNode> {
    if *pos >= lines.len() {
        return Ok(YamlNode {
            span: SourceSpan { line: 1, col: 1 },
            value: YamlValue::Null,
        });
    }

    let first = &lines[*pos];
    if first.content.starts_with("- ") || first.content == "-" {
        parse_block_sequence(lines, pos, first.indent)
    } else if first.content.starts_with('{') || first.content.starts_with('[') {
        let span = SourceSpan {
            line: first.line_no,
            col: first.indent + 1,
        };
        let val = parse_inline_value(first.content, span)?;
        *pos += 1;
        Ok(val)
    } else {
        parse_block_mapping(lines, pos, min_indent.max(first.indent))
    }
}

fn parse_block_sequence(
    lines: &[Line<'_>],
    pos: &mut usize,
    seq_indent: usize,
) -> Result<YamlNode> {
    let start_span = SourceSpan {
        line: lines[*pos].line_no,
        col: seq_indent + 1,
    };
    let mut items = Vec::new();

    while *pos < lines.len() {
        let line = &lines[*pos];
        if line.indent < seq_indent {
            break;
        }
        if line.indent > seq_indent {
            bail!(
                "Unexpected indentation at line {}: expected {}",
                line.line_no,
                seq_indent
            );
        }
        if !line.content.starts_with('-') {
            break;
        }
        let after_dash = line.content[1..].trim_start();
        let item_span = SourceSpan {
            line: line.line_no,
            col: line.indent + 3,
        };
        if after_dash.is_empty() {
            *pos += 1;
            if *pos < lines.len() && lines[*pos].indent > seq_indent {
                let child_indent = lines[*pos].indent;
                items.push(parse_block(lines, pos, child_indent)?);
            } else {
                items.push(YamlNode {
                    span: item_span,
                    value: YamlValue::Null,
                });
            }
        } else if let Some((k, v)) = split_key_value(after_dash) {
            // Inline mapping item starting on the `- ` line
            let item_map_indent = line.indent + 2;
            let mut map = IndexMap::new();
            let key = unquote_str(k.trim())?;
            let val_str = v.trim();
            *pos += 1;
            let val_node = if val_str.is_empty() {
                if *pos < lines.len() && lines[*pos].indent > item_map_indent {
                    let child_indent = lines[*pos].indent;
                    parse_block(lines, pos, child_indent)?
                } else {
                    YamlNode {
                        span: item_span,
                        value: YamlValue::Null,
                    }
                }
            } else {
                parse_inline_value(val_str, item_span)?
            };
            map.insert(key, val_node);
            while *pos < lines.len() && lines[*pos].indent >= item_map_indent {
                if lines[*pos].indent == seq_indent && lines[*pos].content.starts_with('-') {
                    break;
                }
                let sub_line = &lines[*pos];
                if let Some((sk, sv)) = split_key_value(sub_line.content) {
                    let skey = unquote_str(sk.trim())?;
                    let sval_str = sv.trim();
                    let sspan = SourceSpan {
                        line: sub_line.line_no,
                        col: sub_line.indent + 1,
                    };
                    *pos += 1;
                    let snode = if sval_str.is_empty() {
                        if *pos < lines.len() && lines[*pos].indent > sub_line.indent {
                            let c_indent = lines[*pos].indent;
                            parse_block(lines, pos, c_indent)?
                        } else {
                            YamlNode {
                                span: sspan,
                                value: YamlValue::Null,
                            }
                        }
                    } else {
                        parse_inline_value(sval_str, sspan)?
                    };
                    map.insert(skey, snode);
                } else {
                    break;
                }
            }
            items.push(YamlNode {
                span: item_span,
                value: YamlValue::Mapping(map),
            });
        } else {
            *pos += 1;
            items.push(parse_inline_value(after_dash, item_span)?);
        }
    }

    Ok(YamlNode {
        span: start_span,
        value: YamlValue::Sequence(items),
    })
}

fn parse_block_mapping(lines: &[Line<'_>], pos: &mut usize, map_indent: usize) -> Result<YamlNode> {
    let start_span = SourceSpan {
        line: lines[*pos].line_no,
        col: map_indent + 1,
    };
    let mut map = IndexMap::new();

    while *pos < lines.len() {
        let line = &lines[*pos];
        if line.indent < map_indent {
            break;
        }
        if line.indent > map_indent {
            bail!(
                "Unexpected indentation at line {}: got {}, expected {}",
                line.line_no,
                line.indent,
                map_indent
            );
        }
        if line.content.starts_with("- ") || line.content == "-" {
            break;
        }
        let Some((raw_key, raw_val)) = split_key_value(line.content) else {
            bail!(
                "Expected `key: value` at line {}: `{}`",
                line.line_no,
                line.content
            );
        };
        let key = unquote_str(raw_key.trim())?;
        let val_str = raw_val.trim();
        let entry_span = SourceSpan {
            line: line.line_no,
            col: line.indent + 1,
        };
        *pos += 1;

        if val_str.is_empty() {
            if *pos < lines.len() && lines[*pos].indent > map_indent {
                let child_indent = lines[*pos].indent;
                let child = parse_block(lines, pos, child_indent)?;
                map.insert(key, child);
            } else {
                map.insert(
                    key,
                    YamlNode {
                        span: entry_span,
                        value: YamlValue::Null,
                    },
                );
            }
        } else {
            let val_col = line.indent + raw_key.len() + 2;
            let val_span = SourceSpan {
                line: line.line_no,
                col: val_col,
            };
            let node = parse_inline_value(val_str, val_span)?;
            map.insert(key, node);
        }
    }

    Ok(YamlNode {
        span: start_span,
        value: YamlValue::Mapping(map),
    })
}

/// Split `key: value` at the first top-level `:` that is followed by whitespace or end-of-string.
fn split_key_value(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
        } else if in_double {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
        } else {
            match b {
                b'\'' => in_single = true,
                b'"' => in_double = true,
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b':' if brace_depth == 0 && bracket_depth == 0 => {
                    if i + 1 == bytes.len() || bytes[i + 1].is_ascii_whitespace() {
                        return Some((&s[..i], &s[i + 1..]));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn parse_inline_value(s: &str, span: SourceSpan) -> Result<YamlNode> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "null" || trimmed == "~" {
        return Ok(YamlNode {
            span,
            value: YamlValue::Null,
        });
    }
    if trimmed == "true" {
        return Ok(YamlNode {
            span,
            value: YamlValue::Bool(true),
        });
    }
    if trimmed == "false" {
        return Ok(YamlNode {
            span,
            value: YamlValue::Bool(false),
        });
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1].trim();
        if inner.is_empty() {
            return Ok(YamlNode {
                span,
                value: YamlValue::Sequence(Vec::new()),
            });
        }
        let parts = split_top_level_comma(inner);
        let mut items = Vec::with_capacity(parts.len());
        for p in parts {
            items.push(parse_inline_value(p.trim(), span)?);
        }
        return Ok(YamlNode {
            span,
            value: YamlValue::Sequence(items),
        });
    }
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        let inner = &trimmed[1..trimmed.len() - 1].trim();
        if inner.is_empty() {
            return Ok(YamlNode {
                span,
                value: YamlValue::Mapping(IndexMap::new()),
            });
        }
        let parts = split_top_level_comma(inner);
        let mut map = IndexMap::with_capacity(parts.len());
        for p in parts {
            let p_trim = p.trim();
            if p_trim.is_empty() {
                continue;
            }
            let Some((k, v)) = split_key_value(p_trim) else {
                bail!("Invalid flow mapping entry `{p_trim}` at line {}", span.line);
            };
            let key = unquote_str(k.trim())?;
            let val = parse_inline_value(v.trim(), span)?;
            map.insert(key, val);
        }
        return Ok(YamlNode {
            span,
            value: YamlValue::Mapping(map),
        });
    }
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        return Ok(YamlNode {
            span,
            value: YamlValue::String(unquote_str(trimmed)?),
        });
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Ok(YamlNode {
            span,
            value: YamlValue::Int(i),
        });
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        return Ok(YamlNode {
            span,
            value: YamlValue::Float(f),
        });
    }
    Ok(YamlNode {
        span,
        value: YamlValue::String(trimmed.to_string()),
    })
}

fn split_top_level_comma(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut angle_depth = 0usize;
    let mut start = 0usize;
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
        } else if in_double {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
        } else {
            match b {
                b'\'' => in_single = true,
                b'"' => in_double = true,
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'<' => angle_depth += 1,
                b'>' => angle_depth = angle_depth.saturating_sub(1),
                b',' if brace_depth == 0 && bracket_depth == 0 && angle_depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start <= s.len() {
        out.push(&s[start..]);
    }
    out
}

pub fn unquote_str(s: &str) -> Result<String> {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        let inner = &s[1..s.len() - 1];
        return Ok(inner.replace("''", "'"));
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return Ok(out);
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spec_claims_yaml() {
        let yaml = r#"
schema: claimwire/v1

aliases:
  workload:
    "fib(19)": ["fibonacci, fib(19) = 4181"]
    "sha256":  ['sha256("abc")', 'sha256 of "abc"']

constants:
  paper.t22.proof_size_max: { type: Bytes, value: "599 KiB" }
  paper.t22.soundness_bits: { type: Quantity<bits>, value: "120.0 bits" }

families:
  guest_bench:
    producer: om.benchmarks
    dimensions: [workload, profile]
    metrics:
      cycles:      { type: Cycles,   format: "{:,}" }
      prove_wall:  { type: Duration, format: "{:.2f s}", deadband: 2.5% } # inline comment
"#;
        let node = parse_yaml(yaml).unwrap();
        let json = node.to_json_value();
        assert_eq!(json["schema"], "claimwire/v1");
        assert_eq!(
            json["aliases"]["workload"]["sha256"][0],
            "sha256(\"abc\")"
        );
        assert_eq!(
            json["constants"]["paper.t22.soundness_bits"]["type"],
            "Quantity<bits>"
        );
        assert_eq!(
            json["families"]["guest_bench"]["metrics"]["prove_wall"]["deadband"],
            "2.5%"
        );
    }
}
