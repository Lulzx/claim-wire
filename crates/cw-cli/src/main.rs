//! `cw` command-line compiler for Claim Wire (`spec.md` §9.2).

use anyhow::Result;
use cw_engine::{
    RevisionSnapshot, compare_snapshots, run_adopt, run_check_files, run_explain, run_impact,
    run_merge_driver, run_sync,
};
use cw_evidence::{EvidenceConfig, execute_capture};
use cw_ir::ClaimsConfig;
use cw_syntax::{Diagnostic, Severity, format_diagnostics_sarif, from_yaml_str};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(args: &[String]) -> Result<u8> {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print_help();
        return Ok(0);
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("cw {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    // Extract global `-C <dir>` / `--dir <dir>` if present
    let (workspace_root, filtered_args) = extract_workspace_dir(args)?;
    if filtered_args.is_empty() {
        print_help();
        return Ok(2);
    }

    let subcmd = filtered_args[0].as_str();
    let sub_args = &filtered_args[1..];

    match subcmd {
        "check" => cmd_check(&workspace_root, sub_args),
        "sync" => cmd_sync(&workspace_root, sub_args),
        "adopt" => cmd_adopt(&workspace_root, sub_args),
        "capture" => cmd_capture(&workspace_root, sub_args),
        "impact" => cmd_impact(&workspace_root, sub_args),
        "explain" => cmd_explain(&workspace_root, sub_args),
        "diff" => cmd_diff(&workspace_root, sub_args),
        "merge-driver" => cmd_merge_driver(sub_args),
        other => {
            eprintln!("error: unknown subcommand `{other}`\n");
            print_help();
            Ok(2)
        }
    }
}

fn extract_workspace_dir(args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let mut dir = env::current_dir()?;
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-C" || args[i] == "--dir" {
            if i + 1 >= args.len() {
                anyhow::bail!("Missing directory argument after `{}`", args[i]);
            }
            dir = PathBuf::from(&args[i + 1]);
            i += 2;
        } else {
            out.push(args[i].clone());
            i += 1;
        }
    }
    Ok((dir, out))
}

fn cmd_check(workspace_root: &Path, args: &[String]) -> Result<u8> {
    let mut format = "human".to_string();
    let mut deny_warnings = false;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("Missing argument for `--format`");
                }
                format = args[i + 1].clone();
                i += 2;
            }
            "--deny-warnings" => {
                deny_warnings = true;
                i += 1;
            }
            other if other.starts_with('-') => {
                anyhow::bail!("Unknown option `{other}` for `cw check`");
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
    let report = run_check_files(workspace_root, explicit)?;
    print_diagnostics(
        &report.diagnostics,
        &format,
        Some((report.failed_locations, report.total_locations)),
    )?;

    if report.has_errors(deny_warnings) {
        Ok(1)
    } else {
        Ok(0)
    }
}

fn cmd_sync(workspace_root: &Path, args: &[String]) -> Result<u8> {
    let mut exact = false;
    let mut force = false;
    let mut ack_context = false;
    let mut deny_warnings = false;

    for arg in args {
        match arg.as_str() {
            "--exact" => exact = true,
            "--force" => force = true,
            "--ack-context" => ack_context = true,
            "--deny-warnings" => deny_warnings = true,
            other => anyhow::bail!("Unknown option `{other}` for `cw sync`"),
        }
    }

    let report = run_sync(workspace_root, exact, force, ack_context)?;
    let has_err = report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error || (deny_warnings && d.severity == Severity::Warning));

    if !report.diagnostics.is_empty() {
        for d in &report.diagnostics {
            print!("{}", d.format_human());
        }
    }
    if has_err {
        return Ok(1);
    }

    for item in &report.patched_items {
        let coord = format!("{}:{}", item.file, item.line);
        println!(
            "patched  {coord:<27} {}   {} → {}",
            item.claim_summary, item.old_text, item.new_text
        );
    }

    let file_word = if report.files_written == 1 {
        "file"
    } else {
        "files"
    };
    if report.lockfile_updated {
        println!(
            "{} {file_word} written · .claimwire/evidence.lock.json updated",
            report.files_written
        );
    } else {
        println!("0 files written · up to date");
    }

    Ok(0)
}

fn cmd_adopt(workspace_root: &Path, args: &[String]) -> Result<u8> {
    let explicit = if args.is_empty() { None } else { Some(args) };
    let report = run_adopt(workspace_root, explicit)?;
    for d in &report.diagnostics {
        print!("{}", d.format_human());
    }
    if report.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Ok(1);
    }
    println!(
        "adopted {} locations · .claimwire/evidence.lock.json initialized (0 Markdown bytes modified)",
        report.adopted_locations
    );
    Ok(0)
}

