//! `cargo aidoc` — cargo subcommand entry point.
//!
//! This binary is a thin CLI wrapper around the pipeline implemented in
//! `aidoc-core`. It parses `cargo aidoc <args>` invocations (cargo strips
//! the leading `aidoc` argument when invoking `cargo-aidoc` as a
//! subcommand) and delegates to the core library for indexing,
//! generation, and linting.
//!
//! ## Exit codes
//!
//! - `0` — clean run, all artifacts written (or, in `--check` mode, all
//!   on-disk artifacts already match).
//! - `1` — pipeline error (rustdoc failed, I/O error, config invalid),
//!   or a write refused because the committed artifacts describe
//!   another target (see [`aidoc_core::manifest`]).
//! - `2` — lint violation, or `--check` detected a drift between the
//!   generated artifacts and the on-disk copy.
//! - `3` — `--check` could not answer: the committed artifacts describe
//!   another target, so any diff found here is about `cfg` resolution
//!   rather than about whether they are stale. Distinct from 2 because
//!   the two want opposite reactions — 2 means regenerate, 3 means this
//!   host cannot say. A caller that treats every non-zero code as
//!   failure keeps its old behaviour minus the false red; one that
//!   wants "check where it can be checked" branches on 3.

use std::path::PathBuf;
use std::process::ExitCode;

use aidoc_core::{Config, Level, Platform, Report};
use clap::Parser;

/// Generate LLM-facing documentation artifacts from rustdoc JSON.
#[derive(Debug, Parser)]
#[command(name = "cargo-aidoc", version, about, long_about = None)]
struct Cli {
    /// Workspace root (directory containing the top-level Cargo.toml).
    /// Defaults to the current directory.
    #[arg(long, value_name = "PATH")]
    workspace_root: Option<PathBuf>,

    /// Output directory. Defaults to `<workspace_root>/docs/aidoc/`.
    #[arg(long, value_name = "PATH")]
    out_dir: Option<PathBuf>,

    /// Check mode: don't write artifacts. Instead, compare them against
    /// the on-disk copy and exit with code 2 if any file would change.
    /// Intended for CI drift detection.
    #[arg(long)]
    check: bool,

    /// Strict mode: promote lint warnings to errors so they cause a
    /// non-zero exit code.
    #[arg(long)]
    strict: bool,

    /// External LLM-doc platform overlays to emit on top of the core
    /// output. Accepts a comma-separated list or can be repeated.
    /// Known values: `context7`, `deepwiki`, `anthropic-style`.
    #[arg(long, value_delimiter = ',', value_name = "NAME")]
    platform: Vec<String>,

    /// Additionally emit an error catalog (`errors/<CODE>.md`,
    /// `errors/index.json`, `llms-errors.txt`) built from every
    /// `#[derive(miette::Diagnostic)]` item in the workspace.
    /// Consumers do not need to depend on cargo-aidoc for this to
    /// work — the extractor reads rustdoc JSON.
    #[arg(long)]
    errors: bool,

    /// Project title for the `llms.txt` H1. Defaults to the workspace
    /// root directory's basename — pass this when the checkout name is
    /// not the project name (a git worktree named after its branch, a
    /// CI job dir), otherwise the committed artifact drifts between
    /// checkouts.
    #[arg(long, value_name = "TITLE")]
    title: Option<String>,

    /// Toolchain to run rustdoc under. Defaults to the dated nightly
    /// whose JSON format this build reads — pass this only to try a
    /// format the build does not yet support.
    #[arg(long, value_name = "TOOLCHAIN")]
    toolchain: Option<String>,

    /// Byte cap for `llms-full.txt`. When set, the file is truncated
    /// at chunk boundaries to fit, and ends with a notice listing every
    /// omitted chunk. Overrides
    /// `[workspace.metadata.aidoc].llms-full-max-bytes`. Unset means no
    /// cap and no size diagnostics: `llms-full.txt` is bulk-ingest
    /// material for tools that chunk it, and no spec or platform
    /// publishes a size limit to enforce.
    #[arg(long, value_name = "BYTES")]
    llms_full_max_bytes: Option<usize>,

