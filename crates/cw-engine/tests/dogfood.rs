//! Regressions found by dogfooding `cw` on a C++/CUDA repository (manifest counts, benchmark
//! JSON, recorded regression runs). Every way a binding can disappear must fail `cw check`
//! instead of silently shrinking the verified set, and common Markdown shapes (header units,
//! `52-file`, derived claims in prose) must round-trip.

use cw_engine::{EvidenceLockfile, run_adopt, run_check, run_sync};
use cw_evidence::{EvidenceConfig, execute_capture};
use cw_ir::ClaimsConfig;
use cw_syntax::{DiagnosticCode, compute_text_context_digest, from_yaml_str};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(1);

const CLAIMS: &str = r#"schema: claimwire/v1

claims:
  matrix.total: { kind: source, producer: counts, metric: total, type: Count }
  matrix.typed: { kind: source, producer: counts, metric: typed, type: Count }

families:
  bench:
    producer: bench
    dimensions: [variant]
    metrics:
      time:   { type: Duration, format: "{:.1f [ms]}" }
      gflops: { type: Float,    format: "{:,.1f}" }
    derivations:
      share:
        type: Ratio
        expr: "gflops / bench[fast].gflops"
        format: "{:.0f%}"

assertions:
  check.typed_le_total:
    expr: "matrix.typed <= matrix.total"
  check.fast_is_faster:
    expr: "bench[fast].time < bench[slow].time"

blocks:
  status.snapshot:
    file: docs/status.md
    lines:
      - "- Reviewed {matrix.total}-file matrix: typed **{matrix.typed}/{matrix.total}**."
  bench.summary:
    file: docs/bench.md
    lines:
      - "Slow reaches {bench[slow].share} of fast."

tables:
  status.matrix:
    file: docs/status.md
    mode: checklist
    key_column: Frontend
    rows:
      "direct `.cu`":
        "Typed": "{matrix.typed}/{matrix.total}"
  bench.table:
    file: docs/bench.md
    mode: records
    family: bench
    key_columns: { Variant: variant }
    columns:
      "Time (ms)": "{time:.1f [ms]}"
      "GFLOP/s": "{gflops}"
      "Share": "{share}"
"#;

const EVIDENCE: &str = r#"schema: claimwire/v1

producers:
  counts:
    command: ["sh", "-c", "cat counts.txt"]
    families: []
    inputs:
      files: ["counts.txt"]
    epochs:
      current: pending
  bench:
    command: ["sh", "-c", "cat bench.txt"]
    families: [bench]
    inputs:
      files: ["bench.txt"]
    epochs:
      current: pending
"#;

const COUNTS: &str = "@cw:record total=52 typed=50\n";
const BENCH: &str = "@cw:record family=\"bench\" variant=\"fast\" time=\"10ms\" gflops=2000\n\
                     @cw:record family=\"bench\" variant=\"slow\" time=\"40ms\" gflops=500\n";

const STATUS_MD: &str = "# Status

<!-- cw:block status.snapshot -->
- Reviewed 52-file matrix: typed **50/52**.
- Unrelated bullet.

<!-- cw:table status.matrix -->
| Frontend | Typed |
| --- | ---: |
| direct `.cu` | **50/52** |
";

const BENCH_MD: &str = "# Bench

<!-- cw:block bench.summary -->
Slow reaches 25% of fast.

<!-- cw:table bench.table -->
| Variant | Time (ms) | GFLOP/s | Share | Notes |
| --- | ---: | ---: | ---: | --- |
| fast | 10.0 | 2,000.0 | 100% | tiled |
| slow | 40.0 | 500.0 | 25% | naive |
";

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("cw-dogfood-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claimwire/runs")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join(".claimwire/claims.yaml"), CLAIMS).unwrap();
        fs::write(root.join(".claimwire/evidence.yaml"), EVIDENCE).unwrap();
        fs::write(root.join("docs/status.md"), STATUS_MD).unwrap();
        fs::write(root.join("docs/bench.md"), BENCH_MD).unwrap();
        fs::write(root.join("counts.txt"), COUNTS).unwrap();
        fs::write(root.join("bench.txt"), BENCH).unwrap();
        let ws = Self { root };
        for producer in ["counts", "bench"] {
            let (_, _, reused) = ws.capture(producer);
            assert!(!reused);
        }
        let adopt = run_adopt(ws.path(), None).unwrap();
        assert!(adopt.diagnostics.is_empty(), "{:?}", adopt.diagnostics);
        ws.assert_clean();
        ws
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn capture(&self, producer: &str) -> (cw_evidence::RunManifest, String, bool) {
        let claims: ClaimsConfig =
            from_yaml_str(&fs::read_to_string(self.root.join(".claimwire/claims.yaml")).unwrap())
                .unwrap()
                .0;
        let evidence: EvidenceConfig =
            from_yaml_str(&fs::read_to_string(self.root.join(".claimwire/evidence.yaml")).unwrap())
                .unwrap()
                .0;
        execute_capture(&self.root, producer, &[], None, &evidence, &claims).unwrap()
    }

    fn edit(&self, rel: &str, from: &str, to: &str) {
        let path = self.root.join(rel);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(from).count(), 1, "`{from}` in {rel}");
        fs::write(&path, text.replacen(from, to, 1)).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
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

