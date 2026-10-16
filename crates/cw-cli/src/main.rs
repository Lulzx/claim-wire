//! `cw` command-line compiler for Claim Wire (`spec.md` §9.2).

mod commands;
mod style;

use anyhow::Result;
use commands::{
    cmd_completions, cmd_init, cmd_list, cmd_scan, cmd_status, explain_diagnostic_code,
    render_unified_diff,
};
use cw_engine::{
    RevisionSnapshot, compare_snapshots, run_adopt, run_check_files, run_explain, run_impact,
    run_merge_driver, run_sync_options,
};
use cw_evidence::{EvidenceConfig, execute_capture};
use cw_ir::ClaimsConfig;
use cw_syntax::{Diagnostic, Severity, format_diagnostics_sarif, from_yaml_str};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};
use style::{AnimChoice, ColorChoice, LiveSpinner, Styler, style_diagnostic};

const ALL_SUBCOMMANDS: &[&str] = &[
    "init",
    "scan",
    "status",
    "list",
    "check",
    "sync",
    "capture",
    "impact",
    "explain",
    "diff",
    "adopt",
    "merge-driver",
    "completions",
    "help",
];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            let s = Styler::for_stderr(ColorChoice::Auto);
            eprintln!("{} {err:#}", s.badge_err("ERROR"));
            ExitCode::from(2)
        }
    }
}

struct GlobalOpts {
    explicit_dir: Option<PathBuf>,
    color: ColorChoice,
    anim: AnimChoice,
    quiet: bool,
}

fn dispatch(args: &[String]) -> Result<u8> {
    let (opts, filtered_args) = extract_global_opts(args)?;
    let s = Styler::with_anim(opts.color, opts.anim);

    if filtered_args.is_empty() {
        print_help(&s);
        return Ok(0);
    }

    let first = filtered_args[0].as_str();
    if first == "--help" || first == "-h" {
        print_help(&s);
        return Ok(0);
    }
    if first == "--version" || first == "-V" || first == "version" {
        println!(
            "{} {} {}",
            s.brand_gradient("⚡ cw"),
            s.cyan_bold(env!("CARGO_PKG_VERSION")),
            s.dim("(Claim Wire incremental documentation compiler)")
        );
        return Ok(0);
    }
    if first == "help" {
        if let Some(topic) = filtered_args.get(1) {
            return print_subcommand_help(topic, &s);
        }
        print_help(&s);
        return Ok(0);
    }

    let subcmd = first;
    let sub_args = &filtered_args[1..];

    // Subcommand-level --help / -h
    if sub_args.iter().any(|a| a == "--help" || a == "-h") {
        return print_subcommand_help(subcmd, &s);
    }

    // Commands that do not require an initialized `.claimwire/` workspace
    match subcmd {
        "merge-driver" => return cmd_merge_driver(sub_args),
        "completions" => return cmd_completions(sub_args),
        "init" => {
            let target = opts
                .explicit_dir
                .clone()
                .unwrap_or(env::current_dir()?);
            return cmd_init(&target, sub_args, &s);
        }
        "explain" => {
            if let Some(q) = sub_args.first()
                && let Some(doc) = explain_diagnostic_code(q, &s)
            {
                print!("{doc}");
                return Ok(0);
            }
        }
        _ => {}
    }

    if !ALL_SUBCOMMANDS.contains(&subcmd) {
        let s_err = Styler::for_stderr(opts.color);
        eprintln!(
            "{} unknown subcommand `{}`",
            s_err.badge_err("ERROR"),
            s_err.bold(subcmd)
        );
        if let Some(suggestion) = suggest_subcommand(subcmd) {
            eprintln!(
                "  {} did you mean `{}`?",
                s_err.cyan_bold("╭─▸ tip:"),
                s_err.green_bold(suggestion)
            );
        }
        eprintln!("  Run `{}` for usage information.", s_err.bold("cw --help"));
        return Ok(2);
    }

    let workspace_root = resolve_workspace_root(opts.explicit_dir.as_deref(), subcmd, &s)?;

    match subcmd {
        "scan" => cmd_scan(&workspace_root, sub_args, &s),
        "status" => cmd_status(&workspace_root, &s),
        "list" => cmd_list(&workspace_root, sub_args, &s),
        "check" => cmd_check(&workspace_root, sub_args, &s, opts.quiet),
        "sync" => cmd_sync(&workspace_root, sub_args, &s, opts.quiet),
        "adopt" => cmd_adopt(&workspace_root, sub_args, &s),
        "capture" => cmd_capture(&workspace_root, sub_args, &s),
        "impact" => cmd_impact(&workspace_root, sub_args, &s),
        "explain" => cmd_explain(&workspace_root, sub_args, &s),
        "diff" => cmd_diff(&workspace_root, sub_args, &s),
        _ => unreachable!(),
    }
}

