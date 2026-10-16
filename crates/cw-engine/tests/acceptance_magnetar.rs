use cw_engine::{
    DiffCategory, EvidenceLockfile, RevisionSnapshot, WriteAheadJournal, compare_snapshots,
    recover_staging_journal, run_adopt, run_check, run_check_files, run_explain, run_impact,
    run_merge_driver, run_sync,
};
use cw_evidence::{EvidenceConfig, RunManifest, execute_capture};
use cw_ir::ClaimsConfig;
use cw_syntax::{DiagnosticCode, from_yaml_str};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempFixture {
    root: PathBuf,
}

impl TempFixture {
    fn new(name: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("cw-acceptance-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/fixtures/magnetar");
        copy_dir_recursive(&src, &root);
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

fn update_current_bench_manifest<F: FnOnce(&mut RunManifest)>(ws: &Path, f: F) {
    let manifest_path = ws.join(".claimwire/runs/om.benchmarks/bench-20261016-m4pro.json");
    let content = fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: RunManifest = serde_json::from_str(&content).unwrap();
    manifest.normalize();
    f(&mut manifest);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
}

/// Acceptance Matrix Point 1 (`spec.md` §10.1 & §10.2 #1):
/// Zero-Diff Adoption & Live Drift Detection (`cw adopt` + `cw check`), plus `CW1008` on `04-pcs.md:245`.
#[test]
fn point_01_zero_diff_adoption_and_live_drift_detection() {
    let fix = TempFixture::new("point01");
    let ws = fix.path();

    let md_files = [
        "README.md",
        "docs/BENCHMARKS.md",
        "docs/VALIDATION.md",
        "docs/spec/13-security.md",
        "docs/spec/15-testing.md",
    ];
    let before_bytes: Vec<Vec<u8>> = md_files
        .iter()
        .map(|f| fs::read(ws.join(f)).unwrap())
        .collect();

    // 1. Run `cw adopt` -> 0 Markdown bytes modified, 340 locations adopted
    let adopt_res = run_adopt(ws, None).unwrap();
    assert!(
        adopt_res.diagnostics.is_empty(),
        "unexpected adopt diagnostics: {:?}",
        adopt_res.diagnostics
    );
    assert_eq!(adopt_res.adopted_locations, 340);

    for (f, before) in md_files.iter().zip(before_bytes.iter()) {
        let after = fs::read(ws.join(f)).unwrap();
        assert_eq!(&after, before, "cw adopt must not modify {f}");
    }

    // 2. Run `cw check` -> catches the live `192 vs 227` drift at `docs/spec/15-testing.md:19:39`
    let check_res = run_check(ws).unwrap();
    assert_eq!(check_res.total_locations, 340);
    assert_eq!(check_res.failed_locations, 1);
    assert_eq!(check_res.diagnostics.len(), 1);
    let diag = &check_res.diagnostics[0];
    assert_eq!(diag.code, DiagnosticCode::CW3003);
    assert_eq!(diag.file.as_deref(), Some("docs/spec/15-testing.md"));
    assert_eq!(diag.line, Some(19));
    assert_eq!(diag.col, Some(39));
    let human = diag.format_human();
    assert!(human.contains("evidence says 227"), "got:\n{human}");
    assert!(
        human.contains("README.md:18           (block readme.test_suite_comment) ✓"),
        "got:\n{human}"
    );
    assert!(
        human.contains("docs/BENCHMARKS.md:185 (table bench.test_suites)         ✓"),
        "got:\n{human}"
    );

    // 3. Verify `CW1008` on `docs/spec/04-pcs.md:245` (`2k/|K|` unescaped pipe inside backticks)
    let pcs_check = run_check_files(ws, Some(&["docs/spec/04-pcs.md".to_string()])).unwrap();
    assert!(
        pcs_check
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::CW1008 && d.line == Some(245)),
        "expected CW1008 on docs/spec/04-pcs.md:245, got: {:?}",
        pcs_check.diagnostics
    );

    // 4. Run `cw sync` -> repairs only `docs/spec/15-testing.md:19` (`192 -> 227`), 0 diffs on the other 4 files
    let sync_res = run_sync(ws, false, false, false).unwrap();
    assert!(sync_res.diagnostics.is_empty());
    assert_eq!(sync_res.files_written, 1);
    assert_eq!(sync_res.patched_items.len(), 1);
    assert_eq!(sync_res.patched_items[0].file, "docs/spec/15-testing.md");
    assert_eq!(sync_res.patched_items[0].line, 19);
    assert_eq!(sync_res.patched_items[0].old_text, "192");
    assert_eq!(sync_res.patched_items[0].new_text, "227");

    for (f, before) in md_files[..4].iter().zip(before_bytes[..4].iter()) {
        let after = fs::read(ws.join(f)).unwrap();
        assert_eq!(&after, before, "cw sync must not touch already-synced {f}");
    }

    // 5. Subsequent `cw check` passes with 0 errors across all 340 locations
    let check_after = run_check(ws).unwrap();
    assert_eq!(check_after.total_locations, 340);
    assert_eq!(check_after.failed_locations, 0);
    assert!(check_after.diagnostics.is_empty());
}

/// Acceptance Matrix Point 2 (`spec.md` §10.2 #2):
/// Multi-Table, Transposed & Multi-Doc Fan-Out (`cw sync`).
#[test]
fn point_02_multi_table_transposed_and_multi_doc_fan_out() {
    let fix = TempFixture::new("point02");
    let ws = fix.path();

    // Sync initial 192 -> 227 drift first
    run_sync(ws, false, false, false).unwrap();

    // Update `guest_bench[radix-sort-u64, test].prove_wall` from `8.5487 s` to `7.90 s` (`7_900_000_000 ns`)
    update_current_bench_manifest(ws, |m| {
        let records = m.families.get_mut("guest_bench").unwrap();
        for rec in records {
            if rec.scope.get("workload").map(|s| s.as_str()) == Some("radix-sort-u64")
                && rec.scope.get("profile").map(|s| s.as_str()) == Some("test")
            {
                rec.metrics.insert(
                    "prove_wall".to_string(),
                    cw_syntax::CanonicalValue::Number(7_900_000_000.0),
                );
            }
        }
    });

    let sync_res = run_sync(ws, false, false, false).unwrap();
    assert!(
        sync_res.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        sync_res.diagnostics
    );
    assert_eq!(sync_res.files_written, 2); // README.md and docs/BENCHMARKS.md

    let readme = fs::read_to_string(ws.join("README.md")).unwrap();
    // Records table cell preserves `**...**` decorator and updates to `**7.90 s**`
    assert!(
        readme.contains("| `radix-sort-u64` (`A = 17`) | `2^22` | test | **7.90 s** | 8.3 GiB | 10 ms (27 MiB) | 60,806 B |"),
        "README table not updated properly:\n{readme}"
    );
    // Prose throughput updated from `~491k cycles/s` to `~531k cycles/s`
    assert!(
        readme.contains(
            "<!-- cw:guest_bench[radix-sort-u64, test].throughput -->~531k cycles/s<!-- /cw -->"
        ),
        "README throughput span not updated:\n{readme}"
    );

    let bench = fs::read_to_string(ws.join("docs/BENCHMARKS.md")).unwrap();
    // Prose spans in BENCHMARKS.md:75-76
    assert!(
        bench
            .contains("<!-- cw:guest_bench[radix-sort-u64, test].prove_wall -->7.90 s<!-- /cw -->")
    );
    assert!(bench.contains(
        "<!-- cw:guest_bench[radix-sort-u64, test].speedup_vs_initial -->13.5×<!-- /cw -->"
    ));
    // Transposed deep-dive table row
    assert!(bench.contains("| Prove (wall) | **8.82 s** (≈ 476 k cycles/s; 66.8 s CPU) | **7.90 s** (≈ 531 k cycles/s; 66.3 s CPU) |"));
    // Paper comparison checklist table row
    assert!(bench.contains("**~531k cycles/s** (7.90 s wall, 66.3 s CPU)"));
    assert!(bench.contains(
        "**~15× behind** (improved **13.5×** from the initial 39k cycles/s / 107 s baseline)"
    ));
}

/// Acceptance Matrix Point 3 (`spec.md` §10.2 #3):
/// Ordered Prefix-Sum & Subset-Sum Cascade.
#[test]
fn point_03_ordered_prefix_sum_and_subset_sum_cascade() {
    let fix = TempFixture::new("point03");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    // Update `phase_bench[virtual_spartan].duration` from `1.54 s` to `1.20 s` (`1_200_000_000 ns`)
    update_current_bench_manifest(ws, |m| {
        let records = m.families.get_mut("phase_bench").unwrap();
        for rec in records {
            if rec.scope.get("phase").map(|s| s.as_str()) == Some("virtual_spartan") {
                rec.metrics.insert(
                    "duration".to_string(),
                    cw_syntax::CanonicalValue::Number(1_200_000_000.0),
                );
            }
        }
    });

    let sync_res = run_sync(ws, false, false, false).unwrap();
    assert!(sync_res.diagnostics.is_empty());
    assert_eq!(sync_res.files_written, 1);

    let bench = fs::read_to_string(ws.join("docs/BENCHMARKS.md")).unwrap();
    // All 7 downstream `Ends at` cells in `bench.phase_trace` cascaded by -0.34 s:
    assert!(bench.contains("| Virtual Spartan | 3.69 s | **1.20 s** | 3.2 GiB |"));
    assert!(bench.contains("| register Twist | 4.48 s | **0.79 s** | 2.6 GiB |"));
    assert!(bench.contains("| RAM Twist (A = 17) | 5.87 s | **1.39 s** | 2.4 GiB |"));
    assert!(bench.contains("| routers | 6.29 s | 0.42 s | 1.7 GiB |"));
    assert!(bench.contains("| bytecode Shout | 6.49 s | **0.20 s** | 1.7 GiB |"));
    assert!(bench.contains("| Mosaic batch | 6.76 s | **0.27 s** | 1.8 GiB |"));
    assert!(bench.contains("| ring switching + Ligerito open | 8.42 s | **1.66 s** | 5.8 GiB |"));
    // Total wall time in heading updated from `8.76 s` to `8.42 s`
    assert!(bench.contains("<!-- cw:phase_bench.total_wall -->8.42 s<!-- /cw -->"));
    // Phase breakdown table updated Virtual Spartan duration (`1.20 s`), share (`14%`), and speedup (`7.78×`),
    // and Twist & Shout share (`2.38 / 8.42 = 28%`) and Ligerito Open share (`1.66 / 8.42 = 20%`)
    assert!(
        bench.contains(
            "| **Twist & Shout (`om-memcheck`)** | **2.38 s** | 28% | 3.68 s | **1.55×** |"
        )
    );
    assert!(bench.contains(
        "| **Ligerito Open + Ring Switching (`om-pcs`)** | **1.66 s** | 20% | 2.28 s | **1.37×** |"
    ));
    assert!(
        bench.contains("| **Execute + Witness + Routers** | **1.69 s** | 20% | 1.63 s | ~1.0× |")
    );
    assert!(bench.contains(
        "| **Virtual Spartan (`om-spartan`)** | **1.20 s** | 14% | 9.33 s | **7.78×** |"
    ));
}

/// Acceptance Matrix Point 4 (`spec.md` §10.2 #4):
/// Idempotence (`0` files written on second `cw sync`).
#[test]
fn point_04_idempotence() {
    let fix = TempFixture::new("point04");
    let ws = fix.path();

    let first = run_sync(ws, false, false, false).unwrap();
    assert_eq!(first.files_written, 1);

    let snapshot_readme = fs::read(ws.join("README.md")).unwrap();
    let snapshot_bench = fs::read(ws.join("docs/BENCHMARKS.md")).unwrap();
    let snapshot_testing = fs::read(ws.join("docs/spec/15-testing.md")).unwrap();

    let second = run_sync(ws, false, false, false).unwrap();
    assert!(second.diagnostics.is_empty());
    assert_eq!(second.files_written, 0);
    assert!(second.patched_items.is_empty());

    assert_eq!(fs::read(ws.join("README.md")).unwrap(), snapshot_readme);
    assert_eq!(
        fs::read(ws.join("docs/BENCHMARKS.md")).unwrap(),
        snapshot_bench
    );
    assert_eq!(
        fs::read(ws.join("docs/spec/15-testing.md")).unwrap(),
        snapshot_testing
    );
}

/// Acceptance Matrix Point 5 (`spec.md` §10.2 #5):
/// Semantic Fingerprint Immunity (adding comments/whitespace to Rust source does not invalidate evidence).
#[test]
fn point_05_semantic_fingerprint_immunity() {
    let fix = TempFixture::new("point05");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    let spartan_path = ws.join("crates/om-spartan/src/lib.rs");
    let orig = fs::read_to_string(&spartan_path).unwrap();
    let commented = format!(
        "// Added line comment that must be stripped by Rust lexical normalizer\n\n/* Block comment\n   spanning multiple lines */\n{orig}\n   \n// Trailing comment\n"
    );
    fs::write(&spartan_path, commented).unwrap();

    let check_res = run_check(ws).unwrap();
    assert!(
        check_res.diagnostics.is_empty(),
        "comments/whitespace must not trigger CW3001: {:?}",
        check_res.diagnostics
    );
    assert_eq!(check_res.failed_locations, 0);
}

/// Acceptance Matrix Point 6 (`spec.md` §10.2 #6):
/// True Code Invalidation (`CW3001` & `cw impact`).
#[test]
fn point_06_true_code_invalidation_and_impact() {
    let fix = TempFixture::new("point06");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    let spartan_path = ws.join("crates/om-spartan/src/lib.rs");
    let orig = fs::read_to_string(&spartan_path).unwrap();
    fs::write(
        &spartan_path,
        format!("{orig}\npub fn new_round_folding_kernel() -> usize {{ 42 }}\n"),
    )
    .unwrap();

    // `cw check` emits `CW3001 (Stale Evidence)` on `om.benchmarks@current`
    let check_res = run_check(ws).unwrap();
    assert!(
        check_res.diagnostics.iter().any(|d| {
            d.code == DiagnosticCode::CW3001 && d.message.contains("om.benchmarks@current")
        }),
        "expected CW3001 on om.benchmarks, got: {:?}",
        check_res.diagnostics
    );

    // `cw impact` traces `crates/om-spartan/src/lib.rs` to `om.benchmarks`, `guest_bench`, `phase_bench`,
    // and every bound table and span across `README.md` and `docs/BENCHMARKS.md`.
    let impact = run_impact(ws, &["crates/om-spartan/src/lib.rs".to_string()]).unwrap();
    assert!(
        impact
            .stale_producers
            .contains(&"om.benchmarks".to_string())
    );
    assert!(
        impact
            .affected_families
            .contains(&"guest_bench".to_string())
    );
    assert!(
        impact
            .affected_families
            .contains(&"phase_bench".to_string())
    );
    assert!(
        impact
            .affected_locations
            .iter()
            .any(|l| l.file == "README.md" && l.line == 53)
    );
    assert!(
        impact
            .affected_locations
            .iter()
            .any(|l| l.file == "docs/BENCHMARKS.md")
    );
}

/// Acceptance Matrix Point 7 (`spec.md` §10.2 #7):
/// Deadband Hysteresis & Sentinel Preservation.
#[test]
fn point_07_deadband_hysteresis_and_sentinel_preservation() {
    let fix = TempFixture::new("point07");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    // 1. Record `8.60 s` (+0.6% vs locked `8.5487 s`, within `deadband: 2.5%`):
    //    `cw check` and `cw sync` pass with 0 churn!
    update_current_bench_manifest(ws, |m| {
        let records = m.families.get_mut("guest_bench").unwrap();
        for rec in records {
            if rec.scope.get("workload").map(|s| s.as_str()) == Some("radix-sort-u64")
                && rec.scope.get("profile").map(|s| s.as_str()) == Some("test")
            {
                rec.metrics.insert(
                    "prove_wall".to_string(),
                    cw_syntax::CanonicalValue::Number(8_600_000_000.0),
                );
            }
        }
    });

    let check_within_db = run_check(ws).unwrap();
    assert!(
        check_within_db.diagnostics.is_empty(),
        "8.60 s within 2.5% deadband must pass cw check: {:?}",
        check_within_db.diagnostics
    );

    let sync_within_db = run_sync(ws, false, false, false).unwrap();
    assert_eq!(sync_within_db.files_written, 0);
    let readme = fs::read_to_string(ws.join("README.md")).unwrap();
    assert!(readme.contains("**8.55 s**"));
    assert!(readme.contains("~491k cycles/s"));

    // Verify sentinels (`exact` on rows 5 and 19 of `13-security.md`, `under 5 min (not timed)` in `BENCHMARKS.md`)
    let sec = fs::read_to_string(ws.join("docs/spec/13-security.md")).unwrap();
    assert!(
        sec.contains("| 5 | Segment split (identity 6.2) | 06 6.5 | 0 (exact) | exact | exact |")
    );
    assert!(sec.contains(
        "| 19 | Twist RAM: Inc at random point (inc_extra) | 07 7.7 | 0 (exact) | exact | exact |"
    ));
    let bench = fs::read_to_string(ws.join("docs/BENCHMARKS.md")).unwrap();
    assert!(bench.contains(
        "| sha256-merkle-path (depth 2) | 32,768 | under 5 min (not timed) | 40,382 B |"
    ));

    // 2. Changing deterministic `proof_size` by 1 byte (`603,942 B -> 603,943 B`, `deadband: 0%`)
    //    triggers an immediate update!
    update_current_bench_manifest(ws, |m| {
        let records = m.families.get_mut("guest_bench").unwrap();
        for rec in records {
            if rec.scope.get("workload").map(|s| s.as_str()) == Some("radix-sort-u64")
                && rec.scope.get("profile").map(|s| s.as_str()) == Some("provable-120")
            {
                rec.metrics.insert(
                    "proof_size".to_string(),
                    cw_syntax::CanonicalValue::Number(603_943.0),
                );
            }
        }
    });

    let check_det = run_check(ws).unwrap();
    assert!(
        check_det
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::CW3003),
        "1-byte proof_size change must trigger CW3003"
    );

