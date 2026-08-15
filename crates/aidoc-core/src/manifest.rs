//! What the committed artifacts *are*: the target triple they describe.
//!
//! # Why an artifact set belongs to one target
//!
//! rustdoc documents a crate for one target at a time, and `cfg` is
//! resolved before rustdoc ever sees the item tree. A module behind
//! `#[cfg(target_os = "macos")]` is in the JSON payload on a Mac and
//! absent everywhere else, so the artifacts generated from that payload
//! are not a property of the source alone — they are a property of the
//! source *and* the host that ran rustdoc.
//!
//! Without a record of which host that was, the two front ends cannot
//! tell these apart:
//!
//! - the committed artifacts are stale (somebody added a module and did
//!   not regenerate) — the drift this tool exists to catch;
//! - the committed artifacts are fine and this host simply resolves
//!   `cfg` differently — no drift at all, and nothing the person running
//!   the check can fix by regenerating.
//!
//! `--check` reported both as drift, which makes a red CI on the second
//! case a message nobody can act on. Worse is the generate side: a run
//! on the other host silently rewrote every artifact for its own `cfg`
//! resolution, deleting the modules the recorded host documents. That
//! deletion looks exactly like a regeneration in a diff, which is how it
//! ships.
//!
//! So the artifact set records its triple, in [`MANIFEST_PATH`], and
//! both front ends compare it against the payload's own
//! `Crate::target::triple` before doing anything. See [`TargetVerdict`]
//! for the three answers and what each front end does with them.
//!
//! # What this deliberately does not do
//!
//! It does not make generation target-independent. Documenting a
//! `#[cfg(target_os = "macos")]` module from Linux would mean running
//! rustdoc under `--target aarch64-apple-darwin`, which needs that
//! target's std and every build script in the dependency graph to
//! cross-compile — available to a CI matrix, not to a person on one
//! laptop. One canonical target per artifact set, stated rather than
//! assumed, is what this buys.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Where the target record lives, relative to the output directory.
///
/// A separate file rather than a field inside `llms.txt` or one of the
/// `api/<crate>.json` payloads: those are read by LLMs and by diff
/// tooling respectively, and neither should have to carry a fact about
/// the build host to answer the question they exist for.
pub const MANIFEST_PATH: &str = "aidoc-manifest.json";

/// The current [`Manifest::schema`] value.
pub const MANIFEST_SCHEMA: u32 = 1;

/// The record written alongside the artifacts: what target they
/// describe.
///
/// Deliberately minimal. A generator version or a timestamp here would
/// rewrite a committed file on every upgrade and every run, turning the
/// drift check into noise; the triple changes only when the answer to
/// "which host are these artifacts for" changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version of this file ([`MANIFEST_SCHEMA`]).
    pub schema: u32,
    /// The target triple rustdoc resolved `cfg` for, taken from the
    /// payload's own `Crate::target::triple` rather than from the
    /// host this process happens to run on.
    pub target: String,
}

impl Manifest {
    /// A manifest for `triple`, at the current schema version.
    pub fn new(triple: impl Into<String>) -> Self {
        Self {
            schema: MANIFEST_SCHEMA,
            target: triple.into(),
        }
    }

    /// Render to the exact bytes written to [`MANIFEST_PATH`]: pretty
    /// JSON with a trailing newline, matching every other JSON artifact
    /// this crate emits.
    pub fn render(&self) -> Result<String> {
        let mut body = serde_json::to_string_pretty(self)?;
        body.push('\n');
        Ok(body)
    }
}

/// What comparing the committed manifest against a fresh run found.
///
/// Front ends branch on this *before* writing or diffing, because on
/// [`Mismatch`](Self::Mismatch) neither operation means what it usually
/// means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetVerdict {
    /// The committed manifest names the target this run resolved `cfg`
    /// for. Write and check both mean what they always meant.
    Match,

    /// No manifest on disk: either a first run, or an artifact set
    /// committed by a version of this tool that predates the record.
    ///
    /// Treated as permission to proceed, not as a mismatch — a fence
    /// that fired here would block every existing repository from ever
    /// recording a triple. The consequence is that the *first*
    /// generation after upgrading is unfenced, so it should happen on
    /// the host the artifacts already belong to.
    Unrecorded,

    /// The committed manifest names a different target than this run
    /// resolved `cfg` for. Any drift found is unactionable, and any
    /// write would drop what the recorded target documents.
    Mismatch {
        /// Triple named by the committed manifest.
        recorded: String,
        /// Triple this run's rustdoc payload was generated for.
        generated: String,
    },
}

