//! Indexed representation of a Rust workspace's public API surface.
//!
//! The index stage transforms rustdoc JSON (one file per crate) into these
//! in-memory types. The generate stage consumes an [`IndexedWorkspace`] to
//! emit `llms.txt`, narrative markdown, and machine JSON artifacts. The lint
//! stage inspects the same structure to report doc-coverage violations.
//!
//! The entry point is [`IndexedWorkspace::build`], which walks the
//! workspace via `cargo_metadata` and invokes rustdoc once per crate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cargo_metadata::{MetadataCommand, TargetKind};

use crate::config::Config;
use crate::error::{Error, Result};
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
    /// The target triple every crate in this workspace was documented
    /// for, or [`None`] when nothing was indexed.
    ///
    /// One value for the whole workspace because one run invokes
    /// rustdoc the same way for every crate; the first payload's answer
    /// is the run's answer. It is read off the payload rather than off
    /// this process's own `cfg` so that a future `--target` would be
    /// recorded correctly without touching this.
    ///
    /// See [`crate::manifest`] for what it is compared against and why.
    pub fn target_triple(&self) -> Option<&str> {
        self.crates
            .first()
            .map(|krate| krate.crate_data.target.triple.as_str())
    }

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

        // The merge point config.rs promises: `[workspace.metadata.aidoc]`
        // is read here, where cargo_metadata has already parsed the
        // workspace manifest, and unioned with whatever the caller put in
        // `config.exclude`. Validated against the real package list
        // because the failure mode this field exists for is silent — an
        // entry that matches nothing excludes nothing, the artifact grows
        // anyway, and the operator reads the unchanged output as "the
        // exclude did not work". A stale entry after a crate rename or
        // removal fails the run for the same reason `aidoc-check` fails
        // on drift: the committed configuration no longer describes the
        // tree, and somebody should look.
        let mut exclude = config.exclude.clone();
        exclude.extend(exclude_from_workspace_metadata(
            &metadata.workspace_metadata,
        )?);

        let package_names: HashSet<String> = metadata
            .workspace_packages()
            .iter()
            .map(|package| package.name.to_string())
            .collect();
        validate_exclude(&exclude, &package_names)?;

        let mut crates = Vec::new();

        for package in metadata.workspace_packages() {
            let package_name = package.name.to_string();

            if exclude.iter().any(|excluded| excluded == &package_name) {
                continue;
            }

            let Some(target) = select_target(&package.targets) else {
                continue;
            };

            let manifest_path = package.manifest_path.as_std_path();
            let crate_data = rustdoc::build_and_parse(
                &target,
                &package_name,
                manifest_path,
                &root,
                config.toolchain(),
            )?;

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
    if let Some(lib) = targets.iter().find(|t| {
        t.kind
            .iter()
            .any(|k| matches!(k, TargetKind::Lib | TargetKind::RLib))
    }) {
        // Carry the target's own name: `[lib] name = "..."` (e.g. a
        // Tauri app's `<pkg>_lib`) makes it diverge from the package
        // name, and the JSON payload is named after the target.
        return Some(Target::Lib(lib.name.as_str()));
    }

    targets.iter().find_map(|t| {
        if t.kind.iter().any(|k| matches!(k, TargetKind::Bin)) {
            Some(Target::Bin(t.name.as_str()))
        } else {
            None
        }
    })
}

/// Read crate names out of `[workspace.metadata.aidoc].exclude`.
///
/// `metadata` is the raw `[workspace.metadata]` table as cargo reported
/// it (`Metadata::workspace_metadata`), `Value::Null` when the manifest
/// has none. An absent `aidoc` table or an absent `exclude` key both
/// mean "exclude nothing" — the field is opt-in. A present key that is
/// not an array of strings is a config error rather than an empty list,
/// because the misspelling that produces one (`exclude = "name"`, a
/// nested table, a number in the list) would otherwise read exactly
/// like success.
fn exclude_from_workspace_metadata(metadata: &serde_json::Value) -> Result<Vec<String>> {
    let Some(exclude) = metadata.get("aidoc").and_then(|aidoc| aidoc.get("exclude")) else {
        return Ok(Vec::new());
    };

    let Some(entries) = exclude.as_array() else {
        return Err(Error::Config {
            message: format!(
                "[workspace.metadata.aidoc].exclude must be an array of crate names, got: {exclude}"
            ),
        });
    };

    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::Config {
                    message: format!(
                        "[workspace.metadata.aidoc].exclude entries must be strings, got: {entry}"
                    ),
                })
        })
        .collect()
}

/// Refuse an exclude list that names crates the workspace does not have.
///
/// An entry that matches nothing excludes nothing, and the run's output
/// is indistinguishable from the exclude never having been written —
/// the one failure mode this feature cannot afford, since its whole job
/// is to be visibly in effect. Stale entries (a renamed or removed
/// crate) fail here too, deliberately: the configuration should follow
/// the tree the same way `docs/aidoc/` itself does.
fn validate_exclude(exclude: &[String], package_names: &HashSet<String>) -> Result<()> {
    for name in exclude {
        if !package_names.contains(name) {
            return Err(Error::Config {
                message: format!(
                    "[workspace.metadata.aidoc].exclude names `{name}`, which is not a \
                     workspace package — fix the spelling, or drop the entry if the \
                     crate is gone"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exclude_absent_metadata_is_empty() {
        assert!(
            exclude_from_workspace_metadata(&serde_json::Value::Null)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exclude_absent_aidoc_table_is_empty() {
        let metadata = json!({ "other-tool": { "exclude": ["x"] } });
        assert!(
            exclude_from_workspace_metadata(&metadata)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exclude_absent_key_is_empty() {
        let metadata = json!({ "aidoc": {} });
        assert!(
            exclude_from_workspace_metadata(&metadata)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exclude_reads_names_in_order() {
        let metadata = json!({ "aidoc": { "exclude": ["teams-core", "teams-infra"] } });
        assert_eq!(
            exclude_from_workspace_metadata(&metadata).unwrap(),
            vec!["teams-core".to_string(), "teams-infra".to_string()]
        );
    }

    #[test]
    fn exclude_rejects_non_array() {
        let metadata = json!({ "aidoc": { "exclude": "teams-core" } });
        let err = exclude_from_workspace_metadata(&metadata).unwrap_err();
        assert!(matches!(err, Error::Config { .. }), "got: {err}");
    }

    #[test]
    fn exclude_rejects_non_string_entry() {
        let metadata = json!({ "aidoc": { "exclude": ["teams-core", 7] } });
        let err = exclude_from_workspace_metadata(&metadata).unwrap_err();
        assert!(matches!(err, Error::Config { .. }), "got: {err}");
    }

    #[test]
    fn validate_accepts_known_names_and_empty() {
        let known = names(&["a", "b"]);
        validate_exclude(&[], &known).unwrap();
        validate_exclude(&["a".to_string(), "b".to_string()], &known).unwrap();
    }

    #[test]
    fn validate_rejects_unknown_name() {
        let err =
            validate_exclude(&["tems-core".to_string()], &names(&["teams-core"])).unwrap_err();
        assert!(matches!(err, Error::Config { .. }), "got: {err}");
        assert!(err.to_string().contains("tems-core"), "got: {err}");
    }
}
