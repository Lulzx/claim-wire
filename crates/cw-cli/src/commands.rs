//! Rich developer-experience subcommands for `cw`:
//! `init`, `scan`, `status`, `list`, `explain CWxxxx`, `completions`, and unified diff rendering.

use crate::style::Styler;
use anyhow::{Result, bail};
use cw_engine::{LocationKind, audit_locations, load_workspace_pipeline};
use cw_syntax::{DiagnosticCode, FormatPattern, MarkdownDocument, Severity, ValueType};
use indexmap::IndexMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// `cw init [--force] [--git]`: Scaffolds `.claimwire/` in a new repository.
pub fn cmd_init(workspace_root: &Path, args: &[String], styler: &Styler) -> Result<u8> {
    let mut force = false;
    let mut setup_git = false;

    for arg in args {
        match arg.as_str() {
            "--force" | "-f" => force = true,
            "--git" => setup_git = true,
            other => bail!("Unknown option `{other}` for `cw init` (expected `--force` or `--git`)"),
        }
    }

    let cw_dir = workspace_root.join(".claimwire");
    let claims_path = cw_dir.join("claims.yaml");
    let evidence_path = cw_dir.join("evidence.yaml");
    let runs_dir = cw_dir.join("runs");

    if !force && (claims_path.exists() || evidence_path.exists()) {
        eprintln!(
            "{} `.claimwire/` already exists in `{}` (pass `--force` to overwrite starter files)",
            styler.badge_warn("NOTE"),
            workspace_root.display()
        );
        return Ok(1);
    }

    let total_steps = if setup_git { 3 } else { 2 };
    styler.animate_stage(1, total_steps, "Scaffolding .claimwire/ directory & run store...", || {
        fs::create_dir_all(&runs_dir)
    })?;

    let starter_evidence = r#"schema: claimwire-evidence/v1

producers:
  app.benchmarks:
    command: ["cargo", "test", "--release", "--", "--nocapture"]
    families: [bench]
    inputs:
      files: ["src/**/*.rs", "crates/**/*.rs", "Cargo.lock"]
      env: ["RUSTFLAGS"]
    on_stale: error
    epochs:
      current: initial-run
"#;

    let starter_claims = r#"schema: claimwire/v1

constants: {}

families:
  bench:
    producer: app.benchmarks
    dimensions: [workload]
    metrics:
      wall_time: { type: Duration, format: "{:.2f s}", deadband: 2.5% }

claims: {}

assertions: {}

blocks: {}

tables: {}
"#;

    styler.animate_stage(2, total_steps, "Writing starter claims.yaml & evidence.yaml...", || -> Result<()> {
        fs::write(&evidence_path, starter_evidence)?;
        fs::write(&claims_path, starter_claims)?;
        let gitignore_path = workspace_root.join(".gitignore");
        let existing_gi = fs::read_to_string(&gitignore_path).unwrap_or_default();
        if !existing_gi.contains(".claimwire/.cache") {
            let mut updated = existing_gi;
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(
                "\n# Claim Wire local cache and transactional staging\n**/.claimwire/.cache/\n**/.claimwire/.staging/\n",
            );
            fs::write(&gitignore_path, updated)?;
        }
        Ok(())
    })?;

    println!(
        "{}",
        styler.banner("Claim Wire Initialized", &workspace_root.display().to_string())
    );
    println!(
        "  {} {}  {}",
        styler.green_bold("╭─✦"),
        styler.badge_ok("CREATED"),
        styler.bold(".claimwire/evidence.yaml")
    );
    println!(
        "  {} {}  {}",
        styler.green_bold("├─✦"),
        styler.badge_ok("CREATED"),
        styler.bold(".claimwire/claims.yaml")
    );
    println!(
        "  {} {}  {}",
        if setup_git {
            styler.green_bold("├─✦")
        } else {
            styler.green_bold("╰─✦")
        },
        styler.badge_ok("CREATED"),
        styler.bold(".claimwire/runs/")
    );

    if setup_git {
        styler.animate_stage(3, total_steps, "Configuring structural Git merge driver...", || -> Result<()> {
            let gitattributes_path = workspace_root.join(".gitattributes");
            let existing_ga = fs::read_to_string(&gitattributes_path).unwrap_or_default();
            if !existing_ga.contains("merge=claimwire") {
                let mut updated = existing_ga;
                if !updated.is_empty() && !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push_str(".claimwire/evidence.lock.json merge=claimwire\n");
                fs::write(&gitattributes_path, updated)?;
            }
            if workspace_root.join(".git").exists() {
                let _ = Command::new("git")
                    .args(["config", "merge.claimwire.name", "Claim Wire structural merge driver"])
                    .current_dir(workspace_root)
                    .output();
                let _ = Command::new("git")
                    .args(["config", "merge.claimwire.driver", "cw merge-driver %O %A %B"])
                    .current_dir(workspace_root)
                    .output();
            }
            Ok(())
        })?;
        println!(
            "  {} {}  {}",
            styler.green_bold("╰─✦"),
            styler.badge_info("GIT"),
            styler.bold(".gitattributes + merge.claimwire.driver")
        );
    }

    println!("\n{}", styler.brand_gradient("◈ Next Steps"));
    println!(
        "  {} {:<18} {}",
        styler.magenta_bold("1."),
        styler.cyan_bold("cw scan"),
        styler.dim("Discover tables, blocks, and quantitative claims in your Markdown")
    );
    println!(
        "  {} {:<18} {}",
        styler.magenta_bold("2."),
        styler.cyan_bold("cw adopt"),
        styler.dim("Bootstrap .claimwire/evidence.lock.json from existing Markdown")
    );
    println!(
        "  {} {:<18} {}",
        styler.magenta_bold("3."),
        styler.cyan_bold("cw check"),
        styler.dim("Verify all bound locations, types, and assertions")
    );
    Ok(0)
}

