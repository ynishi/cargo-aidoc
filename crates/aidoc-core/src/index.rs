//! Indexed representation of a Rust workspace's public API surface.
//!
//! The index stage transforms rustdoc JSON (one file per crate) into these
//! in-memory types. The generate stage consumes an [`IndexedWorkspace`] to
//! emit `llms.txt`, narrative markdown, and machine JSON artifacts. The lint
//! stage inspects the same structure to report doc-coverage violations.
//!
//! This module currently declares the types only; the actual index build
//! logic lands in a follow-up phase.

use std::path::PathBuf;

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
