//! Coverage regressions found by dogfooding on a C++/CUDA repository: every way a binding can
//! disappear must fail `cw check` instead of silently shrinking the verified set.

use cw_engine::{EvidenceLockfile, run_adopt, run_check, run_sync};
use cw_evidence::{EvidenceConfig, execute_capture};
use cw_ir::ClaimsConfig;
use cw_syntax::{DiagnosticCode, from_yaml_str};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(1);

const CLAIMS: &str = r#"schema: claimwire/v1

claims:
  matrix.total: { kind: source, producer: counts, metric: total, type: Count }
  matrix.typed: { kind: source, producer: counts, metric: typed, type: Count }

assertions:
  check.typed_le_total:
    expr: "matrix.typed <= matrix.total"

blocks:
  status.snapshot:
    file: docs/status.md
    lines:
      - "- Reviewed {matrix.total}-file matrix: typed **{matrix.typed}/{matrix.total}**."

tables:
  status.matrix:
    file: docs/status.md
    mode: checklist
    key_column: Frontend
    rows:
      "direct `.cu`":
        "Typed": "{matrix.typed}/{matrix.total}"
"#;

const EVIDENCE: &str = r#"schema: claimwire/v1

producers:
  counts:
    command: ["sh", "-c", "echo '@cw:record total=52 typed=50'"]
    families: []
    inputs:
      files: ["manifest.txt"]
    epochs:
      current: pending
"#;

const STATUS_MD: &str = "# Status

<!-- cw:block status.snapshot -->
- Reviewed 52-file matrix: typed **50/52**.
- Unrelated bullet.