/// `cw scan [<file...>]`: Scans Markdown files for GFM tables, code blocks, lenses,
/// broken code-span pipes (`CW1008`), and unwired quantitative claims.
pub fn cmd_scan(workspace_root: &Path, args: &[String], styler: &Styler) -> Result<u8> {
    let mut md_files = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            bail!("Unknown option `{arg}` for `cw scan`");
        }
        md_files.push(arg.clone());
    }

    if md_files.is_empty() {
        md_files = collect_markdown_files(workspace_root);
    }

    if md_files.is_empty() {
        println!("No Markdown files found in `{}`.", workspace_root.display());
        return Ok(0);
    }

    styler.animate_sweep("Scanning Markdown CST", &md_files, 38);

    let mut total_bound_tables = 0usize;
    let mut total_unbound_tables = 0usize;
    let mut total_bound_blocks = 0usize;
    let mut total_inline_spans = 0usize;
    let mut total_cw1008 = 0usize;

    println!(
        "{}",
        styler.banner(
            &format!(
                "Markdown Lens & Table Scanner ({} {})",
                md_files.len(),
                if md_files.len() == 1 { "file" } else { "files" }
            ),
            &workspace_root.display().to_string()
        )
    );

    for rel_file in &md_files {
        let full_path = workspace_root.join(rel_file);
        let Ok(source) = fs::read_to_string(&full_path) else {
            continue;
        };
        let doc = MarkdownDocument::parse(rel_file, source.clone());
        let unbound_tables = count_unbound_tables(&source);
        let cw1008_diags: Vec<_> = doc
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::CW1008)
            .collect();

        total_bound_tables += doc.tables.len();
        total_unbound_tables += unbound_tables.len();
        total_bound_blocks += doc.blocks.len();
        total_inline_spans += doc.inline_spans.len();
        total_cw1008 += cw1008_diags.len();

        if doc.tables.is_empty()
            && unbound_tables.is_empty()
            && doc.blocks.is_empty()
            && doc.inline_spans.is_empty()
            && cw1008_diags.is_empty()
        {
            continue;
        }

        println!(
            "\n{} {}",
            styler.magenta_bold("◈"),
            styler.gradient(rel_file, (0, 245, 212), (56, 189, 248))
        );
        for t in &doc.tables {
            let cols: Vec<&str> = t.header.cells.iter().map(|c| c.text.trim()).collect();
            println!(
                "  {} line {:<4} table {:<28} {}",
                styler.badge_ok("✓ WIRED  "),
                t.header.line_no,
                styler.cyan_bold(&t.lens_id),
                styler.dim(&format!(
                    "({} rows × {} cols: {})",
                    t.rows.len(),
                    t.header.cells.len(),
                    cols.join(" │ ")
                ))
            );
        }
        for (line_no, rows, cols_preview) in &unbound_tables {
            println!(
                "  {} line {:<4} {:<34} {}",
                styler.badge_warn("○ UNWIRED"),
                line_no,
                styler.yellow("unbound GFM table"),
                styler.dim(&format!("({rows} rows: {cols_preview})"))
            );
        }
        for b in &doc.blocks {
            let kind = if b.is_fenced_code {
                "fenced code"
            } else {
                "prose block"
            };
            println!(
                "  {} line {:<4} block {:<28} {}",
                styler.badge_ok("✓ WIRED  "),
                b.directive_line + 1,
                styler.cyan_bold(&b.lens_id),
                styler.dim(&format!("({kind}, {} lines)", b.lines.len()))
            );
        }
        if !doc.inline_spans.is_empty() {
            println!(
                "  {} {} inline {} (`<!-- cw:... -->`)",
                styler.badge_info("✦ SPANS  "),
                styler.cyan_bold(&doc.inline_spans.len().to_string()),
                if doc.inline_spans.len() == 1 {
                    "span"
                } else {
                    "spans"
                }
            );
        }
        for d in cw1008_diags {
            println!(
                "  {} line {}:{} — {} {}",
                styler.badge_err("✗ CW1008 "),
                d.line.unwrap_or(0),
                d.col.unwrap_or(0),
                styler.red_bold(&d.message),
                styler.dim("(escape as `\\|`)")
            );
        }
    }

    let total_tables = total_bound_tables + total_unbound_tables;
    println!("\n{}", styler.brand_gradient("◈ Scan Summary"));
    println!(
        "  ├─ Wired lenses:   {} tables, {} blocks, {} inline spans",
        styler.green_bold(&total_bound_tables.to_string()),
        styler.green_bold(&total_bound_blocks.to_string()),
        styler.cyan_bold(&total_inline_spans.to_string())
    );
    println!(
        "  ├─ Table coverage: {}  ({total_bound_tables}/{total_tables} GFM tables wired)",
        styler.progress_bar(total_bound_tables, total_tables.max(1), 22)
    );
    if total_cw1008 > 0 {
        println!(
            "  ╰─ {} {} GFM table row(s) have unescaped pipes inside code spans (`CW1008` — run `{}`)",
            styler.badge_err("LINT"),
            styler.red_bold(&total_cw1008.to_string()),
            styler.cyan_bold("cw explain CW1008")
        );
    } else {
        println!(
            "  ╰─ {} 0 table syntax warnings",
            styler.badge_ok("CLEAN")
        );
    }

    Ok(0)
}

