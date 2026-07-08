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
use crate::generate::{self, Artifact};
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
                let body = generate::render_context7_manifest(workspace);
                artifacts.push(Artifact::in_workspace_root("context7.json", body));
            }
            Platform::DeepWiki => {
                // Phase 12: emit .devin/wiki.json when the workspace
                // is large enough to warrant it.
                let _ = workspace;
                let _ = artifacts;
            }
            Platform::AnthropicStyle => {
                apply_anthropic_style(artifacts);
            }
        }
    }
    Ok(())
}

/// Prepend a reverse cross-ref blockquote to every markdown artifact
/// and to `llms-full.txt`, mirroring Anthropic's `platform.claude.com`
/// docs layout where every `.md` mirror carries a hint back to the
/// top-level index.
///
/// The link uses a filesystem-relative path (`../llms.txt` for
/// per-crate / per-module pages, `llms.txt` for artifacts that live at
/// the output root). A later phase can add a `--base-url` option to
/// swap those relative paths for absolute URLs when the crate author
/// knows the publish target.
fn apply_anthropic_style(artifacts: &mut [Artifact]) {
    for artifact in artifacts.iter_mut() {
        if !is_markdown_target(&artifact.path) {
            continue;
        }
        let link = if artifact.path.contains('/') {
            "../llms.txt"
        } else {
            "llms.txt"
        };
        let hint = format!("> Fetch the complete documentation index at: {link}\n\n");
        let mut new_body = String::with_capacity(hint.len() + artifact.body.len());
        new_body.push_str(&hint);
        new_body.push_str(&artifact.body);
        artifact.body = new_body;
    }
}

fn is_markdown_target(path: &str) -> bool {
    path.ends_with(".md") || path == "llms-full.txt"
}
