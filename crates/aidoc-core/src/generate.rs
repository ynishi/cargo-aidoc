//! Generate stage: project an [`IndexedWorkspace`] into LLM-facing
//! artifacts.
//!
//! The Preset::Publish set produces five artifact families:
//!
//! - `llms.txt` (top-level index, [llmstxt.org](https://llmstxt.org))
//! - `<crate>/index.md` (narrative for each crate root)
//! - `<crate>/<module>.md` (narrative + public-item reference per module)
//! - `llms-full.txt` (all markdown concatenated, chunk-delimited)
//! - `api/<crate>.json` (deterministic public-API surface)
//!
//! Each artifact has its own render function; [`render_all`] wires them
//! together and returns a deterministic list of `(relative_path, body)`
//! pairs the caller can write to disk (or diff against a checked-in
//! tree in `--check` mode). The generate stage never touches the
//! filesystem itself.

use std::fmt::Write as _;

use rustdoc_types::{Item, ItemEnum, Module, Visibility};
use serde::Serialize;

use crate::error::Result;
use crate::index::{IndexedCrate, IndexedWorkspace};

/// One generated artifact ready to be written to disk.
///
/// The `path` is relative to the base directory chosen by
/// [`location`](Self::location). The `body` is the exact bytes to
/// write; the renderer already appended a trailing newline where the
/// artifact family expects one.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Which base directory `path` is relative to.
    pub location: ArtifactLocation,
    /// Path relative to the artifact's base directory (uses forward
    /// slashes).
    pub path: String,
    /// Full file contents.
    pub body: String,
}

/// Which base directory an [`Artifact`]'s path is relative to.
///
/// Core `Preset::Publish` output goes into [`OutDir`](Self::OutDir).
/// Platform overlays that need to place a manifest at the repository
/// root (e.g. `context7.json`, `.devin/wiki.json`) use
/// [`WorkspaceRoot`](Self::WorkspaceRoot) so callers know not to
/// collapse them under `out_dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtifactLocation {
    /// Path is relative to `Config::out_dir`
    /// (default `<workspace>/docs/aidoc/`).
    #[default]
    OutDir,
    /// Path is relative to the workspace root (the parent of the
    /// top-level `Cargo.toml`).
    WorkspaceRoot,
}

impl Artifact {
    /// Convenience constructor for artifacts that land under
    /// `Config::out_dir`.
    pub fn in_out_dir(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            location: ArtifactLocation::OutDir,
            path: path.into(),
            body: body.into(),
        }
    }

    /// Convenience constructor for artifacts that land at the
    /// workspace root (typically platform manifests).
    pub fn in_workspace_root(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            location: ArtifactLocation::WorkspaceRoot,
            path: path.into(),
            body: body.into(),
        }
    }
}

/// Render every artifact in the Preset::Publish set for a workspace.
///
/// The returned artifacts appear in a stable order: `llms.txt` first,
/// then `<crate>/index.md` and `<crate>/<module>.md` for each crate in
/// discovery order, then `api/<crate>.json` for each crate, and finally
/// `llms-full.txt`. Callers that want to write only a subset can filter
/// by `path`.
pub fn render_all(workspace: &IndexedWorkspace, title: Option<&str>) -> Result<Vec<Artifact>> {
    let mut artifacts = Vec::new();

    artifacts.push(Artifact::in_out_dir(
        "llms.txt",
        render_llms_txt(workspace, title),
    ));

    for krate in &workspace.crates {
        let slug = crate_slug(&krate.name);
        artifacts.push(Artifact::in_out_dir(
            format!("{slug}/index.md"),
            render_crate_index(krate),
        ));

        for module in public_modules(krate) {
            artifacts.push(Artifact::in_out_dir(
                format!("{slug}/{}.md", module_slug(&module.path)),
                render_module(krate, &module),
            ));
        }
    }

    for krate in &workspace.crates {
        artifacts.push(Artifact::in_out_dir(
            format!("api/{}.json", crate_slug(&krate.name)),
            render_api_json(krate)?,
        ));
    }

    artifacts.push(Artifact::in_out_dir(
        "llms-full.txt",
        render_llms_full(&artifacts),
    ));

    Ok(artifacts)
}

