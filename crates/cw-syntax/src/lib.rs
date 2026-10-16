//! `cw-syntax`: Lossless Markdown CST, GFM-conformant table lexer, block lens & bidirectional format parser.

pub mod addressing;
pub mod blake3;
pub mod cst;
pub mod diagnostics;
pub mod format;
pub mod types;
pub mod yaml;

pub use addressing::{
    ClaimRef, FamilyRef, OuterDecorator, ScalarRef, Selector, SelectorKey, canonicalize_table_key,
    format_scope_bracketed, parse_row_override_key,
};
pub use blake3::{Hasher as Blake3Hasher, blake3_bytes, blake3_hex, blake3_prefixed};
pub use cst::{
    BlockLensNode, ByteRange, GfmCell, GfmRow, InlineSpanNode, LineSlice, MarkdownDocument,
    TableLensNode, apply_byte_patches, compute_line_context_digest, compute_text_context_digest,
};
pub use diagnostics::{Diagnostic, DiagnosticCode, Severity, format_diagnostics_sarif};
pub use format::{
    FormatPattern, FormatSpec, NamedFormat, NumericKind, PatternSegment, PlaceholderSpec,
    format_auto_iec, format_auto_time, format_number_half_to_even, round_half_to_even_f64,
    round_to_decimals, validate_spec_for_type,
};
pub use types::{CanonicalValue, ValueType, split_numeric_and_unit};
pub use yaml::{SourceSpan, YamlNode, YamlValue, from_yaml_str, parse_yaml, unquote_str};