fn extract_global_opts(args: &[String]) -> Result<(GlobalOpts, Vec<String>)> {
    let mut explicit_dir = None;
    let mut color = ColorChoice::Auto;
    let mut anim = AnimChoice::Auto;
    let mut quiet = false;
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    let mut passthrough = false;

    while i < args.len() {
        if passthrough {
            out.push(args[i].clone());
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--" => {
                passthrough = true;
                out.push(args[i].clone());
                i += 1;
            }
            "-C" | "--dir" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing directory argument after `{}`", args[i]);
                }
                explicit_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--color" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing argument after `--color` (expected auto|always|never)");
                }
                color = ColorChoice::parse(&args[i + 1])
                    .ok_or_else(|| anyhow::anyhow!("Invalid `--color` value `{}` (expected auto|always|never)", args[i + 1]))?;
                if color == ColorChoice::Always && anim == AnimChoice::Auto {
                    anim = AnimChoice::Always;
                } else if color == ColorChoice::Never {
                    anim = AnimChoice::Never;
                }
                i += 2;
            }
            arg if arg.starts_with("--color=") => {
                let val = &arg["--color=".len()..];
                color = ColorChoice::parse(val)
                    .ok_or_else(|| anyhow::anyhow!("Invalid `--color` value `{val}` (expected auto|always|never)"))?;
                if color == ColorChoice::Always && anim == AnimChoice::Auto {
                    anim = AnimChoice::Always;
                } else if color == ColorChoice::Never {
                    anim = AnimChoice::Never;
                }
                i += 1;
            }
            "--animate" | "--anim" => {
                anim = AnimChoice::Always;
                if color == ColorChoice::Auto {
                    color = ColorChoice::Always;
                }
                i += 1;
            }
            "--no-anim" | "--no-animate" => {
                anim = AnimChoice::Never;
                i += 1;
            }
            "-q" | "--quiet" => {
                quiet = true;
                anim = AnimChoice::Never;
                i += 1;
            }
            _ => {
                out.push(args[i].clone());
                i += 1;
            }
        }
    }

    Ok((
        GlobalOpts {
            explicit_dir,
            color,
            anim,
            quiet,
        },
        out,
    ))
}

/// Discovers the workspace root by checking `-C <dir>` or walking upward from `current_dir()`
/// looking for `.claimwire/claims.yaml` or `.claimwire/evidence.yaml` (just like `git` or `cargo`).
fn resolve_workspace_root(
    explicit_dir: Option<&Path>,
    subcmd: &str,
    s: &Styler,
) -> Result<PathBuf> {
    if let Some(dir) = explicit_dir {
        if subcmd == "scan" || subcmd == "diff" || dir.join(".claimwire").is_dir() {
            return Ok(dir.to_path_buf());
        }
        anyhow::bail!(
            "No `.claimwire/` configuration found in `{}`.\n  {} Run `{}` to initialize a Claim Wire workspace.",
            dir.display(),
            s.cyan_bold("tip:"),
            s.bold(&format!("cw -C {} init", dir.display()))
        );
    }

    let cwd = env::current_dir()?;
    let mut cur: Option<&Path> = Some(cwd.as_path());
    while let Some(dir) = cur {
        if dir.join(".claimwire").is_dir() {
            return Ok(dir.to_path_buf());
        }
        cur = dir.parent();
    }

    if subcmd == "scan" || subcmd == "diff" {
        return Ok(cwd);
    }

    anyhow::bail!(
        "No `.claimwire/` workspace found in `{}` (or any parent directory).\n  {} Run `{}` to scaffold a new workspace, or `{}` to inspect existing Markdown files.",
        cwd.display(),
        s.cyan_bold("tip:"),
        s.green_bold("cw init"),
        s.bold("cw scan")
    )
}

fn cmd_check(workspace_root: &Path, args: &[String], s: &Styler, quiet: bool) -> Result<u8> {
    let mut format = "human".to_string();
    let mut deny_warnings = false;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" | "-f" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing argument for `--format`");
                }
                format = args[i + 1].clone();
                i += 2;
            }
            "--deny-warnings" | "-D" => {
                deny_warnings = true;
                i += 1;
            }
            other if other.starts_with('-') => {
                anyhow::bail!("Unknown option `{other}` for `cw check` (run `cw check --help`)");
            }
            file => {
                files.push(file.to_string());
                i += 1;
            }
        }
    }

    let explicit = if files.is_empty() {
        None
    } else {
        Some(files.as_slice())
    };
    let report = if format == "human" && !quiet {
        s.animate_stage(
            1,
            1,
            "Verifying syntax, dimensional algebra, producer freshness & Markdown bindings...",
            || run_check_files(workspace_root, explicit),
        )?
    } else {
        run_check_files(workspace_root, explicit)?
    };

    if !quiet || report.has_errors(deny_warnings) || format != "human" {
        print_diagnostics(
            &report.diagnostics,
            &format,
            Some((report.failed_locations, report.total_locations)),
            s,
        )?;
    }

    if report.has_errors(deny_warnings) {
        Ok(1)
    } else {
        Ok(0)
    }
}