/// Render the top-level `llms.txt` index for a workspace.
///
/// The output follows the [llmstxt.org](https://llmstxt.org) structure:
/// an H1 title, an optional blockquote summary, then one H2 section per
/// crate whose bullet list points at the per-crate / per-module markdown
/// documents that the narrative renderer emits.
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
            writeln!(
                &mut out,
                "- [{name}::{path}]({slug}/{module_slug}.md): {summary}",
                name = krate.name,
                path = module.path,
                slug = crate_slug,
                module_slug = module_slug(&module.path),
                summary = module_summary,
            )
            .unwrap();
        }
        out.push('\n');
    }

    out
}

/// Render the per-crate `index.md` narrative document.
///
/// The document contains the full crate-root doc comment followed by a
/// module list. Missing docs show a short placeholder so downstream
/// readers can tell "no docs" from "docs deliberately empty".
pub fn render_crate_index(krate: &IndexedCrate) -> String {
    let mut out = String::new();
    writeln!(&mut out, "# {} {}", krate.name, krate.version).unwrap();
    out.push('\n');

    match krate.root_module_doc.as_deref() {
        Some(doc) => {
            out.push_str(doc);
            if !doc.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        None => {
            out.push_str("_This crate has no crate-root documentation._\n\n");
        }
    }

    let modules = public_modules(krate);
    if !modules.is_empty() {
        writeln!(&mut out, "## Modules").unwrap();
        out.push('\n');
        for module in &modules {
            let module_summary = module
                .docs
                .and_then(first_line)
                .unwrap_or("(no module-level documentation)");
            writeln!(
                &mut out,
                "- [`{path}`]({slug}.md): {summary}",
                path = module.path,
                slug = module_slug(&module.path),
                summary = module_summary,
            )
            .unwrap();
        }
        out.push('\n');
    }

    out
}

/// Render the per-module narrative document.
///
/// Each module document contains the module-level doc comment followed by
/// a public-item reference organised by kind (functions, types, traits,
/// constants, macros). Item summaries are the first non-empty line of the
/// corresponding doc comment; items with no docs show a placeholder.
pub fn render_module(krate: &IndexedCrate, module: &PublicModule<'_>) -> String {
    let mut out = String::new();
    writeln!(&mut out, "# {}::{}", krate.name, module.path).unwrap();
    out.push('\n');

    match module.docs {
        Some(doc) => {
            out.push_str(doc);
            if !doc.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        None => {
            out.push_str("_This module has no module-level documentation._\n\n");
        }
    }

    let items = module_items(krate, module);
    render_item_group(&mut out, "Functions", ItemKind::Function, &items);
    render_item_group(&mut out, "Types", ItemKind::Type, &items);
    render_item_group(&mut out, "Traits", ItemKind::Trait, &items);
    render_item_group(&mut out, "Constants", ItemKind::Constant, &items);
    render_item_group(&mut out, "Macros", ItemKind::Macro, &items);

    out
}

/// Render `llms-full.txt`: every markdown artifact concatenated, with a
/// short header before each chunk so the reader can identify boundaries.
///
/// This intentionally skips non-markdown artifacts (`llms.txt` itself and
/// any future JSON payloads) — those already have their own well-known
/// paths, and duplicating them here would only bloat the file.
pub fn render_llms_full(artifacts: &[Artifact]) -> String {
    let mut out = String::new();
    for artifact in artifacts {
        if !artifact.path.ends_with(".md") {
            continue;
        }
        writeln!(&mut out, "<!-- {} -->", artifact.path).unwrap();
        out.push_str(&artifact.body);
        if !artifact.body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Render a [Context7](https://context7.com) manifest (`context7.json`)
/// for a workspace.
///
/// Emits the minimum useful field set: `$schema`, `projectTitle`,
/// `description` (first non-empty line of the first crate's root doc,
/// truncated to Context7's 10-200 character range), and `folders`
/// (pointing at cargo-aidoc's default output tree). Callers place the
/// returned body at the workspace root; the [`crate::platform`]
/// dispatcher wires this up when [`crate::Platform::Context7`] is
/// selected.
pub fn render_context7_manifest(workspace: &IndexedWorkspace) -> String {
    let manifest = Context7Manifest {
        schema: "https://context7.com/schema/context7.json",
        project_title: workspace_title(workspace),
        description: workspace_summary(workspace).and_then(clamp_description),
        folders: vec!["docs/aidoc".to_owned()],
    };
    let json = serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| String::from("{}"));
    format!("{json}\n")
}

#[derive(Serialize)]
struct Context7Manifest {
    #[serde(rename = "$schema")]
    schema: &'static str,
    #[serde(rename = "projectTitle")]
    project_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    folders: Vec<String>,
}

/// Truncate / drop a description string to fit Context7's 10-200 char
/// contract. Under 10 characters gets dropped (Context7 rejects short
/// descriptions); over 200 gets truncated to 199 + `…`.
fn clamp_description(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let char_count = trimmed.chars().count();
    if char_count < 10 {
        return None;
    }
    if char_count <= 200 {
        return Some(trimmed.to_owned());
    }
    let mut out = String::with_capacity(200);
    for ch in trimmed.chars().take(199) {
        out.push(ch);
    }
    out.push('…');
    Some(out)
}

/// Render the deterministic public-API surface JSON for a single crate.
///
/// The document intentionally excludes non-public items, unnameable
/// items (impls, imports, associated items), and any information that
/// changes across rustdoc runs without corresponding source changes
/// (spans, IDs, etc.). Callers use this as the check target for
/// `--check --strict`: if two runs produce different JSON, something in
/// the crate's public surface actually moved.
///
/// Items are sorted lexicographically by path so diffs read cleanly.
pub fn render_api_json(krate: &IndexedCrate) -> Result<String> {
    let mut items = Vec::new();
    let index = &krate.crate_data.index;

    if let Some(root_item) = index.get(&krate.crate_data.root)
        && let ItemEnum::Module(root_module) = &root_item.inner
    {
        let crate_prefix = crate_slug(&krate.name);
        walk_api_items(index, root_module, crate_prefix, &mut items);
    }

    items.sort_by(|a, b| a.path.cmp(&b.path));

    let surface = ApiSurface {
        krate: krate.name.clone(),
        version: krate.version.clone(),
        items,
    };
    let json = serde_json::to_string_pretty(&surface)?;
    Ok(format!("{json}\n"))
}

#[derive(Serialize)]
struct ApiSurface {
    #[serde(rename = "crate")]
    krate: String,
    version: String,
    items: Vec<ApiItem>,
}

#[derive(Serialize)]
struct ApiItem {
    path: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    docs: Option<String>,
}

fn walk_api_items(
    index: &std::collections::HashMap<rustdoc_types::Id, Item>,
    module: &Module,
    prefix: String,
    out: &mut Vec<ApiItem>,
) {
    for child_id in &module.items {
        let Some(child) = index.get(child_id) else {
            continue;
        };
        if !matches!(child.visibility, Visibility::Public) {
            continue;
        }
        let Some(name) = child.name.as_deref() else {
            continue;
        };
        let path = format!("{prefix}::{name}");

        if let Some(kind) = api_kind(&child.inner) {
            out.push(ApiItem {
                path: path.clone(),
                kind,
                docs: child.docs.clone(),
            });
        }

        if let ItemEnum::Module(child_module) = &child.inner {
            walk_api_items(index, child_module, path, out);
        }
    }
}

fn api_kind(inner: &ItemEnum) -> Option<&'static str> {
    match inner {
        ItemEnum::Module(_) => Some("module"),
        ItemEnum::Function(_) => Some("function"),
        ItemEnum::Struct(_) => Some("struct"),
        ItemEnum::Enum(_) => Some("enum"),
        ItemEnum::Union(_) => Some("union"),
        ItemEnum::Trait(_) => Some("trait"),
        ItemEnum::TypeAlias(_) => Some("type_alias"),
        ItemEnum::Constant { .. } => Some("constant"),
        ItemEnum::Static(_) => Some("static"),
        ItemEnum::Macro(_) => Some("macro"),
        ItemEnum::ProcMacro(_) => Some("proc_macro"),
        _ => None,
    }
}

// -------- helpers --------

fn workspace_title(workspace: &IndexedWorkspace) -> String {
    workspace
        .root
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "workspace".to_owned())
}

fn workspace_summary(workspace: &IndexedWorkspace) -> Option<&str> {
    let raw = workspace
        .crates
        .iter()
        .find_map(|c| c.root_module_doc.as_deref())?;
    first_line(raw)
}

fn crate_slug(name: &str) -> String {
    name.replace('-', "_")
}

fn module_slug(path: &str) -> String {
    path.replace("::", "__")
}

fn first_line(doc: &str) -> Option<&str> {
    doc.lines().map(str::trim).find(|line| !line.is_empty())
}

/// A public module surfaced under a crate root.
#[derive(Debug, Clone)]
pub struct PublicModule<'a> {
    /// Dotted module path (`config`, `net::tcp`).
    pub path: String,
    /// The module's own doc comment, if any.
    pub docs: Option<&'a str>,
    /// The rustdoc id of the module item, used to look up its children.
    id: rustdoc_types::Id,
}

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
            id: *child_id,
        });

        walk_module(index, child_module, path, out);
    }
}