impl TargetVerdict {
    /// The mismatch case stated as the one fact both front ends open
    /// with: which target the artifacts describe, and which one this
    /// run documented.
    ///
    /// Only the fact. What follows from it differs by front end and by
    /// mode — a check cannot answer, a write would delete — and a
    /// consequence phrased for one of them reads as a non-sequitur in
    /// the other, so each supplies its own.
    ///
    /// `None` for the two variants that need no explanation.
    pub fn explain(&self) -> Option<String> {
        match self {
            Self::Match | Self::Unrecorded => None,
            Self::Mismatch {
                recorded,
                generated,
            } => Some(format!(
                "the committed artifacts describe {recorded}, this run documented {generated}."
            )),
        }
    }
}

/// Read the committed manifest from `out_dir`, if there is one.
///
/// A missing file is [`None`] rather than an error: see
/// [`TargetVerdict::Unrecorded`]. A file that exists but does not parse
/// *is* an error — it is a record this tool wrote, and guessing past a
/// corrupted one would put the fence back to sleep silently.
pub fn read(out_dir: &Path) -> Result<Option<Manifest>> {
    let path = out_dir.join(MANIFEST_PATH);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    serde_json::from_slice(&bytes).map_err(|source| Error::Manifest {
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

/// Compare the committed manifest in `out_dir` against the triple this
/// run generated for.
///
/// `generated` is [`None`] when the run indexed no crates and so has no
/// payload to take a triple from; there is nothing to compare and
/// nothing to write, so that is [`TargetVerdict::Unrecorded`].
pub fn verdict(out_dir: &Path, generated: Option<&str>) -> Result<TargetVerdict> {
    let Some(generated) = generated else {
        return Ok(TargetVerdict::Unrecorded);
    };
    let Some(manifest) = read(out_dir)? else {
        return Ok(TargetVerdict::Unrecorded);
    };

    if manifest.target == generated {
        Ok(TargetVerdict::Match)
    } else {
        Ok(TargetVerdict::Mismatch {
            recorded: manifest.target,
            generated: generated.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(MANIFEST_PATH), body).unwrap();
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aidoc-manifest-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The round trip is what the fence rests on: what `render` writes,
    /// `read` has to recognise.
    #[test]
    fn a_rendered_manifest_reads_back_as_itself() {
        let dir = temp_dir("round-trip");
        let manifest = Manifest::new("aarch64-apple-darwin");
        write_manifest(&dir, &manifest.render().unwrap());

        assert_eq!(read(&dir).unwrap(), Some(manifest));
    }

    /// An artifact set committed before this tool recorded a triple has
    /// no manifest, and must still be generatable — otherwise the fence
    /// locks every existing repository out of ever getting one.
    #[test]
    fn a_missing_manifest_is_permission_to_proceed_not_a_mismatch() {
        let dir = temp_dir("unrecorded");
        assert_eq!(
            verdict(&dir, Some("x86_64-unknown-linux-gnu")).unwrap(),
            TargetVerdict::Unrecorded
        );
    }

    /// The case this module exists for: a Linux run against artifacts a
    /// Mac generated is not drift, and must not be reported as any.
    #[test]
    fn a_different_host_is_a_mismatch_rather_than_drift() {
        let dir = temp_dir("mismatch");
        write_manifest(
            &dir,
            &Manifest::new("aarch64-apple-darwin").render().unwrap(),
        );

        let verdict = verdict(&dir, Some("x86_64-unknown-linux-gnu")).unwrap();
        assert_eq!(
            verdict,
            TargetVerdict::Mismatch {
                recorded: "aarch64-apple-darwin".to_string(),
                generated: "x86_64-unknown-linux-gnu".to_string(),
            }
        );
        assert!(verdict.explain().is_some());
    }

    /// Same host, same answer as before this module existed.
    #[test]
    fn the_recorded_host_still_gets_a_real_check() {
        let dir = temp_dir("match");
        write_manifest(
            &dir,
            &Manifest::new("x86_64-unknown-linux-gnu").render().unwrap(),
        );

        assert_eq!(
            verdict(&dir, Some("x86_64-unknown-linux-gnu")).unwrap(),
            TargetVerdict::Match
        );
        assert_eq!(
            verdict(&dir, Some("x86_64-unknown-linux-gnu"))
                .unwrap()
                .explain(),
            None
        );
    }

    /// A corrupted record is not silently treated as absent: that would
    /// disable the fence on exactly the repository whose record is
    /// broken.
    #[test]
    fn an_unparseable_manifest_is_an_error_rather_than_an_absence() {
        let dir = temp_dir("corrupt");
        write_manifest(&dir, "{ this is not json");

        assert!(matches!(read(&dir), Err(Error::Manifest { .. })));
    }
}
