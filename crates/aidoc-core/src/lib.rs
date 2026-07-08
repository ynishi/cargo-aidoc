//! # aidoc-core
//!
//! Core library that turns rustdoc JSON output into LLM-facing
//! documentation artifacts. This crate holds all of the logic shared by
//! the two thin front ends in this workspace: the `cargo aidoc` subcommand
//! (`crates/cargo-aidoc`) and the MCP server (`crates/aidoc-mcp`).
//!
//! ## Architecture
//!
//! The pipeline is split into three stages:
//!
//! 1. **Index** — read rustdoc's JSON output (produced via
//!    `cargo +nightly rustdoc -- -Z unstable-options --output-format json`)
//!    and load it into an in-memory representation of the crate's public
//!    API surface.
//! 2. **Generate** — project the index into one or more LLM-facing
//!    artifacts: an `llms.txt` summary, per-crate / per-module narrative
//!    markdown, `llms-full.txt`, and a deterministic `api/<crate>.json`
//!    for machine consumption (diffing, CI checks).
//! 3. **Lint** — check the index against a set of doc-coverage rules
//!    (crate root has a narrative, public modules are documented, the
//!    generated `llms-full.txt` fits a soft size cap) and report
//!    violations.
//!
//! Both `cargo-aidoc` (CLI) and `aidoc-mcp` (MCP server) are thin front
//! ends over this pipeline; neither crate should contain pipeline logic
//! of its own. The single entry point for both is [`run`], which returns
//! a [`Report`] of generated artifacts and lint diagnostics.

#![warn(missing_docs)]

use std::path::Path;

pub mod config;
pub mod error;
pub mod generate;
pub mod index;
pub mod lint;
pub mod platform;
mod rustdoc;

pub use config::{Config, Platform, Preset, UnknownPlatform};
pub use error::{Error, Result};
pub use generate::Artifact;
pub use index::{IndexedCrate, IndexedWorkspace};
pub use lint::{Diagnostic, Level};

/// The output of a single pipeline run: everything a front end needs to
/// either write artifacts to disk (`cargo aidoc`) or compare them to a
/// checked-in tree (`cargo aidoc --check`).
///
/// Ordering: `artifacts` matches the emission order of
/// [`generate::render_all`]; `diagnostics` matches the emission order of
/// [`lint::lint`].
#[derive(Debug, Clone)]
pub struct Report {
    /// Generated files, keyed by relative path.
    pub artifacts: Vec<Artifact>,
    /// Lint findings. Empty on a fully clean run.
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// True if any diagnostic has [`Level::Error`]. Callers use this to
    /// pick between exit code 0 (clean) and 2 (lint violation).
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| matches!(d.level, Level::Error))
    }
}

/// Run the full index → generate → lint pipeline against the workspace
/// rooted at `workspace_root`, using `config` for defaults, exclusions
/// and strict-mode toggling.
///
/// Failures during the generate stage are wrapped in [`Error::Generate`]
/// so callers still receive a short summary of what the index stage did
/// before the failure. There is no rollback and no partial output.
pub fn run(workspace_root: &Path, config: &Config) -> Result<Report> {
    let workspace = IndexedWorkspace::build(workspace_root, config)?;
    let index_summary = format!("indexed {} crate(s)", workspace.crates.len());

    let mut artifacts =
        generate::render_all(&workspace, None).map_err(|source| Error::Generate {
            source: Box::new(source),
            index_summary,
        })?;

    platform::apply_overlays(&workspace, &mut artifacts, &config.platforms)?;

    let diagnostics = lint::lint(&workspace, &artifacts, config);

    Ok(Report {
        artifacts,
        diagnostics,
    })
}

/// Write every artifact in `report` under `out_dir`. Existing files are
/// overwritten; parent directories are created as needed.
///
/// This helper exists as a convenience for callers that want the default
/// on-disk layout; front ends that need to project artifacts elsewhere
/// (staging area, tar stream, MCP response envelope) can iterate
/// `report.artifacts` themselves.
pub fn write_report(report: &Report, out_dir: &Path) -> Result<()> {
    for artifact in &report.artifacts {
        let path = out_dir.join(&artifact.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, artifact.body.as_bytes())?;
    }
    Ok(())
}

/// Compare every artifact in `report` against its on-disk counterpart in
/// `out_dir` and return the list of paths whose contents differ (or that
/// are missing on disk).
///
/// This is the check-mode counterpart of [`write_report`]: nothing is
/// written; callers pick between exit code 0 (empty result) and 2
/// (non-empty). Read failures other than "file not found" propagate as
/// errors.
pub fn diff_report(report: &Report, out_dir: &Path) -> Result<Vec<String>> {
    let mut diffs = Vec::new();
    for artifact in &report.artifacts {
        let path = out_dir.join(&artifact.path);
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes != artifact.body.as_bytes() {
                    diffs.push(artifact.path.clone());
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                diffs.push(artifact.path.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(diffs)
}
