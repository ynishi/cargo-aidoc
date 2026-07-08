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
//! - `1` — pipeline error (rustdoc failed, I/O error, config invalid).
//! - `2` — lint violation, or `--check` detected a drift between the
//!   generated artifacts and the on-disk copy.

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
    let out_dir = cli.out_dir.unwrap_or_else(|| workspace_root.join("docs/aidoc"));

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
        ..Config::default()
    };

    let report = aidoc_core::run(&workspace_root, &config)?;

    print_diagnostics(&report);

    if cli.check {
        let diffs = aidoc_core::diff_report(&report, &out_dir, &workspace_root)?;
        if diffs.is_empty() {
            if report.has_errors() {
                return Ok(ExitCode::from(2));
            }
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!("cargo-aidoc: {} artifact(s) would change:", diffs.len());
        for path in diffs {
            eprintln!("  {path}");
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