fn collect_markdown_files(workspace_root: &Path) -> Vec<String> {
    let mut md_files = Vec::new();
    if workspace_root.join("README.md").is_file() {
        md_files.push("README.md".to_string());
    }
    fn walk_md(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_md(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Ok(rel) = path.strip_prefix(root)
            {
                let rel_slash = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(rel_slash);
            }
        }
    }
    let docs_dir = workspace_root.join("docs");
    if docs_dir.is_dir() {
        walk_md(workspace_root, &docs_dir, &mut md_files);
    }
    md_files.sort_by(|a, b| {
        if a == "README.md" {
            std::cmp::Ordering::Less
        } else if b == "README.md" {
            std::cmp::Ordering::Greater
        } else {
            a.cmp(b)
        }
    });
    md_files.dedup();
    md_files
}

fn count_unbound_tables(source: &str) -> Vec<(usize, usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_fence = false;

    while i < lines.len() {
        let t = lines[i].trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            i += 1;
            continue;
        }
        if in_fence {
            i += 1;
            continue;
        }
        if t.starts_with("<!-- cw:table ") {
            i += 2;
            while i < lines.len() && lines[i].trim().contains('|') {
                i += 1;
            }
            continue;
        }
        if i + 1 < lines.len()
            && t.contains('|')
            && is_delimiter_row(lines[i + 1].trim())
        {
            let hdr_line = i + 1;
            let hdr_preview = t.trim_matches('|').trim().to_string();
            i += 2;
            let mut rows = 0usize;
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && lines[i].trim().contains('|')
            {
                rows += 1;
                i += 1;
            }
            out.push((hdr_line, rows, hdr_preview));
            continue;
        }
        i += 1;
    }
    out
}

fn is_delimiter_row(s: &str) -> bool {
    let trimmed = s.trim();
    if !trimmed.contains('|') || !trimmed.contains('-') {
        return false;
    }
    let inner = trimmed.trim_matches('|');
    inner.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty()
            && c.trim_start_matches(':')
                .trim_end_matches(':')
                .bytes()
                .all(|b| b == b'-')
    })
}