<!-- cw:table status.matrix -->
| Frontend | Typed |
| --- | ---: |
| direct `.cu` | **50/52** |
";

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("cw-coverage-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claimwire/runs")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join(".claimwire/claims.yaml"), CLAIMS).unwrap();
        fs::write(root.join(".claimwire/evidence.yaml"), EVIDENCE).unwrap();
        fs::write(root.join("docs/status.md"), STATUS_MD).unwrap();
        fs::write(root.join("manifest.txt"), "a\nb\n").unwrap();
        let ws = Self { root };
        let (_, _, reused) = ws.capture();
        assert!(!reused);
        let adopt = run_adopt(ws.path(), None).unwrap();
        assert!(adopt.diagnostics.is_empty(), "{:?}", adopt.diagnostics);
        ws.assert_clean();
        ws
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn capture(&self) -> (cw_evidence::RunManifest, String, bool) {
        let claims: ClaimsConfig =
            from_yaml_str(&fs::read_to_string(self.root.join(".claimwire/claims.yaml")).unwrap())
                .unwrap()
                .0;
        let evidence: EvidenceConfig =
            from_yaml_str(&fs::read_to_string(self.root.join(".claimwire/evidence.yaml")).unwrap())
                .unwrap()
                .0;
        execute_capture(&self.root, "counts", &[], None, &evidence, &claims).unwrap()
    }

    fn edit(&self, rel: &str, from: &str, to: &str) {
        let path = self.root.join(rel);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(from).count(), 1, "`{from}` in {rel}");
        fs::write(&path, text.replacen(from, to, 1)).unwrap();
    }

    fn assert_clean(&self) {
        let report = run_check(self.path()).unwrap();
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    }

    fn codes(&self) -> Vec<(DiagnosticCode, String)> {
        run_check(self.path())
            .unwrap()
            .diagnostics
            .into_iter()
            .map(|d| (d.code, d.message))
            .collect()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn single(codes: &[(DiagnosticCode, String)], code: DiagnosticCode, needle: &str) {
    assert_eq!(codes.len(), 1, "{codes:?}");
    assert_eq!(codes[0].0, code, "{codes:?}");
    assert!(codes[0].1.contains(needle), "{codes:?}");
}

#[test]
fn integer_placeholders_accept_trailing_punctuation() {
    // `{matrix.total}-file` and `{matrix.total}**.` adopt and verify (no CW1007).
    let ws = Workspace::new("punct");
    assert_eq!(run_check(ws.path()).unwrap().total_locations, 2);
}

#[test]
fn renamed_checklist_row_is_an_error() {
    let ws = Workspace::new("row");
    ws.edit(
        "docs/status.md",
        "| direct `.cu` |",
        "| direct CUDA `.cu` |",
    );
    single(&ws.codes(), DiagnosticCode::CW2003, "Row `direct `.cu``");
}

#[test]
fn renamed_column_is_an_error() {
    let ws = Workspace::new("col");
    ws.edit(
        "docs/status.md",
        "| Frontend | Typed |",
        "| Frontend | Typed IR |",
    );
    single(&ws.codes(), DiagnosticCode::CW2003, "Column `Typed`");
}

#[test]
fn deleted_lens_directive_is_an_error() {
    let ws = Workspace::new("directive");
    ws.edit("docs/status.md", "<!-- cw:table status.matrix -->\n", "");
    let codes = ws.codes();
    single(
        &codes,
        DiagnosticCode::CW2003,
        "has no `<!-- cw:table status.matrix -->` directive",
    );
    let diag = run_check(ws.path()).unwrap().diagnostics.remove(0);
    assert_eq!(diag.file.as_deref(), Some(".claimwire/claims.yaml"));
    assert_eq!(
        diag.line,
        Some(18),
        "points at the `status.matrix:` key, not 1:1"
    );
}

#[test]
fn directive_in_wrong_file_is_one_error() {
    let ws = Workspace::new("moved");
    ws.edit("docs/status.md", "<!-- cw:table status.matrix -->\n", "");
    fs::write(
        ws.path().join("docs/other.md"),
        "<!-- cw:table status.matrix -->\n| Frontend | Typed |\n| --- | --- |\n| direct `.cu` | 50/52 |\n",
    )
    .unwrap();
    single(
        &ws.codes(),
        DiagnosticCode::CW2003,
        "but its directive appears in `docs/other.md`",
    );
}

#[test]
fn deleted_span_is_cw4003_until_sync_prunes_it() {
    let ws = Workspace::new("span");
    // Spans are discovered outside `docs/` too.
    fs::create_dir_all(ws.path().join("demos/x")).unwrap();
    fs::write(
        ws.path().join("demos/x/README.md"),
        "Matrix of <!-- cw:matrix.total -->52<!-- /cw --> files.\n",
    )
    .unwrap();
    let sync = run_sync(ws.path(), false, false, false).unwrap();
    assert!(sync.diagnostics.is_empty(), "{:?}", sync.diagnostics);
    let lock = EvidenceLockfile::load(ws.path()).unwrap();
    assert!(
        lock.locations
            .contains_key("demos/x/README.md::span:matrix.total#0")
    );

    ws.edit(
        "demos/x/README.md",
        "<!-- cw:matrix.total -->52<!-- /cw -->",
        "52",
    );
    single(
        &ws.codes(),
        DiagnosticCode::CW4003,
        "demos/x/README.md::span:matrix.total#0",
    );

    let sync = run_sync(ws.path(), false, false, false).unwrap();
    assert_eq!(
        sync.pruned_locations,
        vec!["demos/x/README.md::span:matrix.total#0"]
    );
    ws.assert_clean();
}

#[test]
fn spans_outside_docs_are_checked() {
    let ws = Workspace::new("outside");
    fs::write(
        ws.path().join("NOTES.md"),
        "Typed: <!-- cw:matrix.typed -->49<!-- /cw -->.\n",
    )
    .unwrap();
    single(&ws.codes(), DiagnosticCode::CW3003, "matrix.typed");
}

#[test]
fn rewrapped_block_line_points_at_closest_line() {
    let ws = Workspace::new("rewrap");
    ws.edit("docs/status.md", "typed **50/52**.", "typed\n  **50/52**.");
    let diag = run_check(ws.path())
        .unwrap()
        .diagnostics
        .into_iter()
        .find(|d| d.code == DiagnosticCode::CW1006)
        .expect("CW1006");
    assert_eq!(diag.line, Some(4));
    assert!(
        diag.snippet_line
            .unwrap()
            .starts_with("- Reviewed 52-file matrix: typed")
    );
}

#[test]
fn capture_reuses_identical_runs_and_stamps_real_time() {
    let ws = Workspace::new("capture");
    let (first, _, _) = ws.capture();
    let (again, _, reused) = ws.capture();
    assert!(reused);
    assert_eq!(first.run_id, again.run_id);
    let runs = fs::read_dir(ws.path().join(".claimwire/runs/counts"))
        .unwrap()
        .count();
    assert_eq!(runs, 1);
    assert_ne!(first.timestamp.as_deref(), Some("2026-10-16T18:00:00Z"));
    assert_eq!(
        first.command,
        vec!["sh", "-c", "echo '@cw:record total=52 typed=50'"]
    );

    fs::write(ws.path().join("manifest.txt"), "a\nb\nc\n").unwrap();
    let (fresh, _, reused) = ws.capture();
    assert!(!reused);
    assert_ne!(fresh.run_id, first.run_id);
}