/// Classification of a public item for grouping under module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Function,
    Type,
    Trait,
    Constant,
    Macro,
}

struct RenderedItem<'a> {
    name: &'a str,
    summary: &'a str,
    kind: ItemKind,
}

fn classify(item: &Item) -> Option<ItemKind> {
    match &item.inner {
        ItemEnum::Function(_) => Some(ItemKind::Function),
        ItemEnum::Struct(_) | ItemEnum::Enum(_) | ItemEnum::Union(_) | ItemEnum::TypeAlias(_) => {
            Some(ItemKind::Type)
        }
        ItemEnum::Trait(_) => Some(ItemKind::Trait),
        ItemEnum::Constant { .. } | ItemEnum::Static(_) => Some(ItemKind::Constant),
        ItemEnum::Macro(_) | ItemEnum::ProcMacro(_) => Some(ItemKind::Macro),
        _ => None,
    }
}

fn module_items<'a>(krate: &'a IndexedCrate, module: &PublicModule<'_>) -> Vec<RenderedItem<'a>> {
    let mut out = Vec::new();
    let index = &krate.crate_data.index;

    let Some(item) = index.get(&module.id) else {
        return out;
    };
    let ItemEnum::Module(module_data) = &item.inner else {
        return out;
    };

    for child_id in &module_data.items {
        let Some(child) = index.get(child_id) else {
            continue;
        };
        if !matches!(child.visibility, Visibility::Public) {
            continue;
        }
        let Some(kind) = classify(child) else {
            continue;
        };
        let Some(name) = child.name.as_deref() else {
            continue;
        };
        let summary = child
            .docs
            .as_deref()
            .and_then(first_line)
            .unwrap_or("(no documentation)");
        out.push(RenderedItem {
            name,
            summary,
            kind,
        });
    }

    // Deterministic within-kind order: alphabetic by name.
    out.sort_by(|a, b| a.name.cmp(b.name));
    out
}

fn render_item_group(out: &mut String, heading: &str, kind: ItemKind, items: &[RenderedItem<'_>]) {
    let group: Vec<&RenderedItem<'_>> = items.iter().filter(|i| i.kind == kind).collect();
    if group.is_empty() {
        return;
    }
    writeln!(out, "## {heading}").unwrap();
    out.push('\n');
    for item in group {
        writeln!(out, "- `{}` — {}", item.name, item.summary).unwrap();
    }
    out.push('\n');
}