/// `cw status`: High-signal dashboard of producers, claims, assertions, and Markdown locations.
pub fn cmd_status(workspace_root: &Path, styler: &Styler) -> Result<u8> {
    let started = Instant::now();
    let (ctx_opt, mut diagnostics) = styler.animate_stage(
        1,
        2,
        "Evaluating claim DAG, semantic fingerprints & run manifests...",
        || load_workspace_pipeline(workspace_root, false, None),
    )?;
    let Some(ctx) = ctx_opt else {
        for d in &diagnostics {
            print!("{}", styler.style_diagnostic(&d.format_human()));
        }
        return Ok(1);
    };

    let audit_diags = styler.animate_stage(
        2,
        2,
        "Auditing Markdown CST locations, deadbands & lockfile...",
        || {
            audit_locations(
                &ctx.lowering.locations,
                ctx.lockfile.as_ref(),
                &ctx.ir,
                &ctx.evidence,
                true,
                false,
                false,
            )
        },
    );
    diagnostics.extend(audit_diags);
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    println!(
        "{}",
        styler.banner("Claim Wire Workspace Dashboard", &workspace_root.display().to_string())
    );

    // 1. Producers section
    println!(
        "\n{} {}",
        styler.magenta_bold("◈"),
        styler.brand_gradient(&format!(
            "Producers ({})",
            ctx.evidence.config.producers.len()
        ))
    );
    for (prod_id, prod_decl) in &ctx.evidence.config.producers {
        let is_fresh = ctx.evidence.freshness.get(prod_id).copied().unwrap_or(false);
        let badge = if is_fresh {
            styler.badge_ok("✓ FRESH")
        } else {
            styler.badge_err("✗ STALE")
        };
        let cur_run = prod_decl
            .epochs
            .get("current")
            .map(|s| s.as_str())
            .unwrap_or("—");
        let (commit, rec_count, hw) = match ctx.evidence.get_manifest(prod_id, "current") {
            Some(m) => {
                let count = m.families.values().map(|v| v.len()).sum::<usize>() + m.scalars.len();
                let hw_str = m.hardware.as_deref().unwrap_or("");
                (m.git_commit.as_str(), count, hw_str)
            }
            None => ("missing", 0, ""),
        };
        let epochs_list: Vec<&str> = prod_decl.epochs.keys().map(|k| k.as_str()).collect();
        let hw_suffix = if hw.is_empty() {
            String::new()
        } else {
            format!(" · {hw}")
        };
        println!(
            "  {badge}  {:<16} @current={:<22} commit={} {:>3} records  epochs=[{}]{}",
            styler.cyan_bold(prod_id),
            styler.bold(cur_run),
            styler.yellow(commit),
            rec_count,
            epochs_list.join(", "),
            styler.dim(&hw_suffix)
        );
    }

    // 2. Claims & Assertions section
    let num_families = ctx.ir.config.families.len();
    let num_family_records: usize = ctx
        .eval
        .display
        .families
        .values()
        .filter_map(|ep| ep.get("current"))
        .map(|v| v.len())
        .sum();
    let num_scalars = ctx.ir.config.claims.len();
    let num_consts = ctx.ir.config.constants.len();
    let total_assertions = ctx.ir.config.assertions.len();
    let failed_assertions = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::CW3002)
        .count();
    let passed_assertions = total_assertions.saturating_sub(failed_assertions);

    println!(
        "\n{} {}",
        styler.magenta_bold("◈"),
        styler.brand_gradient("Claim Graph & Assertions")
    );
    println!(
        "  ├─ Families:    {} ({} records at @current)",
        styler.cyan_bold(&num_families.to_string()),
        styler.bold(&num_family_records.to_string())
    );
    println!(
        "  ├─ Scalars:     {} claims ({} constants)",
        styler.cyan_bold(&num_scalars.to_string()),
        styler.bold(&num_consts.to_string())
    );
    if failed_assertions == 0 {
        println!(
            "  ╰─ Assertions:  {}  {} ({passed_assertions}/{total_assertions} passing)",
            styler.badge_ok("✓ ALL PASSING"),
            styler.progress_bar(passed_assertions, total_assertions.max(1), 16)
        );
    } else {
        println!(
            "  ╰─ Assertions:  {}  {} ({failed_assertions} failing of {total_assertions})",
            styler.badge_err("✗ FAILING"),
            styler.progress_bar(passed_assertions, total_assertions.max(1), 16)
        );
    }

    // 3. Markdown Locations per file
    let mut by_file: IndexMap<String, (usize, usize, usize, usize, usize)> = IndexMap::new();
    for (file, doc) in &ctx.lowering.documents {
        by_file.insert(
            file.clone(),
            (
                doc.tables.len(),
                doc.blocks.len(),
                doc.inline_spans.len(),
                0,
                0,
            ),
        );
    }
    for loc in &ctx.lowering.locations {
        let entry = by_file
            .entry(loc.file.clone())
            .or_insert((0, 0, 0, 0, 0));
        entry.3 += 1;
        if loc.current_text != loc.expected_text {
            entry.4 += 1;
        }
    }

    let total_locs = ctx.lowering.locations.len();
    let out_of_sync_locs: usize = by_file.values().map(|v| v.4).sum();
    let synced_locs = total_locs.saturating_sub(out_of_sync_locs);
    println!(
        "\n{} {}",
        styler.magenta_bold("◈"),
        styler.brand_gradient(&format!(
            "Markdown Bindings ({total_locs} locations across {} files)",
            by_file.len()
        ))
    );
    for (file, (tables, blocks, spans, locs, drifted)) in &by_file {
        let in_sync = locs.saturating_sub(*drifted);
        let status = if *drifted == 0 {
            styler.badge_ok(&format!("✓ {locs:>3} SYNCED"))
        } else {
            styler.badge_err(&format!("✗ {drifted:>3} DRIFT "))
        };
        let bar = styler.progress_bar(in_sync, (*locs).max(1), 12);
        println!(
            "  {status}  {bar}  {:<26} {}",
            styler.bold(file),
            styler.dim(&format!("({tables} tables, {blocks} blocks, {spans} spans)"))
        );
    }

    let err_count = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    println!();
    if err_count == 0 {
        println!(
            "{} {} {} registered locations verified in {:.1} ms · 0 errors",
            styler.badge_ok("✓ VERIFIED"),
            styler.progress_bar(synced_locs, total_locs.max(1), 18),
            styler.green_bold(&total_locs.to_string()),
            elapsed_ms
        );
    } else {
        println!(
            "{} {} issue(s) detected across {} registered locations ({:.1} ms) — run `{}` for details or `{}` to update",
            styler.badge_err("✗ DRIFT DETECTED"),
            styler.red_bold(&err_count.max(out_of_sync_locs).to_string()),
            total_locs,
            elapsed_ms,
            styler.cyan_bold("cw check"),
            styler.cyan_bold("cw sync")
        );
    }
    Ok(0)
}

