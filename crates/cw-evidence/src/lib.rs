//! `cw-evidence`: Two-tier semantic fingerprinter, epoch manifest loader, and `@cw:` wire protocol capture.

pub mod fingerprint;
pub mod manifest;

pub use fingerprint::{
    CacheEntry, FingerprintCache, compute_input_set_digest, compute_semantic_blake3_for_path,
    expand_input_globs, normalize_c_cpp, normalize_julia, normalize_python, normalize_rust,
};
pub use manifest::{
    EvidenceConfig, EvidenceRecord, LoadedEvidenceStore, ProducerDecl, ProducerInputs, RunManifest,
    execute_capture, parse_and_validate_cw_frames, short_digest,
};
