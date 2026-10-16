//! Fast Two-Tier Semantic Input Fingerprinting (`spec.md` §6.2).

use anyhow::Result;
use cw_syntax::{Blake3Hasher, blake3_prefixed};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FingerprintCache {
    pub entries: IndexMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub mtime_ns: u128,
    pub byte_len: u64,
    pub semantic_blake3: String,
}

impl FingerprintCache {
    pub fn load(workspace_root: &Path) -> (Self, Option<u128>) {
        let cache_path = workspace_root.join(".claimwire/.cache/fingerprints.bin");
        let Ok(meta) = fs::metadata(&cache_path) else {
            return (Self::default(), None);
        };
        let cache_mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        let Ok(bytes) = fs::read(&cache_path) else {
            return (Self::default(), None);
        };
        let cache: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        (cache, cache_mtime_ns)
    }

    pub fn save(&self, workspace_root: &Path) -> Result<()> {
        let cache_dir = workspace_root.join(".claimwire/.cache");
        fs::create_dir_all(&cache_dir)?;
        let cache_path = cache_dir.join("fingerprints.bin");
        let bytes = serde_json::to_vec(self)?;
        fs::write(cache_path, bytes)?;
        Ok(())
    }

    /// Compute or retrieve cached semantic fingerprint for `rel_path`.
    /// Enforces the racy-entry guard (§6.2 rule 1): an entry whose `mtime_ns` is not strictly
    /// older than `cache_mtime_ns` is treated as a miss.
    pub fn fingerprint_file(
        &mut self,
        workspace_root: &Path,
        rel_path: &str,
        cache_mtime_ns: Option<u128>,
    ) -> Result<(String, bool)> {
        let full_path = workspace_root.join(rel_path);
        let meta = fs::metadata(&full_path)?;
        let byte_len = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        if let (Some(c_mtime), Some(entry)) = (cache_mtime_ns, self.entries.get(rel_path)) {
            if entry.byte_len == byte_len && entry.mtime_ns == mtime_ns && mtime_ns < c_mtime {
                return Ok((entry.semantic_blake3.clone(), true));
            }
        }

        let raw_bytes = fs::read(&full_path)?;
        let digest = compute_semantic_blake3_for_path(rel_path, &raw_bytes);
        self.entries.insert(
            rel_path.to_string(),
            CacheEntry {
                mtime_ns,
                byte_len,
                semantic_blake3: digest.clone(),
            },
        );
        Ok((digest, false))
    }
}

pub fn compute_semantic_blake3_for_path(rel_path: &str, raw_bytes: &[u8]) -> String {
    let ext = Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let Ok(source) = std::str::from_utf8(raw_bytes) else {
        return blake3_prefixed(raw_bytes);
    };

    match ext {
        "rs" => blake3_prefixed(normalize_rust(source).as_bytes()),
        "c" | "h" | "cc" | "cpp" | "hpp" => blake3_prefixed(normalize_c_cpp(source).as_bytes()),
        "py" => blake3_prefixed(normalize_python(source).as_bytes()),
        "jl" => blake3_prefixed(normalize_julia(source).as_bytes()),
        _ => blake3_prefixed(raw_bytes),
    }
}

