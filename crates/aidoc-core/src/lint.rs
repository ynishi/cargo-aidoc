//! Lint stage: report doc-coverage issues on top of an indexed workspace
//! (and, for size-related lints, on top of the generated artifacts).
//!
//! Lints are advisory by default. In strict mode every warning is
//! promoted to an error and the pipeline is expected to exit with code
//! 2. The `strict` flag lives on [`crate::Config`] so both CLI and MCP
//! callers see the same behaviour.
//!
//! Adding a new lint means: (1) pick a stable `code` (kebab-case,
//! short), (2) emit `Diagnostic` from [`lint`] with an actionable
//! message, (3) if the check is expensive, feature-gate it behind a
//! flag on `Config` and document the default.

use rustdoc_types::{ItemEnum, Module, Visibility};

use crate::config::Config;
use crate::generate::Artifact;
use crate::index::{IndexedCrate, IndexedWorkspace};

/// Severity of a single [`Diagnostic`].
///
/// The lint stage only produces `Warn` values directly; strict mode
/// promotes them to `Error` in [`lint`] so callers don't have to
/// re-implement that logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Informational; does not cause a non-zero exit code on its own.
    Warn,
    /// Blocking; typically maps to CLI exit code 2 and MCP verdict = fail.
    Error,
}

/// A single lint finding.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Severity of this finding.
    pub level: Level,
    /// Stable machine-readable identifier (e.g. `"missing-crate-doc"`).
    /// Callers filter and route by this code.
    pub code: &'static str,
    /// Human-readable location, e.g. `crate:aidoc-core`,
    /// `module:aidoc_core::config`, or `artifact:llms-full.txt`.
    pub location: String,
    /// One-sentence description of the finding, ideally actionable.
    pub message: String,
}

/// Soft byte-size ceiling for `llms-full.txt`. Anything larger than
/// this triggers a warning: LLM context budgets tend to be tight and
/// bloated dumps are usually a sign that we're emitting more than the
/// public API surface warrants.
///
/// This is a soft ceiling, not a hard cap; the artifact is emitted
/// either way.
pub const LLMS_FULL_SOFT_MAX_BYTES: usize = 512 * 1024;

/// Minimum number of non-empty lines a crate-root `//!` block must have
/// before it counts as "an actual narrative" instead of a stub. Missing
/// or shorter narratives produce warnings.
pub const CRATE_ROOT_MIN_NARRATIVE_LINES: usize = 5;

/// Run every lint against `workspace` and `artifacts`, applying the
/// strict-mode promotion from `config`.
///
/// Returns diagnostics in emission order: crate-level lints first (in
/// discovery order), then artifact-level lints. Callers that want a
/// deterministic display order can sort by `(location, code)`.
pub fn lint(
    workspace: &IndexedWorkspace,
    artifacts: &[Artifact],
    config: &Config,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for krate in &workspace.crates {
        lint_crate(krate, &mut diagnostics);
    }

    lint_artifacts(artifacts, &mut diagnostics);

    if config.strict {
        for diag in &mut diagnostics {
            if diag.level == Level::Warn {
                diag.level = Level::Error;
            }
        }
    }

    diagnostics
}

fn lint_crate(krate: &IndexedCrate, diagnostics: &mut Vec<Diagnostic>) {
    match krate.root_module_doc.as_deref() {
        None => {
            diagnostics.push(Diagnostic {
                level: Level::Warn,
                code: "missing-crate-doc",
                location: format!("crate:{}", krate.name),
                message: format!(
                    "crate `{}` has no crate-root `//!` doc block; readers land in an empty index.md",
                    krate.name
                ),
            });
        }
        Some(doc) => {
            let non_empty_lines = doc.lines().filter(|line| !line.trim().is_empty()).count();
            if non_empty_lines < CRATE_ROOT_MIN_NARRATIVE_LINES {
                diagnostics.push(Diagnostic {
                    level: Level::Warn,
                    code: "short-crate-doc",
                    location: format!("crate:{}", krate.name),
                    message: format!(
                        "crate `{name}` root doc is only {found} non-empty lines; \
                         consider expanding to at least {min} lines of narrative",
                        name = krate.name,
                        found = non_empty_lines,
                        min = CRATE_ROOT_MIN_NARRATIVE_LINES,
                    ),
                });
            }
        }
    }

    lint_module_docs(krate, diagnostics);
}

fn lint_module_docs(krate: &IndexedCrate, diagnostics: &mut Vec<Diagnostic>) {
    let index = &krate.crate_data.index;

    let Some(root_item) = index.get(&krate.crate_data.root) else {
        return;
    };
    let ItemEnum::Module(root_module) = &root_item.inner else {
        return;
    };

    let crate_prefix = krate.name.replace('-', "_");
    walk_lint_modules(index, root_module, &crate_prefix, &krate.name, diagnostics);
}

fn walk_lint_modules(
    index: &std::collections::HashMap<rustdoc_types::Id, rustdoc_types::Item>,
    module: &Module,
    prefix: &str,
    crate_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for child_id in &module.items {
        let Some(child) = index.get(child_id) else {
            continue;
        };
        if !matches!(child.visibility, Visibility::Public) {
            continue;
        }
        let ItemEnum::Module(child_module) = &child.inner else {
            continue;
        };
        let Some(name) = child.name.as_deref() else {
            continue;
        };

        let path = format!("{prefix}::{name}");

        if child.docs.as_deref().is_none_or(str::is_empty) {
            diagnostics.push(Diagnostic {
                level: Level::Warn,
                code: "missing-module-doc",
                location: format!("module:{path}"),
                message: format!(
                    "public module `{path}` in crate `{crate_name}` has no `//!` doc block"
                ),
            });
        }

        walk_lint_modules(index, child_module, &path, crate_name, diagnostics);
    }
}

fn lint_artifacts(artifacts: &[Artifact], diagnostics: &mut Vec<Diagnostic>) {
    if let Some(full) = artifacts.iter().find(|a| a.path == "llms-full.txt")
        && full.body.len() > LLMS_FULL_SOFT_MAX_BYTES
    {
        diagnostics.push(Diagnostic {
            level: Level::Warn,
            code: "llms-full-too-large",
            location: "artifact:llms-full.txt".to_owned(),
            message: format!(
                "llms-full.txt is {actual} bytes (> soft cap {cap}); \
                 consider trimming crate-root narratives or excluding low-signal crates",
                actual = full.body.len(),
                cap = LLMS_FULL_SOFT_MAX_BYTES,
            ),
        });
    }
}