/// `cw list [producers|families|claims|assertions|locations]`: Inspect declared entities and current values.
pub fn cmd_list(workspace_root: &Path, args: &[String], styler: &Styler) -> Result<u8> {
    let filter = args.first().map(|s| s.as_str()).unwrap_or("all");
    let (ctx_opt, diagnostics) = styler.animate_stage(
        1,
        1,
        "Loading & evaluating Claim Wire IR...",
        || load_workspace_pipeline(workspace_root, false, None),
    )?;
    let Some(ctx) = ctx_opt else {
        for d in &diagnostics {
            print!("{}", styler.style_diagnostic(&d.format_human()));
        }
        return Ok(1);
    };

    match filter {
        "all" | "producers" | "families" | "claims" | "assertions" | "locations" => {}
        other => bail!(
            "Unknown filter `{other}` for `cw list` (expected producers|families|claims|assertions|locations)"
        ),
    }

    if filter == "all" || filter == "producers" {
        println!(
            "{} {}",
            styler.magenta_bold("◈"),
            styler.brand_gradient("PRODUCERS")
        );
        for (id, decl) in &ctx.evidence.config.producers {
            let fresh = if ctx.evidence.freshness.get(id).copied().unwrap_or(false) {
                styler.badge_ok("✓ FRESH")
            } else {
                styler.badge_err("✗ STALE")
            };
            let run = decl.epochs.get("current").map(|s| s.as_str()).unwrap_or("—");
            println!(
                "  {fresh}  {:<18} @current={:<24} {}",
                styler.cyan_bold(id),
                styler.bold(run),
                styler.dim(&format!("cmd=`{}`", decl.command.join(" ")))
            );
        }
        println!();
    }

    if filter == "all" || filter == "families" {
        println!(
            "{} {}",
            styler.magenta_bold("◈"),
            styler.brand_gradient("CLAIM FAMILIES")
        );
        for (fname, fdecl) in &ctx.ir.config.families {
            let rec_count = ctx
                .eval
                .display
                .families
                .get(fname)
                .and_then(|e| e.get("current"))
                .map(|r| r.len())
                .unwrap_or(0);
            let metrics: Vec<&str> = fdecl.metrics.keys().map(|k| k.as_str()).collect();
            let derivs: Vec<&str> = fdecl.derivations.keys().map(|k| k.as_str()).collect();
            println!(
                "  {}[{}]  ← {} {}",
                styler.cyan_bold(fname),
                styler.yellow(&fdecl.dimensions.join(", ")),
                styler.bold(&fdecl.producer),
                styler.dim(&format!("({rec_count} records)"))
            );
            println!("    ├─ metrics:     {}", styler.green(&metrics.join(", ")));
            if !derivs.is_empty() {
                println!("    ╰─ derivations: {}", styler.magenta(&derivs.join(", ")));
            }
        }
        println!();
    }

    if filter == "all" || filter == "claims" {
        println!(
            "{} {}",
            styler.magenta_bold("◈"),
            styler.brand_gradient("CONSTANTS & SCALAR CLAIMS")
        );
        for (name, cdecl) in &ctx.ir.config.constants {
            println!(
                "  {:<36} {:<16} = {:<14} {}",
                styler.cyan(name),
                styler.yellow(&format!("{}", cdecl.value_type)),
                styler.bold(&cdecl.value.to_string()),
                styler.dim("(const)")
            );
        }
        for (name, cdecl) in &ctx.ir.config.claims {
            let val_str = match ctx.eval.display.scalars.get(name) {
                Some((val, ty, _)) => {
                    let fmt_str = cdecl
                        .format
                        .clone()
                        .unwrap_or_else(|| default_fmt_for_type(ty));
                    FormatPattern::parse(&fmt_str)
                        .ok()
                        .and_then(|p| p.render(|_| Ok((val.clone(), ty.clone()))).ok())
                        .unwrap_or_else(|| format!("{val:?}"))
                }
                None => "null".to_string(),
            };
            let kind_label = cdecl.kind.as_deref().unwrap_or("derived");
            println!(
                "  {:<36} {:<16} = {:<14} {}",
                styler.cyan_bold(name),
                styler.yellow(&format!("{}", cdecl.value_type)),
                styler.green_bold(&val_str),
                styler.dim(&format!("({kind_label})"))
            );
        }
        println!();
    }

    if filter == "all" || filter == "assertions" {
        println!(
            "{} {}",
            styler.magenta_bold("◈"),
            styler.brand_gradient("ASSERTIONS")
        );
        for (aname, adecl) in &ctx.ir.config.assertions {
            let failed = diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::CW3002 && d.message.contains(aname));
            let mark = if failed {
                styler.badge_err("✗ FAIL")
            } else {
                styler.badge_ok("✓ PASS")
            };
            println!(
                "  {mark}  {:<30} {}",
                styler.cyan_bold(aname),
                styler.bold(&adecl.expr)
            );
        }
        println!();
    }

    if filter == "locations" {
        println!(
            "{} {}",
            styler.magenta_bold("◈"),
            styler.brand_gradient(&format!(
                "BOUND LOCATIONS ({})",
                ctx.lowering.locations.len()
            ))
        );
        for loc in &ctx.lowering.locations {
            let synced = if loc.current_text == loc.expected_text {
                styler.green_bold("✓")
            } else {
                styler.red_bold("✗")
            };
            let kind = match &loc.kind {
                LocationKind::InlineSpan { claim_ref } => format!("span {claim_ref}"),
                LocationKind::BlockLine {
                    lens_id,
                    pattern_idx,
                } => {
                    format!("block {lens_id}[{pattern_idx}]")
                }
                LocationKind::TableCell {
                    lens_id,
                    row_label,
                    col_label,
                } => format!("table {lens_id} {row_label}/{col_label}"),
            };
            println!(
                "  {synced} {:<28} {:<45} `{}`",
                styler.cyan(&format!("{}:{}:{}", loc.file, loc.line, loc.col)),
                styler.dim(&kind),
                styler.bold(&loc.current_text)
            );
        }
    }

    Ok(0)
}

fn default_fmt_for_type(ty: &ValueType) -> String {
    match ty {
        ValueType::Duration => "{:.2f s}".to_string(),
        ValueType::Bytes => "{:, B}".to_string(),
        ValueType::Cycles => "{:,}".to_string(),
        ValueType::Throughput => "{:.0f cycles/s}".to_string(),
        ValueType::Count => "{}".to_string(),
        ValueType::PassRatio => "{passed}/{total}".to_string(),
        ValueType::Ratio | ValueType::Float => "{:.2f}".to_string(),
        ValueType::Quantity(u) => format!("{{:.1f {u}}}"),
        ValueType::Bool | ValueType::String | ValueType::Digest => "{}".to_string(),
    }
}