fn cmd_sync(workspace_root: &Path, args: &[String], s: &Styler, quiet: bool) -> Result<u8> {
    let mut exact = false;
    let mut force = false;
    let mut ack_context = false;
    let mut deny_warnings = false;
    let mut dry_run = false;
    let mut show_diff = false;

    for arg in args {
        match arg.as_str() {
            "--exact" => exact = true,
            "--force" => force = true,
            "--ack-context" => ack_context = true,
            "--deny-warnings" | "-D" => deny_warnings = true,
            "--dry-run" | "-n" => dry_run = true,
            "--diff" | "-d" => show_diff = true,
            other => {
                anyhow::bail!("Unknown option `{other}` for `cw sync` (run `cw sync --help`)")
            }
        }
    }

    let report = s.animate_stage(
        1,
        1,
        if dry_run {
            "Evaluating claims & computing dry-run Markdown patches..."
        } else {
            "Evaluating claims & transactionally patching owned Markdown locations..."
        },
        || run_sync_options(workspace_root, exact, force, ack_context, dry_run),
    )?;
    let has_err = report.diagnostics.iter().any(|d| {
        d.severity == Severity::Error || (deny_warnings && d.severity == Severity::Warning)
    });

    if !report.diagnostics.is_empty() {
        for d in &report.diagnostics {
            print!("{}", style_diagnostic(&d.format_human(), s));
        }
    }
    if has_err {
        return Ok(1);
    }

    if !quiet {
        let patch_labels: Vec<String> = report
            .patched_items
            .iter()
            .map(|item| format!("{}:{} ({})", item.file, item.line, item.claim_summary))
            .collect();
        s.animate_sweep("Patching location", &patch_labels, 28);

        let action_verb = if dry_run {
            if s.is_colored() {
                s.badge_warn("WOULD PATCH")
            } else {
                "would patch".to_string()
            }
        } else if s.is_colored() {
            s.badge_ok("PATCHED")
        } else {
            "patched".to_string()
        };
        let arrow = if s.is_colored() {
            s.magenta_bold("──▶")
        } else {
            "→".to_string()
        };
        for item in &report.patched_items {
            let coord = format!("{}:{}", item.file, item.line);
            println!(
                "{action_verb}  {:<27} {}   {} {arrow} {}",
                s.cyan_bold(&coord),
                s.bold(&item.claim_summary),
                s.red(&item.old_text),
                s.green_bold(&item.new_text)
            );
        }

        if show_diff && !report.file_diffs.is_empty() {
            println!();
            for (rel_path, (before, after)) in &report.file_diffs {
                print!("{}", render_unified_diff(rel_path, before, after, s));
            }
        }

        let file_word = if report.files_written == 1 {
            "file"
        } else {
            "files"
        };
        if dry_run {
            println!(
                "{} {} {file_word} would be written · {}",
                if s.is_colored() {
                    s.badge_warn("DRY-RUN")
                } else {
                    "dry-run:".to_string()
                },
                s.yellow_bold(&report.files_written.to_string()),
                s.dim("no files or lockfile modified")
            );
        } else if report.lockfile_updated {
            println!(
                "{} {} {file_word} written · {}",
                if s.is_colored() {
                    format!("{} ", s.badge_ok("✓ SYNCED"))
                } else {
                    String::new()
                },
                s.green_bold(&report.files_written.to_string()),
                s.cyan(".claimwire/evidence.lock.json updated")
            );
        } else {
            println!(
                "{}0 files written · up to date",
                if s.is_colored() {
                    format!("{} ", s.badge_ok("✓ UP TO DATE"))
                } else {
                    String::new()
                }
            );
        }
    }

    Ok(0)
}

fn cmd_adopt(workspace_root: &Path, args: &[String], s: &Styler) -> Result<u8> {
    let explicit = if args.is_empty() { None } else { Some(args) };
    let report = s.animate_stage(
        1,
        1,
        "Reverse-parsing Markdown through bidirectional formats & initializing lockfile...",
        || run_adopt(workspace_root, explicit),
    )?;
    for d in &report.diagnostics {
        print!("{}", style_diagnostic(&d.format_human(), s));
    }
    if report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Ok(1);
    }
    let badge = if s.is_colored() {
        s.badge_ok("✓ ADOPTED")
    } else {
        "adopted".to_string()
    };
    println!(
        "{badge} {} locations · {} (0 Markdown bytes modified)",
        s.green_bold(&report.adopted_locations.to_string()),
        s.cyan(".claimwire/evidence.lock.json initialized")
    );
    Ok(0)
}