    /// Print a per-chunk byte breakdown of `llms-full.txt` — marking
    /// which chunks a configured cap keeps and drops — and write
    /// nothing. A dry run for tuning `llms-full-max-bytes` and
    /// `exclude` before committing to either.
    #[arg(long)]
    size_report: bool,

    /// Move the committed artifacts to this host's target.
    ///
    /// Without it, a run that would overwrite artifacts generated for
    /// another target refuses instead: `cfg`-gated items differ between
    /// targets, so the overwrite silently deletes what the other one
    /// documents. Pass this when moving the canonical target on
    /// purpose — a project whose CI moved from macOS to Linux, say —
    /// and commit the deletions as the deliberate change they then are.
    #[arg(long)]
    retarget: bool,

    /// Print the toolchain this build needs, and exit.
    ///
    /// So a CI job installs exactly the right one without copying a
    /// date that then goes stale:
    /// `rustup toolchain install "$(cargo aidoc --print-required-toolchain)"`.
    #[arg(long)]
    print_required_toolchain: bool,
}

fn main() -> ExitCode {
    let args = collect_args();
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            // clap handles --help / --version by returning a "success"
            // error; those still need to exit 0.
            let is_help_or_version = matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            err.print().ok();
            return if is_help_or_version {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
    };

    // Before anything that needs a workspace: this answers a question
    // about the binary, and a CI job asks it to decide what to install.
    if cli.print_required_toolchain {
        println!("{}", aidoc_core::REQUIRED_NIGHTLY);
        return ExitCode::SUCCESS;
    }

    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("cargo-aidoc: {err}");
            ExitCode::from(1)
        }
    }
}

/// Strip cargo's leading `aidoc` subcommand argument, if present.
///
/// When invoked as `cargo aidoc <args>`, cargo executes
/// `cargo-aidoc aidoc <args>`; we want to parse just `<args>`.
fn collect_args() -> Vec<String> {
    let mut args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "aidoc" {
        args.remove(1);
    }
    args
}