    let sync_det = run_sync(ws, false, false, false).unwrap();
    assert_eq!(sync_det.files_written, 2);
    let readme_after = fs::read_to_string(ws.join("README.md")).unwrap();
    assert!(readme_after.contains("603,943 B (589.8 KiB)"));
}

/// Acceptance Matrix Point 8 (`spec.md` §10.2 #8):
/// Assertion Guard Refusal (`CW3002`).
#[test]
fn point_08_assertion_guard_refusal_cw3002() {
    let fix = TempFixture::new("point08");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    // Set `provable-120` proof size to `620,000 B` (> 599 KiB = 613,376 B)
    update_current_bench_manifest(ws, |m| {
        let records = m.families.get_mut("guest_bench").unwrap();
        for rec in records {
            if rec.scope.get("workload").map(|s| s.as_str()) == Some("radix-sort-u64")
                && rec.scope.get("profile").map(|s| s.as_str()) == Some("provable-120")
            {
                rec.metrics.insert(
                    "proof_size".to_string(),
                    cw_syntax::CanonicalValue::Number(620_000.0),
                );
            }
        }
    });

    let sync_res = run_sync(ws, false, false, false).unwrap();
    assert_eq!(sync_res.files_written, 0, "CW3002 must block cw sync");
    assert!(
        sync_res.diagnostics.iter().any(|d| {
            d.code == DiagnosticCode::CW3002 && d.message.contains("check.proof_ahead_of_paper")
        }),
        "expected CW3002 on check.proof_ahead_of_paper, got: {:?}",
        sync_res.diagnostics
    );

    // Verify Markdown still has `603,942 B` and was NOT overwritten with `620,000 B`
    let bench = fs::read_to_string(ws.join("docs/BENCHMARKS.md")).unwrap();
    assert!(bench.contains("603,942 B"));
    assert!(!bench.contains("620,000 B"));
}