fn cmd_capture(workspace_root: &Path, args: &[String], s: &Styler) -> Result<u8> {
    let mut producer: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut cmd_vec: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--producer" | "-p" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing producer id after `--producer`");
                }
                producer = Some(args[i + 1].clone());
                i += 2;
            }
            "--run-id" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing run id after `--run-id`");
                }
                run_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--" => {
                cmd_vec.extend_from_slice(&args[i + 1..]);
                break;
            }
            other => {
                anyhow::bail!("Unexpected argument `{other}` in `cw capture` (run `cw capture --help`)")
            }
        }
    }

    let Some(producer_id) = producer else {
        anyhow::bail!("`cw capture` requires `--producer <id>` (run `cw capture --help`)");
    };

    let started = Instant::now();
    let claims_src = fs::read_to_string(workspace_root.join(".claimwire/claims.yaml"))?;
    let evidence_src = fs::read_to_string(workspace_root.join(".claimwire/evidence.yaml"))?;
    let (claims_cfg, _) = from_yaml_str::<ClaimsConfig>(&claims_src)?;
    let (evidence_cfg, _) = from_yaml_str::<EvidenceConfig>(&evidence_src)?;

    let spinner = if s.is_animated() {
        Some(LiveSpinner::start(
            format!(
                "Executing producer `{}` and ingesting `@cw:record` frames...",
                s.cyan_bold(&producer_id)
            ),
            Duration::from_millis(32),
        ))
    } else {
        None
    };

    let capture_res = execute_capture(
        workspace_root,
        &producer_id,
        &cmd_vec,
        run_id.as_deref(),
        &evidence_cfg,
        &claims_cfg,
    );
    if let Some(sp) = spinner {
        if started.elapsed() < Duration::from_millis(90) {
            std::thread::sleep(Duration::from_millis(90) - started.elapsed());
        }
        sp.stop_and_clear();
    }
    let (manifest, manifest_rel) = capture_res?;

    let fam_counts: Vec<String> = manifest
        .families
        .iter()
        .map(|(k, v)| format!("{} {k}", v.len()))
        .collect();
    let total_records: usize = manifest.families.values().map(|v| v.len()).sum::<usize>()
        + manifest.scalars.len();
    let summary = if fam_counts.is_empty() {
        format!("{} scalar metrics", manifest.scalars.len())
    } else {
        fam_counts.join(", ")
    };
    let elapsed_ms = started.elapsed().as_millis();
    let badge = if s.is_colored() {
        s.badge_ok("✓ CAPTURED")
    } else {
        "captured".to_string()
    };
    println!(
        "{badge} {} records ({summary}) → {}  {}",
        s.green_bold(&total_records.to_string()),
        s.cyan_bold(&manifest_rel),
        s.dim(&format!("(@current · {elapsed_ms}ms)"))
    );
    Ok(0)
}