/// Render a concise unified diff for `cw sync --diff`.
pub fn render_unified_diff(file: &str, old_text: &str, new_text: &str, styler: &Styler) -> String {
    let mut out = String::new();
    out.push_str(&styler.bold(&format!("--- a/{file}\n+++ b/{file}\n")));
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let max_len = old_lines.len().max(new_lines.len());

    for idx in 0..max_len {
        let o = old_lines.get(idx).copied();
        let n = new_lines.get(idx).copied();
        if o != n {
            out.push_str(&styler.cyan(&format!("@@ -{},1 +{},1 @@\n", idx + 1, idx + 1)));
            if let Some(ol) = o {
                out.push_str(&styler.red(&format!("-{ol}\n")));
            }
            if let Some(nl) = n {
                out.push_str(&styler.green(&format!("+{nl}\n")));
            }
        }
    }
    out
}

/// Built-in reference manual for `cw explain CWxxxx` (like `rustc --explain`).
pub fn explain_diagnostic_code(raw: &str, styler: &Styler) -> Option<String> {
    let code = raw.trim().to_ascii_uppercase();
    let (title, stage, explanation, fix) = match code.as_str() {
        "CW1001" => (
            "Malformed Claim Wire HTML comment marker",
            "Syntax & Lens Parsing (cw-syntax)",
            "An HTML comment starting with `<!-- cw:` or `<!-- /cw` is unclosed (`-->` missing) or contains invalid directive syntax.",
            "Ensure inline spans use `<!-- cw:<claim_ref> -->...<!-- /cw -->` on a single line, and standalone directives use `<!-- cw:table <id> -->` or `<!-- cw:block <id> -->`.",
        ),
        "CW1002" => (
            "Unclosed inline span (`<!-- /cw -->` missing)",
            "Syntax & Lens Parsing (cw-syntax)",
            "An inline span `<!-- cw:... -->` was opened on a line, or another `<!-- cw:... -->` was opened before the previous span was closed with `<!-- /cw -->`.",
            "Close every inline span on the same line with `<!-- /cw -->`. Inline spans cannot nest or cross line breaks.",
        ),
        "CW1003" => (
            "Orphan closing tag (`<!-- /cw -->` without matching open)",
            "Syntax & Lens Parsing (cw-syntax)",
            "A closing `<!-- /cw -->` marker appeared without a preceding open `<!-- cw:... -->` on the same line.",
            "Remove the extra `<!-- /cw -->` or add the opening `<!-- cw:<claim_ref> -->` marker.",
        ),
        "CW1004" => (
            "Span crosses block or table cell boundary",
            "Syntax & Lens Parsing (cw-syntax)",
            "An inline span `<!-- cw:... -->` was opened inside a GFM table cell or paragraph and was not closed before the `|` cell delimiter or end of line.",
            "Close the span with `<!-- /cw -->` inside the same table cell, or bind the entire table cleanly using `<!-- cw:table <id> -->` above the table.",
        ),
        "CW1005" => (
            "`<!-- cw:table <id> -->` not followed by a valid GFM pipe table",
            "Syntax & Lens Parsing (cw-syntax)",
            "A table lens directive must be immediately followed by a GFM pipe table header row and a delimiter row (`|---|---|`).",
            "Place `<!-- cw:table <id> -->` directly above the `| Header | ... |` line with no intervening paragraphs.",
        ),
        "CW1006" => (
            "`<!-- cw:block <id> -->` not followed by a CommonMark block or pattern mismatch",
            "Syntax & Lens Parsing (cw-syntax)",
            "A block lens directive could not match its declared `lines` patterns in `.claimwire/claims.yaml` against the target fenced code block or paragraph.",
            "Verify that every template in `blocks.<id>.lines` matches a line in the block when placeholders are substituted.",
        ),
        "CW1007" => (
            "Ambiguous (non-invertible) format pattern",
            "Format Compiler (cw-syntax)",
            "Every format pattern must be bidirectional so `cw adopt` and `cw check` can uniquely parse rendered strings back into structured values. Adjacent numeric placeholders without a separating non-numeric literal or unit (e.g. `{a}{b}`) have multiple parses.",
            "Separate placeholders with a unit inside braces or non-numeric literal text (e.g. `{passed}/{total}` or `{verify_wall:.0f ms} ({verify_rss:.0f MiB})`).",
        ),
        "CW1008" => (
            "Unescaped pipe (`|`) splits a code span in a GFM table row",
            "GFM Table Row Lexer (cw-syntax §5.2.1)",
            "In GitHub Flavored Markdown, table cells are split on `|` BEFORE inline code spans are parsed. Writing `` `2k/|K|` `` inside a table cell causes GitHub to split the cell at `|K|` and silently drop trailing columns.",
            "Escape literal pipes inside table cells as `\\|` (even inside backticks: `` `2k/\\|K\\|` ``). `cw` refuses to bind a lens to a broken row until escaped.",
        ),
        "CW2001" => (
            "Dimensional type mismatch in expression or assertion",
            "Type Checker (cw-ir §4.1)",
            "Claim Wire enforces dimensional algebra (`Cycles / Duration -> Throughput`, `Duration + Duration -> Duration`, `Bytes / Bytes -> Ratio`). Combining incompatible dimensions (such as `Duration + Count` or comparing `Bytes <= Duration`) is a compile-time error.",
            "Check the declared `type` of each metric and constant in `.claimwire/claims.yaml` and ensure arithmetic/comparison operators combine compatible units.",
        ),
        "CW2002" => (
            "Cyclic claim or family derivation dependency",
            "Dependency Graph Resolver (cw-ir)",
            "Two or more derived claims or family derivations depend on each other in a cycle, preventing topological evaluation.",
            "Break the dependency cycle in `.claimwire/claims.yaml` so derivations form a directed acyclic graph (DAG) rooted in `source` metrics and `const` declarations.",
        ),
        "CW2003" => (
            "Unresolved claim, family, producer, epoch, or dimension key",
            "Scope & Reference Resolver (cw-ir §4.3)",
            "A reference in Markdown or `.claimwire/claims.yaml` names a family, metric, scalar claim, epoch, or table column that does not exist, or supplies the wrong number of positional selectors for a family's `dimensions`.",
            "Run `cw list` to inspect declared families, dimensions, and claims, or add missing `aliases` in `.claimwire/claims.yaml`.",
        ),
        "CW2004" => (
            "Wildcard (`*`) or subset (`{k1, k2}`) selector used without a reduction",
            "Reduction Checker (cw-ir §4.3, §4.4)",
            "A selector that matches multiple records (such as `phase_bench[*].duration` or `phase_bench[{ram_twist, register_twist}].duration`) cannot be rendered as a scalar without an explicit reduction (`sum(...)`, `min(...)`, `max(...)`).",
            "Wrap multi-record selectors in `sum(...)`, `min(...)`, or `max(...)`, or select a single point record.",
        ),
        "CW2005" => (
            "Missing or invalid unit for metric type in format specification",
            "Format Type Validator (cw-syntax §4.5)",
            "For `Duration`, `Bytes`, and `Throughput`, scaling units must live INSIDE the format braces (`{prove_wall:.2f s}`, `{proof_size:, B}`). Putting the unit outside the braces (`{prove_wall:.2f} s`) or using a unit from another dimension (`{prove_wall:.2f MiB}`) is rejected at compile time.",
            "Move the unit inside the format spec braces (`{:.2f s}`, `{:.0f ms}`, `{:, B}`, `{:.1f KiB}`, `{:auto_iec}`, `{:.0fk cycles/s}`).",
        ),
        "CW2104" => (
            "Missing evidence record or metric for non-sentinel location",
            "Evidence & Lowering (cw-engine §5.2.3)",
            "A bound span, block, or table cell references a record scope or metric that is `null` or absent in the producer's run manifest, and the cell does not contain a declared sentinel (`—`, `N/A`, `exact`).",
            "Either emit the missing `@cw:record` from the producer (`cw capture --producer <id>`) or add the cell's placeholder string to `sentinels: [...]` on the table lens.",
        ),
        "CW3001" => (
            "Producer evidence is stale (`input_set_digest` mismatch)",
            "Semantic Fingerprinter (cw-evidence §6.2)",
            "One or more source files or environment variables declared in `producers.<id>.inputs` have semantically changed (ignoring comments and non-semantic whitespace) since the `@current` run manifest was captured.",
            "Run `cw impact` to see which claims and Markdown locations are affected, then re-run `cw capture --producer <id>` and `cw sync`.",
        ),
        "CW3002" => (
            "Assertion guard evaluated to `false`",
            "Assertion Gate (cw-engine §2.2 Invariant 3)",
            "A boolean invariant in `assertions:` (such as `guest_bench[radix-sort-u64, provable-120].proof_size <= paper.t22.proof_size_max`) evaluated to `false` on fresh evidence. By Invariant 3 (No Silent Weakening), `cw sync` halts and refuses to publish the regressed numbers.",
            "Investigate the upstream code/benchmark regression. Never weaken an assertion bound without explicit maintainer sign-off.",
        ),
        "CW3003" => (
            "Rendered Markdown value is out of sync with evaluated claim",
            "Sync Auditor (cw-engine §4.5 rule 7)",
            "The bytes currently in the Markdown file do not match the rendered value of the bound claim (outside any declared deadband hysteresis).",
            "Run `cw sync` to deterministically patch the outdated Markdown spans, table cells, and block lenses from evidence.",
        ),
        "CW3004" => (
            "`:pow2` format applied to a non-power-of-two value",
            "Format Renderer (cw-syntax §4.5 rule 4)",
            "The `:pow2` named format (`2^k`) requires its input to be an exact positive power of two (`256`, `4,194,304`, etc.).",
            "Use `{:,}` for arbitrary cycle counts or ensure the padded cycle count is an exact power of two.",
        ),
        "CW4001" => (
            "Unapproved manual edit inside a Claim Wire-owned span, cell, or block",
            "Conflict Auditor (cw-engine §8.2)",
            "The current Markdown bytes inside an owned location differ from `rendered_digest` in `.claimwire/evidence.lock.json`, meaning someone hand-edited a generated number directly in Markdown instead of updating evidence.",
            "Run `cw sync --force` to overwrite the manual edit from evidence, or update the upstream constant/producer if the change was intentional.",
        ),
        "CW4002" => (
            "Surrounding prose context drifted from locked `context_digest`",
            "Context Window Auditor (cw-engine §8.3)",
            "The sentence or table row surrounding a bound location was edited (for example, changing `At T = 2^22` to `At T = 2^24`) while the claim binding itself was left unchanged.",
            "Verify whether the prose edit changed the meaning of the claim. If the binding needs updating, edit the `cw:` reference; if the prose edit was harmless, acknowledge it with `cw sync --ack-context`.",
        ),
        _ => return None,
    };

    let mut out = String::new();
    out.push_str(&format!(
        "{}  {}\n",
        styler.badge_err(&code),
        styler.brand_gradient(title)
    ));
    out.push_str(&format!(
        "  {} {}\n\n",
        styler.magenta_bold("╭─▸ Stage:"),
        styler.cyan_bold(stage)
    ));
    out.push_str(&format!(
        "  {}\n    {explanation}\n\n",
        styler.yellow_bold("├─▸ Why this happens:")
    ));
    out.push_str(&format!(
        "  {}\n    {}\n",
        styler.green_bold("╰─▸ How to fix:"),
        styler.bold(fix)
    ));
    Some(out)
}