/// Rust (`.rs`) semantic token normalizer (`spec.md` §6.2):
/// - Strips line comments (`// ...`, including doc comments `///`, `//!`) and nested block comments (`/* /* ... */ */`).
/// - Preserves raw strings (`r#"..."#`, `br#"..."#`, `cr#"..."#`), regular strings (`"..."`), and char/byte literals (`'a'`, `'\n'`).
/// - Distinguishes lifetimes (`'a`, `'static`) from char literals (`'a'`).
/// - Canonicalizes whitespace outside literals so `cargo fmt` never invalidates benchmark runs.
pub fn normalize_rust(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Line comment `//`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            push_canonical_ws(&mut out);
            continue;
        }

        // Nested block comment `/* ... */`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            push_canonical_ws(&mut out);
            continue;
        }

        // Raw string literal: `r"..."`, `r#"..."#`, `br#"..."#`, `cr#"..."#`
        if let Some((prefix_len, hashes)) = detect_rust_raw_string(bytes, i) {
            let end_pos = i + prefix_len;
            out.extend_from_slice(&bytes[i..end_pos]);
            i = end_pos;
            // Scan until `"` followed by `hashes` of `#`
            while i < bytes.len() {
                out.push(bytes[i]);
                if bytes[i] == b'"'
                    && i + 1 + hashes <= bytes.len()
                    && bytes[i + 1..i + 1 + hashes].iter().all(|&c| c == b'#')
                {
                    out.extend_from_slice(&bytes[i + 1..i + 1 + hashes]);
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Normal string literal `"`
        if b == b'"' {
            out.push(b'"');
            i += 1;
            while i < bytes.len() {
                out.push(bytes[i]);
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push(bytes[i + 1]);
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Char literal vs lifetime (`'a'` vs `'a`)
        if b == b'\'' {
            if let Some(char_lit_len) = detect_rust_char_literal(bytes, i) {
                out.extend_from_slice(&bytes[i..i + char_lit_len]);
                i += char_lit_len;
                continue;
            } else {
                // Lifetime `'ident`
                out.push(b'\'');
                i += 1;
                continue;
            }
        }

        // Whitespace outside literals
        if b.is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            push_canonical_ws(&mut out);
            continue;
        }

        // If we are about to push a punctuation token after a canonical space, and no space is needed
        // between punctuation and alphanumeric/punctuation, strip trailing space unless needed to separate tokens
        if !is_ident_or_num_byte(b) && out.last() == Some(&b' ') {
            out.pop();
        }
        // Also if `out` ends with punctuation (not space and not ident/num), we don't need space before `b`
        out.push(b);
        i += 1;
    }

    if out.last() == Some(&b' ') {
        out.pop();
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_ident_or_num_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
}

fn push_canonical_ws(out: &mut Vec<u8>) {
    if let Some(&last) = out.last() {
        if is_ident_or_num_byte(last) {
            out.push(b' ');
        }
    }
}

fn detect_rust_raw_string(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let start = if bytes[i] == b'r' {
        i + 1
    } else if (bytes[i] == b'b' || bytes[i] == b'c') && i + 1 < bytes.len() && bytes[i + 1] == b'r'
    {
        i + 2
    } else {
        return None;
    };
    // Make sure this isn't the middle of an identifier like `for` or `bar`
    if i > 0 && is_ident_or_num_byte(bytes[i - 1]) {
        return None;
    }
    let mut k = start;
    while k < bytes.len() && bytes[k] == b'#' {
        k += 1;
    }
    if k < bytes.len() && bytes[k] == b'"' {
        let hashes = k - start;
        Some((k + 1 - i, hashes))
    } else {
        None
    }
}

fn detect_rust_char_literal(bytes: &[u8], i: usize) -> Option<usize> {
    // `'x'` or `'\...'`
    if i + 2 >= bytes.len() {
        return None;
    }
    if bytes[i + 1] == b'\\' {
        let mut k = i + 2;
        while k < bytes.len() && bytes[k] != b'\'' && bytes[k] != b'\n' && k - i <= 12 {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b'\'' {
            return Some(k + 1 - i);
        }
        None
    } else {
        // Single UTF-8 char followed by `'`
        let s = std::str::from_utf8(&bytes[i + 1..]).ok()?;
        let ch = s.chars().next()?;
        let next_idx = i + 1 + ch.len_utf8();
        if next_idx < bytes.len() && bytes[next_idx] == b'\'' {
            Some(next_idx + 1 - i)
        } else {
            None
        }
    }
}

/// C/C++ (`.c`, `.h`, `.cc`, `.cpp`, `.hpp`) normalizer (`spec.md` §6.2):
/// Newlines are significant on preprocessor lines and `\` line continuations are honored.
/// Runs of whitespace collapse to a single separator but are never deleted.
pub fn normalize_c_cpp(source: &str) -> String {
    // Honor `\` line continuations first
    let joined = source.replace("\\\r\n", "").replace("\\\n", "");
    let bytes = joined.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            if !out.ends_with(char::is_whitespace) && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        if b == b'"' || b == b'\'' {
            let quote = b;
            out.push(quote as char);
            i += 1;
            while i < bytes.len() {
                let ch = joined[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
                if ch == '\\' && i < bytes.len() {
                    let next_ch = joined[i..].chars().next().unwrap();
                    out.push(next_ch);
                    i += next_ch.len_utf8();
                    continue;
                }
                if ch as u8 == quote {
                    break;
                }
            }
            continue;
        }
        if b == b'\n' {
            out.push('\n');
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
                i += 1;
            }
            if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        let ch = joined[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out.trim().to_string()
}

/// Python (`.py`) normalizer (`spec.md` §6.2):
/// Indentation and logical-line boundaries are significant and kept verbatim.
/// Only `#` comments and blank lines are dropped. Docstrings are string literals and ARE hashed.
pub fn normalize_python(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut cleaned = String::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        // Triple-quoted strings `"""` or `'''`
        if i + 3 <= bytes.len() && (&bytes[i..i + 3] == b"\"\"\"" || &bytes[i..i + 3] == b"'''") {
            let delim = &source[i..i + 3];
            cleaned.push_str(delim);
            i += 3;
            while i < bytes.len() {
                if i + 3 <= bytes.len() && &source[i..i + 3] == delim {
                    cleaned.push_str(delim);
                    i += 3;
                    break;
                }
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    let c1 = source[i..].chars().next().unwrap();
                    cleaned.push(c1);
                    i += c1.len_utf8();
                    if i < bytes.len() {
                        let c2 = source[i..].chars().next().unwrap();
                        cleaned.push(c2);
                        i += c2.len_utf8();
                    }
                    continue;
                }
                let ch = source[i..].chars().next().unwrap();
                cleaned.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }
        // Single-line string `"` or `'`
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i] as char;
            cleaned.push(q);
            i += 1;
            while i < bytes.len() {
                let ch = source[i..].chars().next().unwrap();
                cleaned.push(ch);
                i += ch.len_utf8();
                if ch == '\\' && i < bytes.len() {
                    let next_ch = source[i..].chars().next().unwrap();
                    cleaned.push(next_ch);
                    i += next_ch.len_utf8();
                    continue;
                }
                if ch == q {
                    break;
                }
            }
            continue;
        }
        // `#` comment outside string
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let ch = source[i..].chars().next().unwrap();
        cleaned.push(ch);
        i += ch.len_utf8();
    }

    // Drop blank lines and trailing horizontal whitespace per line, preserving leading indentation
    let mut lines_out = Vec::new();
    for line in cleaned.lines() {
        let rtrimmed = line.trim_end_matches([' ', '\t', '\r']);
        if !rtrimmed.trim().is_empty() {
            lines_out.push(rtrimmed);
        }
    }
    lines_out.join("\n")
}

/// Julia (`.jl`) normalizer (`spec.md` §6.2):
/// Whitespace runs collapse but are never removed (`[1 -2]` and `[1 - 2]` differ).
/// Nested `#= ... =#` comments are handled.
pub fn normalize_julia(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        // Nested `#= ... =#` block comment
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
            let mut depth = 1usize;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'#' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if !out.ends_with(char::is_whitespace) && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        // Single-line `#` comment
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // String literal
        if bytes[i] == b'"' {
            out.push('"');
            i += 1;
            while i < bytes.len() {
                let ch = source[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
                if ch == '\\' && i < bytes.len() {
                    let next_ch = source[i..].chars().next().unwrap();
                    out.push(next_ch);
                    i += next_ch.len_utf8();
                    continue;
                }
                if ch == '"' {
                    break;
                }
            }
            continue;
        }
        if bytes[i] == b'\n' {
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
                i += 1;
            }
            if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out.trim().to_string()
}

/// Expand globs (supporting `{a,b,c}` brace expansion) relative to `workspace_root`
/// and return sorted relative paths (using forward slashes `/`).
pub fn expand_input_globs(workspace_root: &Path, patterns: &[String]) -> Result<Vec<String>> {
    let mut expanded_patterns = Vec::new();
    for pat in patterns {
        expand_braces(pat, &mut expanded_patterns);
    }

    let mut rel_paths = std::collections::BTreeSet::new();
    for pat in expanded_patterns {
        let full_pat = workspace_root.join(&pat);
        let Some(pat_str) = full_pat.to_str() else {
            continue;
        };
        if let Ok(paths) = glob::glob(pat_str) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    if let Ok(rel) = entry.strip_prefix(workspace_root) {
                        let rel_slash = rel
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/");
                        rel_paths.insert(rel_slash);
                    }
                }
            }
        }
    }
    Ok(rel_paths.into_iter().collect())
}

fn expand_braces(pattern: &str, out: &mut Vec<String>) {
    if let (Some(open), Some(close)) = (pattern.find('{'), pattern.find('}')) {
        if open < close {
            let prefix = &pattern[..open];
            let suffix = &pattern[close + 1..];
            for alt in pattern[open + 1..close].split(',') {
                let combined = format!("{prefix}{}{suffix}", alt.trim());
                expand_braces(&combined, out);
            }
            return;
        }
    }
    out.push(pattern.to_string());
}

/// Compute `input_set_digest` from sorted `(rel_path, semantic_blake3)` pairs,
/// declared `env` values, and the producer's definition (§6.5).
pub fn compute_input_set_digest(
    file_digests: &IndexMap<String, String>,
    env_vars: &IndexMap<String, String>,
    producer_command: &[String],
    producer_families: &[String],
    producer_input_globs: &[String],
) -> String {
    let mut hasher = Blake3Hasher::new();
    hasher.update(b"claimwire-input-set-v1\n");
    for cmd in producer_command {
        hasher.update(b"cmd:");
        hasher.update(cmd.as_bytes());
        hasher.update(b"\n");
    }
    for fam in producer_families {
        hasher.update(b"fam:");
        hasher.update(fam.as_bytes());
        hasher.update(b"\n");
    }
    for g in producer_input_globs {
        hasher.update(b"glob:");
        hasher.update(g.as_bytes());
        hasher.update(b"\n");
    }
    let mut sorted_files: Vec<(&String, &String)> = file_digests.iter().collect();
    sorted_files.sort_by_key(|(k, _)| *k);
    for (path, digest) in sorted_files {
        hasher.update(b"file:");
        hasher.update(path.as_bytes());
        hasher.update(b"=");
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    let mut sorted_env: Vec<(&String, &String)> = env_vars.iter().collect();
    sorted_env.sort_by_key(|(k, _)| *k);
    for (k, v) in sorted_env {
        hasher.update(b"env:");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize_prefixed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_semantic_normalization_immunity() {
        let code_v1 = r#"
/// Doc comment about Spartan
pub fn prove<'a>(x: &'a [u64]) -> u64 {
    // Some comment
    /* Nested /* block */ comment */
    let s = "hello /* not a comment */ // world";
    let c = 'x';
    x.len() as u64 + 1
}
"#;
        let code_v2 = r#"
/// Edited typo in doc comment!
pub fn prove<'a>(
    x: &'a [u64]
) -> u64 {
    let s = "hello /* not a comment */ // world";
    let c = 'x';
    x.len() as u64 + 1
}
"#;
        let code_changed = r#"
pub fn prove<'a>(x: &'a [u64]) -> u64 {
    let s = "hello /* not a comment */ // world";
    let c = 'x';
    x.len() as u64 + 2
}
"#;
        assert_eq!(
            compute_semantic_blake3_for_path("src/lib.rs", code_v1.as_bytes()),
            compute_semantic_blake3_for_path("src/lib.rs", code_v2.as_bytes())
        );
        assert_ne!(
            compute_semantic_blake3_for_path("src/lib.rs", code_v1.as_bytes()),
            compute_semantic_blake3_for_path("src/lib.rs", code_changed.as_bytes())
        );
    }

    #[test]
    fn test_julia_whitespace_sensitivity() {
        // `[1 -2]` and `[1 - 2]` must differ in Julia
        let j1 = "[1 -2] # comment 1";
        let j2 = "[1  -2] #= nested #= block =# =#";
        let j3 = "[1 - 2]";
        assert_eq!(normalize_julia(j1), normalize_julia(j2));
        assert_ne!(normalize_julia(j1), normalize_julia(j3));
    }
}