fn cmd_capture(workspace_root: &Path, args: &[String]) -> Result<u8> {
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
            other => anyhow::bail!("Unexpected argument `{other}` in `cw capture`"),
        }
    }

    let Some(producer_id) = producer else {
        anyhow::bail!("`cw capture` requires `--producer <id>`");
    };

    let claims_src = fs::read_to_string(workspace_root.join(".claimwire/claims.yaml"))?;
    let evidence_src = fs::read_to_string(workspace_root.join(".claimwire/evidence.yaml"))?;
    let (claims_cfg, _) = from_yaml_str::<ClaimsConfig>(&claims_src)?;
    let (evidence_cfg, _) = from_yaml_str::<EvidenceConfig>(&evidence_src)?;

    let (manifest, manifest_rel) = execute_capture(
        workspace_root,
        &producer_id,
        &cmd_vec,
        run_id.as_deref(),
        &evidence_cfg,
        &claims_cfg,
    )?;

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
    println!(
        "captured {total_records} records ({summary}) → {manifest_rel}  (@current)"
    );
    Ok(0)
}

fn cmd_impact(workspace_root: &Path, args: &[String]) -> Result<u8> {
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--git-diff" {
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
        } else {
            paths.push(args[i].clone());
            i += 1;
        }
    }

    let report = run_impact(workspace_root, &paths)?;
    println!("Modified inputs: {:?}", report.modified_files);
    println!("Stale producers: {:?}", report.stale_producers);
    println!("Affected claim families: {:?}", report.affected_families);
    if !report.affected_scalar_claims.is_empty() {
        println!("Affected scalar claims: {:?}", report.affected_scalar_claims);
    }
    if !report.affected_assertions.is_empty() {
        println!("Affected assertions: {:?}", report.affected_assertions);
    }
    println!(
        "Affected Markdown locations ({}):",
        report.affected_locations.len()
    );
    for loc in &report.affected_locations {
        println!(
            "  {}:{}:{}  ({})",
            loc.file, loc.line, loc.col, loc.description
        );
    }
    Ok(0)
}

fn cmd_explain(workspace_root: &Path, args: &[String]) -> Result<u8> {
    if args.is_empty() {
        anyhow::bail!("`cw explain` requires a `<claim>` or `<file:line>` argument");
    }
    let tree = run_explain(workspace_root, &args[0])?;
    print!("{tree}");
    Ok(0)
}

fn cmd_diff(workspace_root: &Path, args: &[String]) -> Result<u8> {
    if args.is_empty() {
        anyhow::bail!("`cw diff` requires `<rev1>..<rev2>` or `<dir1> <dir2>`");
    }
    let (old_snap, new_snap) = if args.len() == 2 {
        (
            RevisionSnapshot::from_dir(Path::new(&args[0])),
            RevisionSnapshot::from_dir(Path::new(&args[1])),
        )
    } else if let Some((r1, r2)) = args[0].split_once("..") {
        (
            RevisionSnapshot::from_git_rev(workspace_root, r1)?,
            RevisionSnapshot::from_git_rev(workspace_root, r2)?,
        )
    } else {
        anyhow::bail!("Expected `<rev1>..<rev2>` in `cw diff`");
    };

    let entries = compare_snapshots(&old_snap, &new_snap);
    if entries.is_empty() {
        println!("No claim changes detected.");
    } else {
        for e in &entries {
            println!("[{}] {} — {}", e.category, e.subject, e.detail);
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
) -> Result<()> {
    match format {
        "human" => {
            for d in diagnostics {
                print!("{}", d.format_human());
                println!();
            }
            if let Some((failed, total)) = summary_counts {
                let err_count = diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count();
                if err_count > 0 {
                    let count_to_report = failed.max(err_count);
                    println!(
                        "error: {count_to_report} of {total} registered locations failed verification"
                    );
                } else {
                    println!(
                        "ok: {total} of {total} registered locations verified · 0 errors"
                    );
                    println!(
                        "note: Unregistered prose outside Claim Wire spans, tables, and blocks was not assessed."
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
        other => anyhow::bail!("Unsupported diagnostic format `{other}` (expected human|json|github|sarif)"),
    }
    Ok(())
}

fn print_help() {
    println!(
        "Claim Wire (cw) {} — An incremental compiler for evidence-backed documentation

USAGE:
    cw [-C <dir>] <COMMAND> [OPTIONS]

COMMANDS:
    check [--format human|json|github|sarif]   Read-only verification of syntax, types, freshness, assertions, and lockfile
    sync [--exact] [--force] [--ack-context]   Re-evaluate claims and transactionally patch owned spans, cells, and blocks
    capture --producer <id> -- <cmd...>        Run harness, ingest @cw:record frames, and write an immutable run manifest
    impact [<path...> | --git-diff <rev>]      Trace changed source files forward to affected producers, claims, and locations
    explain <claim | file:line>                Print the full provenance tree for any rendered value
    diff <rev1>..<rev2>                        Classify claim changes across revisions
    adopt [<file...>]                          Bootstrap evidence.lock.json from existing Markdown with zero byte changes
    merge-driver %O %A %B                      Structural Git merge driver for evidence.lock.json and wired Markdown",
        env!("CARGO_PKG_VERSION")
    );
}