/// Acceptance Matrix Point 9 (`spec.md` §10.2 #9):
/// Manual Edit Conflict (`CW4001`) & Context Drift (`CW4002`).
#[test]
fn point_09_manual_edit_conflict_cw4001_and_context_drift_cw4002() {
    let fix = TempFixture::new("point09");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    // 1. Hand-edit `**8.55 s**` to `**5.00 s**` in `README.md:50` -> triggers `CW4001` (and NOT `CW3003`)
    let readme_path = ws.join("README.md");
    let orig_readme = fs::read_to_string(&readme_path).unwrap();
    let edited_readme = orig_readme.replacen("**8.55 s**", "**5.00 s**", 1);
    fs::write(&readme_path, &edited_readme).unwrap();

    let check_4001 = run_check(ws).unwrap();
    assert!(
        check_4001
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::CW4001 && d.file.as_deref() == Some("README.md")),
        "expected CW4001, got: {:?}",
        check_4001.diagnostics
    );
    assert!(
        !check_4001
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::CW3003),
        "CW4001 must take precedence over CW3003 (§8.2)"
    );

    // Plain `cw sync` refuses to overwrite manual edit
    let sync_blocked = run_sync(ws, false, false, false).unwrap();
    assert_eq!(sync_blocked.files_written, 0);

    // `cw sync --force` restores `**8.55 s**`
    let sync_forced = run_sync(ws, false, true, false).unwrap();
    assert!(sync_forced.diagnostics.is_empty());
    assert_eq!(sync_forced.files_written, 1);
    assert_eq!(fs::read_to_string(&readme_path).unwrap(), orig_readme);

    // 2. Edit `At `T = 2^22`` to `At `T = 2^24`` around the throughput span in `README.md:53` -> triggers `CW4002`
    let drift_readme = orig_readme.replacen("At `T = 2^22`", "At `T = 2^24`", 1);
    fs::write(&readme_path, &drift_readme).unwrap();

    let check_4002 = run_check(ws).unwrap();
    assert!(
        check_4002.diagnostics.iter().any(|d| {
            d.code == DiagnosticCode::CW4002
                && d.file.as_deref() == Some("README.md")
                && d.line == Some(53)
        }),
        "expected CW4002 at README.md:53, got: {:?}",
        check_4002.diagnostics
    );

    // Plain `cw sync` blocks on CW4002
    let sync_ctx_blocked = run_sync(ws, false, false, false).unwrap();
    assert_eq!(sync_ctx_blocked.files_written, 0);

    // `cw sync --ack-context` updates the locked `context_digest`
    let sync_ack = run_sync(ws, false, false, true).unwrap();
    assert!(sync_ack.diagnostics.is_empty());
    assert!(sync_ack.lockfile_updated);

    let check_after_ack = run_check(ws).unwrap();
    assert!(check_after_ack.diagnostics.is_empty());
}

