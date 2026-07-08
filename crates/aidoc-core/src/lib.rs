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
//!    `cargo +nightly rustdoc -- -Z unstable-options --output-format json`,
//!    or an equivalent stable entry point once one exists) and load it into
//!    an in-memory representation of the crate's public API surface.
//! 2. **Generate** — project the index into one or more LLM-facing
//!    artifacts: an `llms.txt` summary, a narrative markdown document that
//!    mirrors the crate/module structure, and a deterministic JSON document
//!    intended for machine consumption (diffing, CI checks).
//! 3. **Lint** — check the index against a set of doc-coverage rules (for
//!    example: every public item has a doc comment, every crate root has an
//!    architecture narrative) and report violations.
//!
//! Both `cargo-aidoc` (CLI) and `aidoc-mcp` (MCP server) are thin front
//! ends over this pipeline; neither crate should contain pipeline logic of
//! its own.
//!
//! This crate is currently a scaffold: the two-stage pipeline is not yet
//! implemented; only the shared type definitions ([`Config`], [`Error`],
//! [`IndexedCrate`], [`IndexedWorkspace`]) are in place.

#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod index;

pub use config::{Config, Preset};
pub use error::{Error, Result};
pub use index::{IndexedCrate, IndexedWorkspace};