fn cmd_impact(workspace_root: &Path, args: &[String], s: &Styler) -> Result<u8> {
    let mut paths = Vec::new();
    let mut format = "human".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--git-diff" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing revision after `--git-diff`");
                }
                let rev = &args[i + 1];
                let out = Command::new("git")
                    .args(["diff", "--name-only", rev])
                    .current_dir(workspace_root)
                    .output()?;
                if out.status.success() {
                    for line in String::from_utf8_lossy(&out.stdout).lines() {
                        let t = line.trim();
                        if !t.is_empty() {
                            paths.push(t.to_string());
                        }
                    }
                }
                i += 2;
            }
            "--format" | "-f" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing format after `--format` (expected human|json)");
                }
                format = args[i + 1].clone();
                i += 2;
            }
            other if other.starts_with('-') => {
                anyhow::bail!("Unknown option `{other}` for `cw impact` (run `cw impact --help`)");
            }
            path => {
                paths.push(path.to_string());
                i += 1;
            }
        }
    }

    let report = if format == "human" {
        s.animate_stage(
            1,
            1,
            "Tracing forward invalidation graph from modified inputs...",
            || run_impact(workspace_root, &paths),
        )?
    } else {
        run_impact(workspace_root, &paths)?
    };
    if format == "json" {
        let json = serde_json::json!({
            "modified_files": report.modified_files,
            "stale_producers": report.stale_producers,
            "affected_families": report.affected_families,
            "affected_scalar_claims": report.affected_scalar_claims,
            "affected_assertions": report.affected_assertions,
            "affected_locations": report.affected_locations.iter().map(|l| serde_json::json!({
                "file": l.file,
                "line": l.line,
                "col": l.col,
                "description": l.description,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(0);
    }

    if s.is_colored() {
        println!(
            "{}",
            s.banner("Forward Invalidation & Impact Trace", &workspace_root.display().to_string())
        );
    }
    println!(
        "{} {:?}",
        s.cyan_bold("╭─▸ Modified inputs:"),
        report.modified_files
    );
    println!(
        "{} {:?}",
        s.yellow_bold("├─▸ Stale producers:"),
        report.stale_producers
    );
    println!(
        "{} {:?}",
        s.magenta_bold("├─▸ Affected claim families:"),
        report.affected_families
    );
    if !report.affected_scalar_claims.is_empty() {
        println!(
            "{} {:?}",
            s.cyan_bold("├─▸ Affected scalar claims:"),
            report.affected_scalar_claims
        );
    }
    if !report.affected_assertions.is_empty() {
        println!(
            "{} {:?}",
            s.red_bold("├─▸ Affected assertions:"),
            report.affected_assertions
        );
    }
    println!(
        "{}",
        s.brand_gradient(&format!(
            "╰─▸ Affected Markdown locations ({}):",
            report.affected_locations.len()
        ))
    );
    for loc in &report.affected_locations {
        println!(
            "    {} {:<28} {}",
            s.magenta_bold("•"),
            s.cyan_bold(&format!("{}:{}:{}", loc.file, loc.line, loc.col)),
            s.dim(&format!("({})", loc.description))
        );
    }
    Ok(0)
}

fn cmd_explain(workspace_root: &Path, args: &[String], s: &Styler) -> Result<u8> {
    if args.is_empty() {
        anyhow::bail!(
            "`cw explain` requires a `<claim>`, `<file:line>`, or `<CWxxxx>` argument (e.g. `cw explain CW1008`)"
        );
    }
    if let Some(doc) = explain_diagnostic_code(&args[0], s) {
        print!("{doc}");
        return Ok(0);
    }
    let tree = s.animate_stage(
        1,
        1,
        "Tracing backward provenance DAG to run manifests...",
        || run_explain(workspace_root, &args[0]),
    )?;
    if !s.is_colored() {
        print!("{tree}");
    } else {
        println!(
            "{}",
            s.banner(&format!("Provenance Trace · {}", args[0]), "")
        );
        for (idx, line) in tree.lines().enumerate() {
            if idx == 0 {
                println!("  {} {}", s.badge_info("TARGET"), s.brand_gradient(line));
            } else if let Some((prefix, rest)) = line.split_once(": ") {
                println!(
                    "  {} {}",
                    s.cyan_bold(&format!("{prefix}:")),
                    s.bold(rest)
                );
            } else {
                println!("  {}", s.green(line));
            }
        }
    }
    Ok(0)
}

fn cmd_diff(workspace_root: &Path, args: &[String], s: &Styler) -> Result<u8> {
    let mut format = "human".to_string();
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" | "-f" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing argument after `--format`");
                }
                format = args[i + 1].clone();
                i += 2;
            }
            arg => {
                pos.push(arg.to_string());
                i += 1;
            }
        }
    }

    if pos.is_empty() {
        anyhow::bail!("`cw diff` requires `<rev1>..<rev2>` or `<dir1> <dir2>`");
    }
    let (old_snap, new_snap) = if pos.len() == 2 {
        (
            RevisionSnapshot::from_dir(Path::new(&pos[0])),
            RevisionSnapshot::from_dir(Path::new(&pos[1])),
        )
    } else if let Some((r1, r2)) = pos[0].split_once("..") {
        (
            RevisionSnapshot::from_git_rev(workspace_root, r1)?,
            RevisionSnapshot::from_git_rev(workspace_root, r2)?,
        )
    } else {
        anyhow::bail!("Expected `<rev1>..<rev2>` or `<dir1> <dir2>` in `cw diff`");
    };

    let entries = compare_snapshots(&old_snap, &new_snap);
    if format == "json" {
        let json: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "category": format!("{}", e.category),
                    "subject": e.subject,
                    "detail": e.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(0);
    }

    if entries.is_empty() {
        println!("{}", s.dim("No claim changes detected."));
    } else {
        for e in &entries {
            let cat_str = format!("{}", e.category);
            let badge = if s.is_colored() {
                match cat_str.as_str() {
                    "value_changed" => s.badge_warn("VALUE CHANGED"),
                    "definition_changed" => s.badge_info("DEFINITION CHANGED"),
                    "tolerance_relaxed" | "assertion_weakened" => {
                        s.badge_err(&cat_str.to_ascii_uppercase())
                    }
                    "evidence_refreshed" => s.badge_ok("EVIDENCE REFRESHED"),
                    other => s.badge_info(&other.to_ascii_uppercase()),
                }
            } else {
                format!("[{cat_str}]")
            };
            println!("{badge} {} — {}", s.cyan_bold(&e.subject), e.detail);
        }
    }
    Ok(0)
}

