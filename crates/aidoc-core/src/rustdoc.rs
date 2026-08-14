//! Invoke rustdoc with `--output-format json` and parse the resulting
//! payload via `rustdoc-types`.
//!
//! rustdoc's JSON output is unstable and toolchain-locked: the payload's
//! `format_version` must match the `rustdoc-types` crate this binary was
//! built against.
//!
//! # Why a dated toolchain and not `nightly`
//!
//! Every nightly emits exactly one `format_version`, and it changes
//! whenever rustdoc's JSON types do. Asking for `nightly` therefore asks
//! for "whatever the schema is today", which is a moving target that a
//! fixed `rustdoc-types` dependency cannot hit for long: a consumer's CI
//! installs the current nightly, the format has moved on, and the run
//! fails on a disagreement between two tools rather than on anything
//! about the code under test.
//!
//! So the toolchain is pinned, in [`crate::REQUIRED_NIGHTLY`], and the
//! pin travels with the `rustdoc-types` version — bump one, bump the
//! other. It is `pub` so a consumer can install exactly what this
//! binary needs without copying a date into their CI and watching it
//! rot.
//!
//! A caller that wants a different one passes `Config::toolchain`, which
//! is the escape hatch for testing a newer format before the pin moves.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Which target to ask rustdoc to document for a given crate.
///
/// Both variants carry the *target* name because that — not the package
/// name — is what rustdoc derives the JSON payload's filename from. The
/// two usually coincide for libraries, but `[lib] name = "..."` breaks
/// the coincidence (Tauri v2 apps ship `<pkg>_lib` to dodge the
/// bin/lib filename clash on Windows, which is how the package-name
/// assumption surfaced as a missing-payload error in 0.1.0).
#[derive(Debug, Clone)]
pub(crate) enum Target<'a> {
    /// Build docs for the library target (`--lib`), carrying its name.
    Lib(&'a str),
    /// Build docs for the named binary target (`--bin <name>`).
    Bin(&'a str),
}

/// Run rustdoc for the given crate under `toolchain` and parse the
/// resulting JSON.
///
/// `crate_name` is the cargo package name (e.g. `aidoc-core`); it only
/// labels error messages. The emitted `target/doc/<name>.json` file is
/// located from the *target* name carried in [`Target`] (rustdoc
/// normalizes dashes to underscores), because a `[lib] name` override
/// makes the two diverge.
pub(crate) fn build_and_parse(
    target: &Target<'_>,
    crate_name: &str,
    manifest_path: &Path,
    workspace_root: &Path,
    toolchain: &str,
) -> Result<rustdoc_types::Crate> {
    // `rustup run <toolchain> cargo` rather than `cargo +<toolchain>`.
    // The two are equivalent when the rustup proxy is what gets
    // spawned, and only the first is reliable when it is not: a
    // `+toolchain` argument reaching a real `cargo` is an unknown
    // subcommand, which is how the same call fails on Windows for other
    // rustdoc-JSON consumers.
    let mut cmd = Command::new("rustup");
    cmd.args(["run", toolchain, "cargo", "rustdoc"]);

    match target {
        Target::Lib(_) => {
            cmd.arg("--lib");
        }
        Target::Bin(name) => {
            cmd.arg("--bin").arg(name);
        }
    }

    cmd.arg("--manifest-path").arg(manifest_path);
    cmd.args(["--", "-Zunstable-options", "--output-format", "json"]);
    cmd.current_dir(workspace_root);

    let output = cmd.output().map_err(|e| Error::RustdocInvocation {
        message: format!("failed to spawn `rustup run {toolchain} cargo rustdoc`: {e}"),
    })?;

    if !output.status.success() {
        return Err(Error::RustdocInvocation {
            message: format!(
                "`rustup run {toolchain} cargo rustdoc` exited with {status} for crate `{crate_name}`\nstderr:\n{stderr}",
                status = output.status,
                stderr = String::from_utf8_lossy(&output.stderr).trim(),
            ),
        });
    }

    let json_name = match target {
        Target::Lib(name) | Target::Bin(name) => name.replace('-', "_"),
    };
    let json_path = workspace_root
        .join("target")
        .join("doc")
        .join(format!("{json_name}.json"));

    if !json_path.exists() {
        return Err(Error::RustdocInvocation {
            message: format!(
                "`rustup run {toolchain} cargo rustdoc` reported success but the JSON payload was not found at {}",
                json_path.display()
            ),
        });
    }

    let bytes = std::fs::read(&json_path)?;
    let payload: rustdoc_types::Crate = serde_json::from_slice(&bytes)?;

    if payload.format_version != rustdoc_types::FORMAT_VERSION {
        return Err(Error::FormatVersionMismatch {
            expected: rustdoc_types::FORMAT_VERSION,
            found: payload.format_version,
            ran_under: toolchain.to_string(),
            required: crate::REQUIRED_NIGHTLY,
        });
    }

    Ok(payload)
}

/// Where rustdoc emits its JSON payload, given a workspace root.
///
/// `target_name` is the *target* name (lib or bin), not the package
/// name — the two diverge under a `[lib] name` override. Exposed for
/// callers that want to reason about the on-disk cache (for example, to
/// invalidate it before a re-run). Currently only used by tests.
#[allow(dead_code)]
pub(crate) fn json_path(workspace_root: &Path, target_name: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join("doc")
        .join(format!("{}.json", target_name.replace('-', "_")))
}
