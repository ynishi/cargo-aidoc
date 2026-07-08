//! Generate stage: project an [`IndexedWorkspace`] into LLM-facing
//! artifacts.
//!
//! Each artifact has its own render function. The Preset::Publish set
//! calls these in a fixed order (`llms.txt`, per-crate markdown,
//! `llms-full.txt`, `api/<crate>.json`). At this point only `llms.txt`
//! is implemented; other renderers land in follow-up phases.

use std::fmt::Write as _;

use rustdoc_types::{Item, ItemEnum, Module, Visibility};

use crate::index::{IndexedCrate, IndexedWorkspace};

/// Render the top-level `llms.txt` index for a workspace.
///
/// The output follows the [llmstxt.org](https://llmstxt.org) structure:
/// an H1 title, an optional blockquote summary, then one H2 section per
/// crate whose bullet list points at the per-crate / per-module markdown
/// documents that the narrative renderer will emit later.
///
/// The `title` argument overrides the default heading (which is the
/// basename of the workspace root). Pass `None` to accept the default.
pub fn render_llms_txt(workspace: &IndexedWorkspace, title: Option<&str>) -> String {
    let mut out = String::new();

    let heading = title
        .map(str::to_owned)
        .unwrap_or_else(|| workspace_title(workspace));
    writeln!(&mut out, "# {heading}").unwrap();
    out.push('\n');

    if let Some(summary) = workspace_summary(workspace) {
        writeln!(&mut out, "> {summary}").unwrap();
        out.push('\n');
    }

    for krate in &workspace.crates {
        writeln!(&mut out, "## {} {}", krate.name, krate.version).unwrap();
        out.push('\n');

        let crate_slug = crate_slug(&krate.name);
        let overview_summary = krate
            .root_module_doc
            .as_deref()
            .and_then(first_line)
            .unwrap_or("(no crate-root documentation)");
        writeln!(
            &mut out,
            "- [{name} overview]({slug}/index.md): {summary}",
            name = krate.name,
            slug = crate_slug,
            summary = overview_summary,
        )
        .unwrap();

        for module in public_modules(krate) {
            let module_summary = module
                .docs
                .and_then(first_line)
                .unwrap_or("(no module-level documentation)");
            let module_slug = module_slug(&module.path);
            writeln!(
                &mut out,
                "- [{name}::{path}]({slug}/{module_slug}.md): {summary}",
                name = krate.name,
                path = module.path,
                slug = crate_slug,
                module_slug = module_slug,
                summary = module_summary,
            )
            .unwrap();
        }
        out.push('\n');
    }

    out
}

/// Fall-back workspace heading when the caller didn't override.
fn workspace_title(workspace: &IndexedWorkspace) -> String {
    workspace
        .root
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "workspace".to_owned())
}

/// The first line of the first crate's root doc, if any. Used as the
/// blockquote summary under the workspace title.
fn workspace_summary(workspace: &IndexedWorkspace) -> Option<&str> {
    let raw = workspace
        .crates
        .iter()
        .find_map(|c| c.root_module_doc.as_deref())?;
    first_line(raw)
}

/// The lowercase, dash-free crate slug used for on-disk paths in the
/// generated tree.
fn crate_slug(name: &str) -> String {
    name.replace('-', "_")
}

/// A dotted module path (e.g. `config`, `net::tcp`) rendered as a
/// filesystem-safe slug (`config`, `net__tcp`).
fn module_slug(path: &str) -> String {
    path.replace("::", "__")
}

/// Return the first non-empty line of a doc comment, trimmed. Used to
/// derive short summaries for link bullet lists.
fn first_line(doc: &str) -> Option<&str> {
    doc.lines().map(str::trim).find(|line| !line.is_empty())
}

/// A public module surfaced under a crate root, with its dotted path
/// (`config::merge`) and its own doc comment.
struct PublicModule<'a> {
    path: String,
    docs: Option<&'a str>,
}

/// Enumerate all public modules reachable from the crate root, depth-first.
/// The crate root itself is not included (it's rendered separately as the
/// crate overview link).
fn public_modules(krate: &IndexedCrate) -> Vec<PublicModule<'_>> {
    let mut out = Vec::new();
    let index = &krate.crate_data.index;

    let Some(root_item) = index.get(&krate.crate_data.root) else {
        return out;
    };
    let ItemEnum::Module(root_module) = &root_item.inner else {
        return out;
    };

    walk_module(index, root_module, String::new(), &mut out);
    out
}

fn walk_module<'a>(
    index: &'a std::collections::HashMap<rustdoc_types::Id, Item>,
    module: &'a Module,
    prefix: String,
    out: &mut Vec<PublicModule<'a>>,
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

        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}::{name}")
        };

        out.push(PublicModule {
            path: path.clone(),
            docs: child.docs.as_deref(),
        });

        walk_module(index, child_module, path, out);
    }
}