fn cmd_merge_driver(args: &[String]) -> Result<u8> {
    if args.len() < 3 {
        anyhow::bail!("Usage: `cw merge-driver <base:%O> <ours:%A> <theirs:%B>`");
    }
    let ok = run_merge_driver(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
    )?;
    if ok {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn print_diagnostics(
    diagnostics: &[Diagnostic],
    format: &str,
    summary_counts: Option<(usize, usize)>,
    s: &Styler,
) -> Result<()> {
    match format {
        "human" => {
            for d in diagnostics {
                print!("{}", style_diagnostic(&d.format_human(), s));
                println!();
            }
            if let Some((failed, total)) = summary_counts {
                let err_count = diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count();
                if err_count > 0 {
                    let count_to_report = failed.max(err_count);
                    let passed = total.saturating_sub(count_to_report);
                    if s.is_colored() {
                        println!(
                            "{} {} {} of {} registered locations failed verification",
                            s.badge_err("✗ FAILED"),
                            s.progress_bar(passed, total.max(1), 16),
                            s.red_bold(&count_to_report.to_string()),
                            s.bold(&total.to_string())
                        );
                    } else {
                        println!(
                            "error: {count_to_report} of {total} registered locations failed verification"
                        );
                    }
                } else {
                    if s.is_colored() {
                        println!(
                            "{} {} {} of {} registered locations verified · 0 errors",
                            s.badge_ok("✓ VERIFIED"),
                            s.progress_bar(total, total.max(1), 16),
                            s.green_bold(&total.to_string()),
                            s.bold(&total.to_string())
                        );
                    } else {
                        println!(
                            "ok: {total} of {total} registered locations verified · 0 errors"
                        );
                    }
                    println!(
                        "{} Unregistered prose outside Claim Wire spans, tables, and blocks was not assessed.",
                        s.dim("note:")
                    );
                }
            }
        }
        "json" => {
            println!("{}", serde_json::to_string_pretty(diagnostics)?);
        }
        "github" => {
            for d in diagnostics {
                println!("{}", d.format_github());
            }
        }
        "sarif" => {
            println!("{}", format_diagnostics_sarif(diagnostics));
        }
        other => anyhow::bail!(
            "Unsupported diagnostic format `{other}` (expected human|json|github|sarif)"
        ),
    }
    Ok(())
}

fn suggest_subcommand(unknown: &str) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for &candidate in ALL_SUBCOMMANDS {
        let dist = edit_distance(unknown, candidate);
        if dist <= 3 {
            if let Some((_, best_d)) = best {
                if dist < best_d {
                    best = Some((candidate, dist));
                }
            } else {
                best = Some((candidate, dist));
            }
        }
    }
    best.map(|(c, _)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut dp = vec![vec![0usize; b_chars.len() + 1]; a_chars.len() + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b_chars.len() {
        dp[0][j] = j;
    }
    for i in 1..=a_chars.len() {
        for j in 1..=b_chars.len() {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[a_chars.len()][b_chars.len()]
}

fn print_help(s: &Styler) {
    println!(
        "{}\n",
        s.banner(
            &format!("Claim Wire (cw) v{}", env!("CARGO_PKG_VERSION")),
            "Incremental compiler for evidence-backed documentation"
        )
    );
    println!("{}", s.brand_gradient("◈ USAGE"));
    println!("    cw [GLOBAL OPTIONS] <COMMAND> [ARGS...]\n");

    println!("{}", s.brand_gradient("◈ ONBOARDING & DISCOVERY"));
    println!(
        "    {:<38} Scaffold `.claimwire/` config, runs dir, and optional Git merge driver",
        s.cyan_bold("init [--force] [--git]")
    );
    println!(
        "    {:<38} Scan Markdown for wired/unwired tables, code blocks, and CW1008 issues",
        s.cyan_bold("scan [<file...>]")
    );
    println!(
        "    {:<38} Bootstrap evidence.lock.json from existing Markdown with zero byte changes",
        s.cyan_bold("adopt [<file...>]")
    );
    println!(
        "    {:<38} Interactive dashboard of producers, claims, assertions, and Markdown bindings",
        s.cyan_bold("status")
    );
    println!(
        "    {:<38} List declared producers, families, claims, assertions, or bindings",
        s.cyan_bold("list [producers|claims|assertions|...]")
    );
    println!();

    println!("{}", s.brand_gradient("◈ VERIFICATION & SYNCHRONIZATION"));
    println!(
        "    {:<38} Read-only verification of syntax, types, freshness, assertions, and lockfile",
        s.green_bold("check [--format human|json|github|sarif]")
    );
    println!(
        "    {:<38} Re-evaluate claims and transactionally patch owned spans, cells, and blocks",
        s.green_bold("sync [--dry-run] [--diff] [--exact]")
    );
    println!(
        "    {:<38} Run harness, ingest @cw:record frames, and write an immutable run manifest",
        s.green_bold("capture --producer <id> -- <cmd...>")
    );
    println!();

    println!("{}", s.brand_gradient("◈ PROVENANCE, IMPACT & GIT GOVERNANCE"));
    println!(
        "    {:<38} Trace changed source files forward to affected producers, claims, and docs",
        s.magenta_bold("impact [<path...> | --git-diff <rev>]")
    );
    println!(
        "    {:<38} Print provenance tree for a claim/location or explain an error code (CW1008)",
        s.magenta_bold("explain <claim | file:line | CWxxxx>")
    );
    println!(
        "    {:<38} Classify semantic claim changes across Git revisions or directories",
        s.magenta_bold("diff <rev1>..<rev2>")
    );
    println!(
        "    {:<38} Structural 3-way Git merge driver for evidence.lock.json and wired Markdown",
        s.magenta_bold("merge-driver %O %A %B")
    );
    println!(
        "    {:<38} Generate shell completion script for bash, zsh, or fish",
        s.magenta_bold("completions <bash|zsh|fish>")
    );
    println!();

    println!("{}", s.brand_gradient("◈ GLOBAL OPTIONS"));
    println!("    -C, --dir <PATH>               Run as if cw was started in <PATH> (otherwise walks up to find .claimwire/)");
    println!("    --color <auto|always|never>    Control TrueColor/ANSI terminal styling (respects NO_COLOR)");
    println!("    --animate / --no-anim          Force-enable or disable terminal animations & spinners");
    println!("    -q, --quiet                    Suppress informational output on check/sync");
    println!("    -h, --help                     Print help (use `cw <command> --help` for detailed subcommand help)");
    println!("    -V, --version                  Print version");
}

fn print_subcommand_help(subcmd: &str, s: &Styler) -> Result<u8> {
    match subcmd {
        "init" => {
            println!(
                "{} — Initialize a new Claim Wire workspace\n\n{}\n    cw init [--force] [--git]\n\n{}\n    --force    Overwrite existing `.claimwire/claims.yaml` and `evidence.yaml`\n    --git      Configure `.gitattributes` and local `git config merge.claimwire.driver`\n",
                s.bold("cw init"),
                s.bold("USAGE:"),
                s.bold("OPTIONS:")
            );
        }
        "scan" => {
            println!(
                "{} — Scan Markdown files for wired & unwired tables, blocks, and spans\n\n{}\n    cw scan [<file...>]\n\n{}\n    Inspects Markdown documents in the repository, reporting which GFM tables\n    and fenced code blocks are already wired and which are candidates for\n    `cw:table` or `cw:block` adoption, plus any `CW1008` unescaped pipe issues.\n",
                s.bold("cw scan"),
                s.bold("USAGE:"),
                s.bold("DESCRIPTION:")
            );
        }
        "status" => {
            println!(
                "{} — Workspace health dashboard\n\n{}\n    cw status\n\n{}\n    Displays producer input-hash freshness, latest run manifests, evaluated\n    scalar claims, assertion pass/fail status, and per-file Markdown sync state.\n",
                s.bold("cw status"),
                s.bold("USAGE:"),
                s.bold("DESCRIPTION:")
            );
        }
        "list" | "ls" => {
            println!(
                "{} — Inspect declared producers, families, claims, assertions, and locations\n\n{}\n    cw list [all | producers | families | claims | assertions | locations]\n",
                s.bold("cw list"),
                s.bold("USAGE:")
            );
        }
        "check" => {
            println!(
                "{} — Read-only verification of syntax, types, freshness, assertions, and lockfile\n\n{}\n    cw check [OPTIONS] [<file...>]\n\n{}\n    -f, --format <FMT>     Output format: `human` (default), `json`, `github`, `sarif`\n    -D, --deny-warnings    Treat warnings (e.g., CW4002 unacknowledged context) as exit-code-1 errors\n",
                s.bold("cw check"),
                s.bold("USAGE:"),
                s.bold("OPTIONS:")
            );
        }
        "sync" => {
            println!(
                "{} — Re-evaluate claims and transactionally patch owned spans, cells, and blocks\n\n{}\n    cw sync [OPTIONS]\n\n{}\n    -n, --dry-run          Preview all Markdown patches without modifying files or lockfile\n    -d, --diff             Display a unified diff of all patched Markdown lines\n    --exact                Require rendered text to match canonical formatted string even within tolerance\n    --force                Overwrite target locations even on manual conflict (CW3004)\n    --ack-context          Record current adjacent prose context hashes in evidence.lock.json (clears CW4002)\n    -D, --deny-warnings    Fail with exit code 1 if any warnings remain\n",
                s.bold("cw sync"),
                s.bold("USAGE:"),
                s.bold("OPTIONS:")
            );
        }
        "adopt" => {
            println!(
                "{} — Bootstrap evidence.lock.json from existing wired Markdown without modifying prose\n\n{}\n    cw adopt [<file...>]\n",
                s.bold("cw adopt"),
                s.bold("USAGE:")
            );
        }
        "capture" => {
            println!(
                "{} — Execute a producer harness, ingest `@cw:record` frames, and write a run manifest\n\n{}\n    cw capture --producer <ID> [--run-id <RUN_ID>] -- <COMMAND...>\n\n{}\n    -p, --producer <ID>    Producer ID declared in `.claimwire/evidence.yaml`\n    --run-id <RUN_ID>      Deterministic run identifier (defaults to timestamp + git SHA)\n\n{}\n    cw capture --producer bench_throughput -- cargo bench --bench magnetar\n",
                s.bold("cw capture"),
                s.bold("USAGE:"),
                s.bold("OPTIONS:"),
                s.bold("EXAMPLE:")
            );
        }
        "impact" => {
            println!(
                "{} — Trace modified source files forward to stale producers, claims, and Markdown locations\n\n{}\n    cw impact [--format human|json] [<path...> | --git-diff <rev>]\n\n{}\n    cw impact src/engine.rs\n    cw impact --git-diff origin/main\n",
                s.bold("cw impact"),
                s.bold("USAGE:"),
                s.bold("EXAMPLES:")
            );
        }
        "explain" => {
            println!(
                "{} — Print the full provenance tree for a claim/location or explain a diagnostic code\n\n{}\n    cw explain <claim_ref | file:line | CWxxxx>\n\n{}\n    cw explain speedup_vs_sqlite_64b\n    cw explain README.md:27\n    cw explain CW1008\n",
                s.bold("cw explain"),
                s.bold("USAGE:"),
                s.bold("EXAMPLES:")
            );
        }
        "diff" => {
            println!(
                "{} — Classify semantic claim changes across Git revisions or directories\n\n{}\n    cw diff [--format human|json] <rev1>..<rev2>\n    cw diff [--format human|json] <dir1> <dir2>\n",
                s.bold("cw diff"),
                s.bold("USAGE:")
            );
        }
        "merge-driver" => {
            println!(
                "{} — Structural 3-way Git merge driver for evidence.lock.json and wired Markdown\n\n{}\n    cw merge-driver <base:%O> <ours:%A> <theirs:%B>\n",
                s.bold("cw merge-driver"),
                s.bold("USAGE:")
            );
        }
        "completions" => {
            println!(
                "{} — Generate shell completion script\n\n{}\n    cw completions <bash|zsh|fish>\n",
                s.bold("cw completions"),
                s.bold("USAGE:")
            );
        }
        other => {
            anyhow::bail!("Unknown subcommand `{other}` (run `cw --help`)");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subcommand_typo_suggestions() {
        assert_eq!(suggest_subcommand("chck"), Some("check"));
        assert_eq!(suggest_subcommand("staus"), Some("status"));
        assert_eq!(suggest_subcommand("snc"), Some("sync"));
        assert_eq!(suggest_subcommand("explan"), Some("explain"));
        assert_eq!(suggest_subcommand("completely_unrelated_xyz"), None);
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let ty = entry.file_type().unwrap();
            let dst_path = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_recursive(&entry.path(), &dst_path);
            } else {
                fs::copy(entry.path(), dst_path).unwrap();
            }
        }
    }

    #[test]
    fn test_dispatch_cli_ux_commands_on_magnetar() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let magnetar = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/fixtures/magnetar");
        let tmp = env::temp_dir().join(format!("cw_cli_ux_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        copy_dir_recursive(&magnetar, &tmp);
        let dir_str = tmp.to_string_lossy().to_string();

        // adopt first so evidence.lock.json is initialized
        let code = dispatch(&[
            "--color".into(),
            "never".into(),
            "-C".into(),
            dir_str.clone(),
            "adopt".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);

        // status
        let code = dispatch(&[
            "--color".into(),
            "never".into(),
            "-C".into(),
            dir_str.clone(),
            "status".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);

        // scan
        let code = dispatch(&[
            "--color".into(),
            "never".into(),
            "-C".into(),
            dir_str.clone(),
            "scan".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);

        // list
        for topic in ["all", "producers", "families", "claims", "assertions", "locations"] {
            let code = dispatch(&[
                "--color".into(),
                "never".into(),
                "-C".into(),
                dir_str.clone(),
                "list".into(),
                topic.into(),
            ])
            .unwrap();
            assert_eq!(code, 0);
        }

        // explain diagnostic code
        let code = dispatch(&[
            "--color".into(),
            "never".into(),
            "explain".into(),
            "CW1008".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);

        // sync --dry-run --diff
        let code = dispatch(&[
            "--color".into(),
            "never".into(),
            "-C".into(),
            dir_str,
            "sync".into(),
            "--dry-run".into(),
            "--diff".into(),
        ])
        .unwrap();
        assert_eq!(code, 0);

        // init in a fresh temp directory
        let init_tmp = env::temp_dir().join(format!("cw_cli_init_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&init_tmp);
        fs::create_dir_all(&init_tmp).unwrap();
        let init_code = dispatch(&[
            "--color".into(),
            "never".into(),
            "-C".into(),
            init_tmp.to_string_lossy().to_string(),
            "init".into(),
            "--git".into(),
        ])
        .unwrap();
        assert_eq!(init_code, 0);
        assert!(init_tmp.join(".claimwire/claims.yaml").is_file());
        assert!(init_tmp.join(".claimwire/evidence.yaml").is_file());
        assert!(init_tmp.join(".gitattributes").is_file());

        let _ = fs::remove_dir_all(&tmp);
        let _ = fs::remove_dir_all(&init_tmp);
    }
}