fn yaml_line_of(key_line: &str) -> usize {
    CLAIMS.lines().position(|l| l == key_line).expect(key_line) + 1
}

#[test]
fn common_markdown_shapes_adopt_cleanly() {
    // `{matrix.total}-file` and `{..}**.` (no CW1007), a `Time (ms)` header column with bare
    // numbers (hidden `[ms]` unit), and spec-less placeholders inheriting the claim's format
    // (`{bench[slow].share}` -> `25%`, `{gflops}` -> `2,000.0`).
    let ws = Workspace::new("shapes");
    assert_eq!(run_check(ws.path()).unwrap().total_locations, 1 + 1 + 1 + 6);
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
    single(
        &ws.codes(),
        DiagnosticCode::CW2003,
        "has no `<!-- cw:table status.matrix -->` directive",
    );
    let diag = run_check(ws.path()).unwrap().diagnostics.remove(0);
    assert_eq!(diag.file.as_deref(), Some(".claimwire/claims.yaml"));
    assert_eq!(
        diag.line,
        Some(yaml_line_of("  status.matrix:")),
        "the lens key, not 1:1"
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
    assert!(
        diag.notes.iter().any(|n| n.starts_with("mismatch:")),
        "{:?}",
        diag.notes
    );
}

#[test]
fn failing_assertion_holds_only_dependent_locations() {
    let ws = Workspace::new("held");
    // An unrelated count changes while the benchmark regresses.
    fs::write(
        ws.path().join("counts.txt"),
        "@cw:record total=53 typed=51\n",
    )
    .unwrap();
    fs::write(
        ws.path().join("bench.txt"),
        BENCH
            .replace("time=\"40ms\"", "time=\"5ms\"")
            .replace("gflops=500", "gflops=4000"),
    )
    .unwrap();
    ws.capture("counts");
    ws.capture("bench");

    let sync = run_sync(ws.path(), false, false, false).unwrap();
    assert!(
        sync.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::CW3002 && d.message.contains("check.fast_is_faster")),
        "{:?}",
        sync.diagnostics
    );
    // The count-fed locations synced ...
    assert!(
        ws.read("docs/status.md")
            .contains("Reviewed 53-file matrix: typed **51/53**.")
    );
    assert!(
        ws.read("docs/status.md")
            .contains("| direct `.cu` | **51/53** |")
    );
    // ... while the benchmark-fed ones were held back, untouched.
    assert_eq!(ws.read("docs/bench.md"), BENCH_MD);
    assert!(
        sync.held_locations
            .iter()
            .all(|id| id.starts_with("docs/bench.md::"))
    );
    assert!(!sync.held_locations.is_empty());

    // `check` keeps failing on the assertion and the held values, not on the synced ones.
    let codes = ws.codes();
    assert!(codes.iter().any(|(c, _)| *c == DiagnosticCode::CW3002));
    assert!(
        codes
            .iter()
            .filter(|(c, _)| *c == DiagnosticCode::CW3003)
            .count()
            >= 1
    );
    assert!(
        !codes.iter().any(|(c, _)| *c == DiagnosticCode::CW4001),
        "{codes:?}"
    );
}

#[test]
fn table_context_drift_is_reported_once_per_table() {
    let ws = Workspace::new("drift");
    ws.edit(
        "docs/bench.md",
        "| slow | 40.0 | 500.0 | 25% | naive |",
        "| slow | 40.0 | 500.0 | 25% | naive, unrolled |",
    );
    single(
        &ws.codes(),
        DiagnosticCode::CW4002,
        "3 bound cell(s) of table `bench.table`",
    );
    let sync = run_sync(ws.path(), false, false, true).unwrap();
    assert!(sync.diagnostics.is_empty(), "{:?}", sync.diagnostics);
    ws.assert_clean();
}

#[test]
fn legacy_table_context_digests_are_accepted() {
    let ws = Workspace::new("legacy");
    // Lockfiles written before row/header context existed hashed `row::column`.
    let path = ws.path().join(".claimwire/evidence.lock.json");
    let mut lock: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for (id, loc) in lock["locations"].as_object_mut().unwrap() {
        let mut parts = id.split("::");
        if parts.nth(1).is_some_and(|k| k.starts_with("table:")) {
            let (row, col) = (parts.next().unwrap(), parts.next().unwrap());
            loc["context_digest"] = compute_text_context_digest(&format!("{row}::{col}")).into();
        }
    }
    fs::write(&path, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
    ws.assert_clean();
}

#[test]
fn capture_reuses_identical_runs_and_stamps_real_time() {
    let ws = Workspace::new("capture");
    let (first, _, _) = ws.capture("counts");
    let (again, _, reused) = ws.capture("counts");
    assert!(reused);
    assert_eq!(first.run_id, again.run_id);
    let runs = fs::read_dir(ws.path().join(".claimwire/runs/counts"))
        .unwrap()
        .count();
    assert_eq!(runs, 1);
    assert_ne!(first.timestamp.as_deref(), Some("2026-10-16T18:00:00Z"));
    assert_eq!(first.command, vec!["sh", "-c", "cat counts.txt"]);

    fs::write(
        ws.path().join("counts.txt"),
        "@cw:record total=53 typed=50\n",
    )
    .unwrap();
    let (fresh, _, reused) = ws.capture("counts");
    assert!(!reused);
    assert_ne!(fresh.run_id, first.run_id);
}
