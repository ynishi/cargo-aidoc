//! Error type for the aidoc pipeline.
//!
//! The pipeline is a two-stage sequence (index → generate) with an optional
//! lint stage layered on top. Failures during generate carry an index-stage
//! summary in the error itself so the caller can report both what succeeded
//! and what failed without needing a side channel. This mirrors the facade
//! error contract used by algocline's `hub_dist` (which this crate ports to
//! rustdoc JSON).

/// The result alias used throughout `aidoc-core`.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors produced by the aidoc pipeline.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `cargo metadata` failed while enumerating workspace crates.
    #[error("cargo metadata failed: {0}")]
    CargoMetadata(#[from] cargo_metadata::Error),

    /// Invoking `cargo +nightly rustdoc` failed at the process level
    /// (non-zero exit, missing toolchain, etc.).
    #[error("cargo rustdoc failed: {message}")]
    RustdocInvocation {
        /// Human-readable description of the failure.
        message: String,
    },

    /// Parsing the rustdoc JSON output failed.
    #[error("rustdoc JSON parse failed: {0}")]
    RustdocParse(#[from] serde_json::Error),

    /// The rustdoc JSON output uses a `format_version` this crate does not
    /// yet support. Bumping `rustdoc-types` is the typical fix.
    #[error("rustdoc format version mismatch: expected {expected}, found {found}")]
    FormatVersionMismatch {
        /// The `format_version` this crate was built against.
        expected: u32,
        /// The `format_version` actually present in the JSON payload.
        found: u32,
    },

    /// An I/O error while reading rustdoc JSON or writing output artifacts.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration parsing or validation failed (bad
    /// `[package.metadata.aidoc]`, unknown projection, etc.).
    #[error("config error: {message}")]
    Config {
        /// Human-readable description of the failure.
        message: String,
    },

    /// The generate stage failed. Carries a compact index-stage summary so
    /// callers can report both the successful indexing pass and the failure
    /// site without an out-of-band channel.
    #[error("generate stage failed: {source}\nindex summary: {index_summary}")]
    Generate {
        /// The underlying failure from the generate stage.
        #[source]
        source: Box<Error>,
        /// Compact human-readable summary of the completed index stage.
        index_summary: String,
    },
}