/// `cw completions <bash|zsh|fish>`: Generate shell completion scripts.
pub fn cmd_completions(args: &[String]) -> Result<u8> {
    let Some(shell) = args.first().map(|s| s.as_str()) else {
        bail!("`cw completions` requires a shell argument (`bash`, `zsh`, or `fish`)");
    };
    match shell {
        "bash" => {
            print!(
                r#"_cw_completions() {{
    local cur prev opts cmds
    COMPREPLY=()
    cur="${{COMP_WORDS[COMP_CWORD]}}"
    prev="${{COMP_WORDS[COMP_CWORD-1]}}"
    cmds="init scan status list check sync capture impact explain diff adopt merge-driver completions help"

    if [[ ${{COMP_CWORD}} -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "${{cmds}} --help --version --color -C" -- "${{cur}}") )
        return 0
    fi

    case "${{COMP_WORDS[1]}}" in
        check)
            COMPREPLY=( $(compgen -W "--format --deny-warnings --help" -- "${{cur}}") )
            ;;
        sync)
            COMPREPLY=( $(compgen -W "--dry-run --diff --exact --force --ack-context --deny-warnings --help" -- "${{cur}}") )
            ;;
        capture)
            COMPREPLY=( $(compgen -W "--producer --run-id --help" -- "${{cur}}") )
            ;;
        impact)
            COMPREPLY=( $(compgen -W "--git-diff --format --help" -- "${{cur}}") )
            ;;
        list)
            COMPREPLY=( $(compgen -W "producers families claims assertions locations" -- "${{cur}}") )
            ;;
        completions)
            COMPREPLY=( $(compgen -W "bash zsh fish" -- "${{cur}}") )
            ;;
        init)
            COMPREPLY=( $(compgen -W "--force --git --help" -- "${{cur}}") )
            ;;
    esac
}}
complete -F _cw_completions cw
"#
            );
        }
        "zsh" => {
            print!(
                r#"#compdef cw
_cw() {{
    local -a commands
    commands=(
        'init:Initialize a new .claimwire/ workspace'
        'scan:Scan Markdown files for GFM tables, blocks, and unwired claims'
        'status:Show workspace health dashboard (producers, claims, coverage)'
        'list:List declared producers, families, claims, assertions, or locations'
        'check:Verify syntax, types, freshness, assertions, and Markdown sync'
        'sync:Re-evaluate claims and transactionally patch Markdown files'
        'capture:Run a producer command and capture @cw:record frames'
        'impact:Trace modified source files forward to affected claims and docs'
        'explain:Print provenance tree for a claim/file:line or explain a CWxxxx code'
        'diff:Classify semantic claim changes across revisions'
        'adopt:Bootstrap evidence.lock.json from existing Markdown with zero byte changes'
        'merge-driver:3-way structural Git merge driver'
        'completions:Generate shell completions for bash, zsh, or fish'
    )
    if (( CURRENT == 2 )); then
        _describe -t commands 'cw command' commands
    else
        _files
    fi
}}
_cw "$@"
"#
            );
        }
        "fish" => {
            print!(
                r#"complete -c cw -f
complete -c cw -n "__fish_use_subcommand" -a init -d "Initialize a new .claimwire/ workspace"
complete -c cw -n "__fish_use_subcommand" -a scan -d "Scan Markdown files for tables, blocks, and claims"
complete -c cw -n "__fish_use_subcommand" -a status -d "Show workspace health dashboard"
complete -c cw -n "__fish_use_subcommand" -a list -d "List producers, families, claims, assertions, or locations"
complete -c cw -n "__fish_use_subcommand" -a check -d "Verify syntax, types, freshness, assertions, and sync"
complete -c cw -n "__fish_use_subcommand" -a sync -d "Transactionally patch Markdown files from evidence"
complete -c cw -n "__fish_use_subcommand" -a capture -d "Run producer and capture @cw:record frames"
complete -c cw -n "__fish_use_subcommand" -a impact -d "Trace modified files forward to affected claims"
complete -c cw -n "__fish_use_subcommand" -a explain -d "Print provenance tree or explain CWxxxx code"
complete -c cw -n "__fish_use_subcommand" -a diff -d "Classify claim changes across revisions"
complete -c cw -n "__fish_use_subcommand" -a adopt -d "Bootstrap evidence.lock.json with zero byte changes"
complete -c cw -n "__fish_use_subcommand" -a merge-driver -d "3-way structural Git merge driver"
complete -c cw -n "__fish_use_subcommand" -a completions -d "Generate shell completions"
"#
            );
        }
        other => bail!("Unsupported shell `{other}` (expected `bash`, `zsh`, or `fish`)"),
    }
    Ok(0)
}
