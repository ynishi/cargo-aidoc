//! Invoke `cargo +nightly rustdoc --output-format json` and parse the
//! resulting payload via `rustdoc-types`.
//!
//! rustdoc's JSON output is unstable and toolchain-locked: the payload's
//! `format_version` must match the `rustdoc-types` crate this binary was
//! built against. Any mismatch is surfaced as
//! [`Error::FormatVersionMismatch`] rather than silently producing bad
//! output. Bumping the `rustdoc-types` dependency (and, if necessary, the
//! nightly toolchain the user runs) is the intended fix.

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

/// Run `cargo +nightly rustdoc` for the given crate and parse the
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
) -> Result<rustdoc_types::Crate> {
    let mut cmd = Command::new("cargo");
    cmd.arg("+nightly").arg("rustdoc");

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
        message: format!("failed to spawn `cargo +nightly rustdoc`: {e}"),
    })?;

    if !output.status.success() {
        return Err(Error::RustdocInvocation {
            message: format!(
                "`cargo +nightly rustdoc` exited with {status} for crate `{crate_name}`\nstderr:\n{stderr}",
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
                "`cargo +nightly rustdoc` reported success but the JSON payload was not found at {}",
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
