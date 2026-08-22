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
//!    (crate root has a narrative, public modules are documented) and
//!    report violations.
//!
//! Both `cargo-aidoc` (CLI) and `aidoc-mcp` (MCP server) are thin front
//! ends over this pipeline; neither crate should contain pipeline logic
//! of its own. The single entry point for both is [`run`], which returns
//! a [`Report`] of generated artifacts and lint diagnostics.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

pub mod config;
pub mod error;
pub mod error_catalog;
pub mod generate;
pub mod index;
pub mod lint;
pub mod manifest;
pub mod platform;
mod rustdoc;

/// The nightly toolchain whose rustdoc JSON this build can read.
///
/// rustdoc's JSON payload carries a `format_version`, every nightly
/// emits exactly one, and it changes whenever rustdoc's types do. This
/// crate parses with a fixed `rustdoc-types`, so the two are one pair:
/// **bump `rustdoc-types` and this constant in the same commit.**
///
/// It is public because the alternative is every consumer copying a
/// date into their CI and not noticing when it rots. Install what this
/// binary asks for:
///
/// ```bash
/// rustup toolchain install "$(cargo aidoc --print-required-toolchain)"
/// ```
///
/// Asking for `nightly` instead was the previous behaviour and is a
/// moving target by construction: a consumer's CI installs the current
/// nightly, the format has moved on, and the run fails on a
/// disagreement between two tools rather than on the code under test.
///
/// Current value tracks `rustdoc-types 0.60` (`format_version` 60),
/// whose window upstream runs from the commit that raised 59→60 to the
/// one that raised 60→61.
pub const REQUIRED_NIGHTLY: &str = "nightly-2026-07-07";

pub use config::{Config, Platform, Preset, UnknownPlatform};
pub use error::{Error, Result};
pub use error_catalog::{ErrorEntry, Snippet};
pub use generate::{Artifact, ArtifactLocation, ChunkSize, LlmsFullReport};
pub use index::{IndexedCrate, IndexedWorkspace};
pub use lint::{Diagnostic, Level};
pub use manifest::{Manifest, TargetVerdict};

// `DiffSummary` and `classify_diffs` are defined below in this file;
// re-export them for consumers that stick to `aidoc_core::` paths.

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
    /// The target triple these artifacts describe, or [`None`] when the
    /// run indexed no crates.
    ///
    /// Carried on the report so a front end can ask
    /// [`target_verdict`] without re-running the index stage. See
    /// [`manifest`] for what the answer is used for.
    pub target: Option<String>,
    /// Size facts about `llms-full.txt`: its per-chunk byte breakdown
    /// and what the byte cap (if one was configured via
    /// `--llms-full-max-bytes` or
    /// `[workspace.metadata.aidoc].llms-full-max-bytes`) kept and
    /// dropped. Captured at render time; front ends use it for the
    /// truncation notice and `--size-report`.
    pub llms_full: generate::LlmsFullReport,
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

/// Compare what this run documented against what the artifact set in
/// `out_dir` says it describes.
///
/// Both front ends call this before writing or diffing: on
/// [`TargetVerdict::Mismatch`] a write deletes another host's items and
/// a diff answers a question nobody asked. See [`manifest`] for the
/// full reasoning.
pub fn target_verdict(report: &Report, out_dir: &Path) -> Result<TargetVerdict> {
    manifest::verdict(out_dir, report.target.as_deref())
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

    let (mut artifacts, llms_full) = generate::render_all(&workspace, config.title.as_deref())
        .map_err(|source| Error::Generate {
            source: Box::new(source),
            index_summary: index_summary.clone(),
        })?;

    if config.emit_error_catalog {
        let entries = error_catalog::extract(&workspace);
        generate::render_error_catalog(&entries, &mut artifacts).map_err(|source| {
            Error::Generate {
                source: Box::new(source),
                index_summary,
            }
        })?;
    }

    platform::apply_overlays(&workspace, &mut artifacts, &config.platforms)?;

    let diagnostics = lint::lint(&workspace, config);

    Ok(Report {
        artifacts,
        diagnostics,
        target: workspace.target_triple().map(str::to_string),
        llms_full,
    })
}

/// Resolve where a single artifact lands, given the two possible base
/// directories a caller might have configured.
fn resolve_path(artifact: &Artifact, out_dir: &Path, workspace_root: &Path) -> PathBuf {
    let base = match artifact.location {
        ArtifactLocation::OutDir => out_dir,
        ArtifactLocation::WorkspaceRoot => workspace_root,
    };
    base.join(&artifact.path)
}

