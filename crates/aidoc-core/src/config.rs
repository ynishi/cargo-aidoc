//! Configuration for the aidoc pipeline.
//!
//! The `Config` merges three sources with a fixed precedence:
//!
//! 1. CLI arguments (highest precedence, wins over anything else)
//! 2. `[package.metadata.aidoc]` in a member crate's `Cargo.toml`
//! 3. `[workspace.metadata.aidoc]` in the workspace `Cargo.toml`
//!
//! Values not set by any source fall back to the defaults documented on each
//! field. `Config` itself is a plain struct; the merge logic lives in the
//! index stage where the workspace layout is already known.

use std::path::PathBuf;

/// Runtime configuration for a single aidoc invocation.
#[derive(Debug, Clone)]
pub struct Config {
    /// Output directory for generated artifacts. Defaults to
    /// `<repo>/docs/aidoc/` relative to the workspace root.
    pub out_dir: PathBuf,

    /// Which artifact set to emit. Currently only [`Preset::Publish`].
    pub preset: Preset,

    /// If true, promote lint warnings to errors and cause the pipeline to
    /// exit with a non-zero code. Wired to `--strict` on the CLI.
    pub strict: bool,

    /// If true, run in check mode: generate artifacts into a temporary
    /// directory and diff against the committed `out_dir`. Wired to
    /// `--check` on the CLI. No files are written to `out_dir`.
    pub check: bool,

    /// Crate names to skip when enumerating the workspace. Sourced from
    /// `[workspace.metadata.aidoc].exclude`.
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            out_dir: PathBuf::from("docs/aidoc"),
            preset: Preset::Publish,
            strict: false,
            check: false,
            exclude: Vec::new(),
        }
    }
}

/// Which artifact set the pipeline should emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Emit the full artifact set intended for publishing:
    /// `llms.txt`, per-crate narrative markdown, `llms-full.txt`, and
    /// deterministic `api/<crate>.json`.
    Publish,
}
