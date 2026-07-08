//! Platform overlay dispatch: given a `Config::platforms` list, add
//! the platform-specific artifacts on top of the core `docs/aidoc/`
//! output produced by [`crate::generate::render_all`].
//!
//! The core output is intentionally platform-neutral (it follows
//! [llmstxt.org](https://llmstxt.org)). Overlays add the small extra
//! files each external doc service expects — for example, `context7.json`
//! at the repo root, or `.devin/wiki.json` for DeepWiki — without
//! changing the core layout. Callers opt in per platform via
//! `--platform <name>` on the CLI or `Config::platforms` in code.
//!
//! This file is intentionally a thin dispatcher; each overlay's actual
//! emitter lives in a follow-up phase.

use crate::config::Platform;
use crate::error::Result;
use crate::generate::Artifact;
use crate::index::IndexedWorkspace;

/// Apply every overlay in `platforms` to `artifacts` in the order they
/// appear. Overlays may append new artifacts, replace existing ones by
/// path, or mutate existing bodies in place; the dispatch loop takes no
/// position on how they combine, only on the order they run.
pub fn apply_overlays(
    workspace: &IndexedWorkspace,
    artifacts: &mut Vec<Artifact>,
    platforms: &[Platform],
) -> Result<()> {
    for platform in platforms {
        match platform {
            Platform::Context7 => {
                // Phase 10: emit context7.json at the repo root.
                let _ = workspace;
                let _ = artifacts;
            }
            Platform::DeepWiki => {
                // Phase 12: emit .devin/wiki.json when the workspace
                // is large enough to warrant it.
                let _ = workspace;
                let _ = artifacts;
            }
            Platform::AnthropicStyle => {
                // Phase 11: prepend a reverse cross-ref blockquote to
                // each per-module .md artifact.
                let _ = workspace;
                let _ = artifacts;
            }
        }
    }
    Ok(())
}