/// Write every artifact in `report` to disk. `out_dir` is the base for
/// artifacts whose location is [`ArtifactLocation::OutDir`]; the
/// workspace root is the base for
/// [`ArtifactLocation::WorkspaceRoot`] artifacts (typically platform
/// manifests such as `context7.json`).
///
/// Existing files are overwritten; parent directories are created as
/// needed.
pub fn write_report(report: &Report, out_dir: &Path, workspace_root: &Path) -> Result<()> {
    for artifact in &report.artifacts {
        let path = resolve_path(artifact, out_dir, workspace_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, artifact.body.as_bytes())?;
    }
    Ok(())
}

/// Detailed classification of every diff in a check-mode run.
///
/// `missing` collects the artifact paths that don't exist on disk yet
/// (the caller would create them on the next `aidoc_gen`). `modified`
/// collects paths whose on-disk copy differs from the freshly
/// generated body. Paths that match on disk contribute to neither
/// list.
///
/// This is the shape returned by [`classify_diffs`]; both `cargo aidoc
/// --check` and the MCP `aidoc_check` tool use it to produce a
/// consistent, actionable summary message via
/// [`DiffSummary::summary_message`].
#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    /// Artifact paths (base-relative) that don't exist on disk.
    pub missing: Vec<String>,
    /// Artifact paths (base-relative) whose on-disk bytes differ.
    pub modified: Vec<String>,
}

impl DiffSummary {
    /// True when nothing would change on disk.
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.modified.is_empty()
    }

    /// Total number of artifacts that would change (missing + modified).
    pub fn total(&self) -> usize {
        self.missing.len() + self.modified.len()
    }

    /// Iterate every diff path in a stable order (all missing first,
    /// then all modified), matching the order they were discovered.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.missing.iter().chain(self.modified.iter())
    }

    /// Actionable, human-readable summary of the check result. Callers
    /// pass the total artifact count so the clean-state message can
    /// state how many artifacts were compared.
    ///
    /// The four branches:
    ///
    /// 1. Clean — `"aidoc: check clean (N artifact(s))"`
    /// 2. Only missing — `"aidoc: N artifact(s) not on disk yet — run
    ///    aidoc_gen to write them"`. Triggers for both full uninit and
    ///    partial uninit (e.g. `docs/aidoc/` exists but the error
    ///    catalog subset was never written).
    /// 3. Only modified — `"aidoc: N artifact(s) content changed —
    ///    run aidoc_gen to update"`
    /// 4. Mixed — `"aidoc: N missing + M modified — run aidoc_gen"`
    pub fn summary_message(&self, total_artifacts: usize) -> String {
        if self.is_empty() {
            format!("aidoc: check clean ({total_artifacts} artifact(s))")
        } else if self.modified.is_empty() {
            format!(
                "aidoc: {} artifact(s) not on disk yet — run aidoc_gen to write them",
                self.missing.len()
            )
        } else if self.missing.is_empty() {
            format!(
                "aidoc: {} artifact(s) content changed — run aidoc_gen to update",
                self.modified.len()
            )
        } else {
            format!(
                "aidoc: {} missing + {} modified — run aidoc_gen",
                self.missing.len(),
                self.modified.len()
            )
        }
    }
}

/// Compare every artifact in `report` against its on-disk copy and
/// return the categorised diff. See [`write_report`] for how `out_dir`
/// and `workspace_root` are interpreted.
///
/// This is the check-mode counterpart of [`write_report`]: nothing is
/// written; the caller decides how to react to the returned
/// [`DiffSummary`]. Read failures other than "file not found"
/// propagate as errors so no diff is ever silently dropped.
pub fn classify_diffs(
    report: &Report,
    out_dir: &Path,
    workspace_root: &Path,
) -> Result<DiffSummary> {
    let mut summary = DiffSummary::default();
    for artifact in &report.artifacts {
        let path = resolve_path(artifact, out_dir, workspace_root);
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes != artifact.body.as_bytes() {
                    summary.modified.push(artifact.path.clone());
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                summary.missing.push(artifact.path.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(summary)
}

/// Backwards-compatible wrapper around [`classify_diffs`]. Returns the
/// same flat list of paths that earlier versions of aidoc-core
/// produced; new callers should prefer [`classify_diffs`] so they can
/// distinguish missing from modified artifacts.
pub fn diff_report(report: &Report, out_dir: &Path, workspace_root: &Path) -> Result<Vec<String>> {
    let summary = classify_diffs(report, out_dir, workspace_root)?;
    Ok(summary.paths().cloned().collect())
}
