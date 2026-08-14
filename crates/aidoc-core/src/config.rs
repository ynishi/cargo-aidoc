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

    /// External LLM-doc platforms to emit overlay artifacts for.
    ///
    /// Each entry adds platform-specific files on top of the core
    /// `docs/aidoc/` output (e.g. `context7.json` at the repo root for
    /// [`Platform::Context7`]). See the `platform` module for what each
    /// overlay emits.
    pub platforms: Vec<Platform>,

    /// If true, additionally emit an error catalog: one
    /// `errors/<CODE>.md` per catalogued diagnostic, a deterministic
    /// `errors/index.json`, and a top-level `llms-errors.txt`. The
    /// catalog is populated from every rustdoc item that derives
    /// `miette::Diagnostic` — consumers add no dependency on
    /// cargo-aidoc, only on miette. See [`crate::error_catalog`] for
    /// the consumer contract.
    pub emit_error_catalog: bool,

    /// Project title for the `llms.txt` H1. Wired to `--title` on the
    /// CLI. When unset, the workspace root directory's basename is
    /// used — which makes the committed artifact depend on what the
    /// checkout happens to be called: a git worktree named after its
    /// branch bakes the branch slug into `llms.txt`, and a `--check`
    /// run from a normally-named clone then reports drift. Virtual
    /// workspaces have no root package name to fall back on, so the
    /// stable title has to come from the caller.
    pub title: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            out_dir: PathBuf::from("docs/aidoc"),
            preset: Preset::Publish,
            strict: false,
            check: false,
            exclude: Vec::new(),
            platforms: Vec::new(),
            emit_error_catalog: false,
            title: None,
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

/// External LLM-doc platform this crate can emit overlay artifacts for.
///
/// Each variant maps to a small set of extra files (typically a
/// platform manifest at the repo root) that a downstream service
/// consumes. Layouts and required fields are captured in the
/// `platform` module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// [Context7](https://context7.com): manual-submit + GitHub Action
    /// service. Overlay emits `context7.json` at the repo root
    /// (`projectTitle`, `description`, `folders`, `branch`).
    Context7,
    /// [DeepWiki](https://deepwiki.com): auto-crawl service. Overlay
    /// emits `.devin/wiki.json` at the repo root for workspaces large
    /// enough to hit DeepWiki's page-count limit.
    DeepWiki,
    /// Anthropic-style: `.md` page mirror + reverse cross-ref hint
    /// pointing back at the workspace `llms.txt`. Modifies existing
    /// per-module `.md` artifacts in place; adds no new files.
    AnthropicStyle,
}

impl Platform {
    /// The name expected by the CLI (`--platform <name>`) and by any
    /// future configuration parser.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context7 => "context7",
            Self::DeepWiki => "deepwiki",
            Self::AnthropicStyle => "anthropic-style",
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = UnknownPlatform;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "context7" => Ok(Self::Context7),
            "deepwiki" => Ok(Self::DeepWiki),
            "anthropic-style" => Ok(Self::AnthropicStyle),
            other => Err(UnknownPlatform(other.to_owned())),
        }
    }
}

/// Returned by [`Platform::from_str`] when the input doesn't match any
/// known platform name.
#[derive(Debug, Clone)]
pub struct UnknownPlatform(pub String);

impl std::fmt::Display for UnknownPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown platform `{}`; expected one of: context7, deepwiki, anthropic-style",
            self.0
        )
    }
}

impl std::error::Error for UnknownPlatform {}