fn run(cli: Cli) -> aidoc_core::Result<ExitCode> {
    let workspace_root = cli
        .workspace_root
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let out_dir = cli
        .out_dir
        .unwrap_or_else(|| workspace_root.join("docs/aidoc"));

    let platforms: Vec<Platform> = cli
        .platform
        .iter()
        .map(|s| s.parse::<Platform>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| aidoc_core::Error::Config {
            message: e.to_string(),
        })?;

    let config = Config {
        strict: cli.strict,
        check: cli.check,
        out_dir: out_dir.clone(),
        platforms,
        emit_error_catalog: cli.errors,
        title: cli.title,
        llms_full_max_bytes: cli.llms_full_max_bytes,
        toolchain: cli.toolchain,
        ..Config::default()
    };

    let report = aidoc_core::run(&workspace_root, &config)?;

    print_diagnostics(&report);

    // A size question, answered without touching the disk: neither the
    // target-mismatch guard nor check/write below applies to it.
    if cli.size_report {
        print_size_report(&report);
        return Ok(ExitCode::SUCCESS);
    }

    if report.llms_full.truncated() {
        eprintln!(
            "cargo-aidoc: llms-full.txt truncated to {final_bytes} bytes \
             (llms-full-max-bytes = {cap}): {n} chunk(s) omitted — \
             the file lists them at its end; --size-report shows the full breakdown",
            final_bytes = report.llms_full.final_bytes,
            cap = report
                .llms_full
                .cap_bytes
                .expect("truncated() implies a cap"),
            n = report.llms_full.dropped.len(),
        );
    }

    // Before either branch does its work: on a target mismatch, writing
    // deletes another host's items and diffing compares two different
    // questions. Neither is worth doing, and both look ordinary in
    // their output, which is why this is checked here rather than left
    // to the reader of a diff.
    if let aidoc_core::TargetVerdict::Mismatch {
        recorded,
        generated,
    } = aidoc_core::target_verdict(&report, &out_dir)?
    {
        let verdict = aidoc_core::TargetVerdict::Mismatch {
            recorded: recorded.clone(),
            generated,
        };
        for line in verdict.explain().unwrap_or_default().lines() {
            eprintln!("cargo-aidoc: {line}");
        }
        if cli.check {
            eprintln!(
                "cargo-aidoc: cfg-gated items differ between the two, so a diff here says \
                 nothing about whether the artifacts are stale."
            );
            eprintln!("cargo-aidoc: NOT CHECKED — re-run this on {recorded}.");
            return Ok(ExitCode::from(3));
        }
        if !cli.retarget {
            eprintln!(
                "cargo-aidoc: writing here would drop every item only {recorded} documents, \
                 which reads as an ordinary regeneration in the diff."
            );
            eprintln!(
                "cargo-aidoc: refusing to write. Regenerate on {recorded}, or pass --retarget \
                 to move the committed artifacts to this host."
            );
            return Ok(ExitCode::from(1));
        }
        eprintln!("cargo-aidoc: --retarget given; the artifacts now describe this host.");
    }

    if cli.check {
        // Shared with the `aidoc_check` MCP tool via aidoc_core so
        // both front ends produce the same actionable message.
        let summary = aidoc_core::classify_diffs(&report, &out_dir, &workspace_root)?;
        eprintln!(
            "cargo-aidoc: {}",
            summary.summary_message(report.artifacts.len())
        );
        if !summary.missing.is_empty() {
            eprintln!("cargo-aidoc:   missing ({}):", summary.missing.len());
            for path in &summary.missing {
                eprintln!("cargo-aidoc:     {path}");
            }
        }
        if !summary.modified.is_empty() {
            eprintln!("cargo-aidoc:   modified ({}):", summary.modified.len());
            for path in &summary.modified {
                eprintln!("cargo-aidoc:     {path}");
            }
        }
        if summary.is_empty() {
            if report.has_errors() {
                return Ok(ExitCode::from(2));
            }
            return Ok(ExitCode::SUCCESS);
        }
        Ok(ExitCode::from(2))
    } else {
        aidoc_core::write_report(&report, &out_dir, &workspace_root)?;
        eprintln!(
            "cargo-aidoc: wrote {} artifact(s) to {}",
            report.artifacts.len(),
            out_dir.display()
        );
        if report.has_errors() {
            Ok(ExitCode::from(2))
        } else {
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Print the `--size-report` view: one line per `llms-full.txt` chunk
/// in emission order, `KEEP`/`DROP`-tagged when a cap is configured,
/// then the totals. Goes to stdout — it is the command's answer, not
/// commentary.
fn print_size_report(report: &Report) {
    let full = &report.llms_full;
    let dropped: std::collections::HashSet<&str> =
        full.dropped.iter().map(|c| c.path.as_str()).collect();

    println!("llms-full.txt size report");
    match full.cap_bytes {
        Some(cap) => println!("  cap: {cap} bytes (llms-full-max-bytes)"),
        None => println!("  cap: none (llms-full-max-bytes unset)"),
    }

    let total: usize = full.chunks.iter().map(|c| c.bytes).sum();
    for chunk in &full.chunks {
        let tag = match (
            full.cap_bytes.is_some(),
            dropped.contains(chunk.path.as_str()),
        ) {
            (false, _) => "",
            (true, false) => "KEEP  ",
            (true, true) => "DROP  ",
        };
        println!(
            "  {tag}{path}  {bytes}",
            path = chunk.path,
            bytes = chunk.bytes
        );
    }
    println!(
        "  total: {total} bytes in {n} chunk(s); emitted: {final_bytes} bytes ({d} dropped)",
        n = full.chunks.len(),
        final_bytes = full.final_bytes,
        d = full.dropped.len(),
    );
}

fn print_diagnostics(report: &Report) {
    for diag in &report.diagnostics {
        let tag = match diag.level {
            Level::Warn => "warning",
            Level::Error => "error",
        };
        eprintln!(
            "cargo-aidoc: {tag}[{code}] {location}: {message}",
            code = diag.code,
            location = diag.location,
            message = diag.message,
        );
    }
}
