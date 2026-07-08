//! Indexed representation of a Rust workspace's public API surface.
//!
//! The index stage transforms rustdoc JSON (one file per crate) into these
//! in-memory types. The generate stage consumes an [`IndexedWorkspace`] to
//! emit `llms.txt`, narrative markdown, and machine JSON artifacts. The lint
//! stage inspects the same structure to report doc-coverage violations.
//!
//! The entry point is [`IndexedWorkspace::build`], which walks the
//! workspace via `cargo_metadata` and invokes rustdoc once per crate.

use std::path::{Path, PathBuf};

use cargo_metadata::{MetadataCommand, TargetKind};

use crate::config::Config;
use crate::error::Result;
use crate::rustdoc::{self, Target};

/// A single crate that has been indexed via rustdoc JSON.
#[derive(Debug, Clone)]
pub struct IndexedCrate {
    /// The crate's package name (as reported by cargo metadata).
    pub name: String,

    /// The crate's version string (as reported by cargo metadata).
    pub version: String,

    /// The crate root's outer doc comment (`//!` block), if any. Sourced
    /// from `rustdoc_types::Crate::index[root].docs`.
    pub root_module_doc: Option<String>,

    /// The raw parsed rustdoc JSON payload. Retained so the generate and
    /// lint stages can walk item / module trees without re-parsing.
    pub crate_data: rustdoc_types::Crate,
}

/// The complete indexed workspace: every crate that was successfully
/// indexed, plus workspace-level context needed by the generate stage.
#[derive(Debug, Clone)]
pub struct IndexedWorkspace {
    /// Workspace root directory (parent of the top-level `Cargo.toml`).
    pub root: PathBuf,

    /// Indexed crates in the order they were discovered by cargo metadata.
    /// Callers should not assume alphabetic or dependency order.
    pub crates: Vec<IndexedCrate>,
}

impl IndexedWorkspace {
    /// Enumerate the workspace at `workspace_root`, run rustdoc for every
    /// crate that is not excluded by `config`, and package the parsed
    /// results into an [`IndexedWorkspace`].
    ///
    /// Crates that expose neither a library nor a binary target are
    /// skipped silently (e.g. workspace-only virtual manifests). A single
    /// crate failure aborts the whole index pass; there is no partial
    /// result path at this stage.
    pub fn build(workspace_root: &Path, config: &Config) -> Result<Self> {
        let metadata = MetadataCommand::new()
            .manifest_path(workspace_root.join("Cargo.toml"))
            .exec()?;

        let root: PathBuf = metadata.workspace_root.as_std_path().to_path_buf();
        let mut crates = Vec::new();

        for package in metadata.workspace_packages() {
            let package_name = package.name.to_string();

            if config
                .exclude
                .iter()
                .any(|excluded| excluded == &package_name)
            {
                continue;
            }

            let Some(target) = select_target(&package.targets) else {
                continue;
            };

            let manifest_path = package.manifest_path.as_std_path();
            let crate_data =
                rustdoc::build_and_parse(&target, &package_name, manifest_path, &root)?;

            let root_module_doc = crate_data
                .index
                .get(&crate_data.root)
                .and_then(|item| item.docs.clone());

            crates.push(IndexedCrate {
                name: package_name,
                version: package.version.to_string(),
                root_module_doc,
                crate_data,
            });
        }

        Ok(IndexedWorkspace { root, crates })
    }
}

/// Pick which rustdoc target to build for a crate.
///
/// Prefers the library target; falls back to the first binary target.
/// Proc-macro / example / test / bench targets are ignored because they
/// don't contribute to the crate's public API surface.
fn select_target(targets: &[cargo_metadata::Target]) -> Option<Target<'_>> {
    if targets.iter().any(|t| {
        t.kind
            .iter()
            .any(|k| matches!(k, TargetKind::Lib | TargetKind::RLib))
    }) {
        return Some(Target::Lib);
    }

    targets.iter().find_map(|t| {
        if t.kind.iter().any(|k| matches!(k, TargetKind::Bin)) {
            Some(Target::Bin(t.name.as_str()))
        } else {
            None
        }
    })
}