/// Acceptance Matrix Point 10 (`spec.md` §10.2 #10):
/// Full Provenance Trace (`cw explain README.md:53`).
#[test]
fn point_10_full_provenance_trace_cw_explain() {
    let fix = TempFixture::new("point10");
    let ws = fix.path();

    let trace = run_explain(ws, "README.md:53").unwrap();
    assert!(
        trace.contains("README.md:53  \"~491k cycles/s\""),
        "got:\n{trace}"
    );
    assert!(
        trace.contains("└─ guest_bench[radix-sort-u64, test].throughput = cycles / prove_wall"),
        "got:\n{trace}"
    );
    assert!(
        trace.contains("├─ cycles     = 4,194,304   ← om.benchmarks@current"),
        "got:\n{trace}"
    );
    assert!(
        trace.contains("└─ prove_wall = 8.5487 s    ← om.benchmarks@current"),
        "got:\n{trace}"
    );
    assert!(
        trace.contains("run     bench-20261016-m4pro  (Apple M4 Pro, 14-core, 24 GiB)"),
        "got:\n{trace}"
    );
    assert!(trace.contains("commit  9872a7e  (clean)"), "got:\n{trace}");
    assert!(trace.contains("fresh"), "got:\n{trace}");
}

/// End-to-end test for `cw capture`, `cw diff`, `cw merge-driver`, and crash-safe staging journal recovery.
#[test]
fn bonus_capture_diff_merge_driver_and_journal_recovery() {
    let fix = TempFixture::new("bonus");
    let ws = fix.path();
    run_sync(ws, false, false, false).unwrap();

    // 1. Test `cw diff` semantic classification between two snapshots
    let snap_a = RevisionSnapshot::from_dir(ws);
    update_current_bench_manifest(ws, |m| {
        // Swap phase_bench record order -> DefinitionChanged (`prefix_sum` order changed, §4.4!)
        if let Some(phases) = m.families.get_mut("phase_bench") {
            phases.swap(0, 1);
        }
    });
    let snap_b = RevisionSnapshot::from_dir(ws);
    let diffs = compare_snapshots(&snap_a, &snap_b);
    assert!(
        diffs
            .iter()
            .any(|d| d.category == DiffCategory::DefinitionChanged && d.subject == "phase_bench"),
        "expected DefinitionChanged on phase_bench record order swap, got: {diffs:?}"
    );

    // 2. Test `cw capture` with `@cw:record` stdout wire protocol
    let claims_src = fs::read_to_string(ws.join(".claimwire/claims.yaml")).unwrap();
    let (claims_cfg, _): (ClaimsConfig, _) = from_yaml_str(&claims_src).unwrap();
    let ev_src = fs::read_to_string(ws.join(".claimwire/evidence.yaml")).unwrap();
    let (ev_cfg, _): (EvidenceConfig, _) = from_yaml_str(&ev_src).unwrap();

    let (cap_manifest, _, _) = execute_capture(
        ws,
        "om.inventory",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo '@cw:record rust_crates=16 vector_files=27 table1_workloads=15 table1_proved=11 rust_tests=365 python_tests=227 isa_tests=46 oracle_cases=47'".to_string(),
        ],
        Some("inventory-captured-test"),
        &ev_cfg,
        &claims_cfg,
    )
    .unwrap();
    assert_eq!(
        cap_manifest.scalars.get("rust_tests"),
        Some(&cw_syntax::CanonicalValue::Number(365.0))
    );

    // 3. Test `cw merge-driver` 3-way resolution on `.claimwire/evidence.lock.json`
    let base_lock = EvidenceLockfile::load(ws).unwrap();
    let mut ours_lock = base_lock.clone();
    let mut theirs_lock = base_lock.clone();

    ours_lock.evaluated_claims.insert(
        "branch_ours.metric".to_string(),
        cw_engine::LockedClaimValue {
            value: serde_json::json!(10),
            unit: "count".to_string(),
            epoch: Some("current".to_string()),
        },
    );
    theirs_lock.evaluated_claims.insert(
        "branch_theirs.metric".to_string(),
        cw_engine::LockedClaimValue {
            value: serde_json::json!(20),
            unit: "count".to_string(),
            epoch: Some("current".to_string()),
        },
    );

    let base_file = ws.join("lock.base.json");
    let ours_file = ws.join("lock.ours.json");
    let theirs_file = ws.join("lock.theirs.json");
    fs::write(&base_file, base_lock.to_pretty_json().unwrap()).unwrap();
    fs::write(&ours_file, ours_lock.to_pretty_json().unwrap()).unwrap();
    fs::write(&theirs_file, theirs_lock.to_pretty_json().unwrap()).unwrap();

    let clean = run_merge_driver(&base_file, &ours_file, &theirs_file).unwrap();
    assert!(clean, "disjoint lockfile merge should succeed cleanly");
    let merged: EvidenceLockfile =
        serde_json::from_str(&fs::read_to_string(&ours_file).unwrap()).unwrap();
    assert!(merged.evaluated_claims.contains_key("branch_ours.metric"));
    assert!(merged.evaluated_claims.contains_key("branch_theirs.metric"));

    // 4. Test crash-safe write-ahead journal recovery (`spec.md` §2.2 Invariant 5 & §8.4)
    let staging_dir = ws.join(".claimwire/.staging");
    fs::create_dir_all(&staging_dir).unwrap();
    fs::write(staging_dir.join("staged_0.tmp"), "recovered content\n").unwrap();
    let mut staged_files = indexmap::IndexMap::new();
    staged_files.insert("RECOVERED.md".to_string(), "staged_0.tmp".to_string());
    let journal = WriteAheadJournal { staged_files };
    fs::write(
        staging_dir.join("journal.json"),
        serde_json::to_string_pretty(&journal).unwrap(),
    )
    .unwrap();

    recover_staging_journal(ws).unwrap();
    assert_eq!(
        fs::read_to_string(ws.join("RECOVERED.md")).unwrap(),
        "recovered content\n"
    );
    assert!(!staging_dir.join("journal.json").exists());
}
