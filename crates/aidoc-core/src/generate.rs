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
use crate::error_catalog::{ErrorEntry, Snippet};
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
///
/// The second return value carries the `llms-full.txt` size facts
/// (per-chunk breakdown, cap outcome), captured at the moment that
/// file was concatenated. See [`LlmsFullReport`] for why it cannot be
/// recomputed later.
pub fn render_all(
    workspace: &IndexedWorkspace,
    title: Option<&str>,
) -> Result<(Vec<Artifact>, LlmsFullReport)> {
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

    let llms_full = match workspace.llms_full_max_bytes {
        Some(cap) => {
            let (body, llms_full) = render_llms_full_capped(&artifacts, cap)?;
            artifacts.push(Artifact::in_out_dir("llms-full.txt", body));
            llms_full
        }
        None => {
            let body = render_llms_full(&artifacts);
            let llms_full = LlmsFullReport {
                cap_bytes: None,
                chunks: llms_full_chunk_sizes(&artifacts),
                dropped: Vec::new(),
                final_bytes: body.len(),
            };
            artifacts.push(Artifact::in_out_dir("llms-full.txt", body));
            llms_full
        }
    };

    // Last, and deliberately after `llms-full.txt` is rendered: this is
    // a record *about* the artifact set rather than a part of it. The
    // concatenation above takes `.md` only, so a `.json` here is safe
    // either way — the ordering says which of the two facts is load
    // bearing.
    if let Some(triple) = workspace.target_triple() {
        artifacts.push(Artifact::in_out_dir(
            crate::manifest::MANIFEST_PATH,
            crate::manifest::Manifest::new(triple).render()?,
        ));
    }

    Ok((artifacts, llms_full))
}

/// Append the error-catalog artifact set to `artifacts` in place.
///
/// Emits three families:
///
/// - `errors/<CODE>.md` — per-code narrative page: the message
///   template, `help` / `url` metadata, the original doc body, and
///   every tagged snippet grouped by its bare tags.
/// - `errors/index.json` — deterministic list of every `ErrorEntry`
///   for machine consumption (drift check, MCP fetch by code).
/// - `llms-errors.txt` — top-level index of every code, mirroring
///   `llms.txt`'s shape so LLM callers already conditioned on
///   [llmstxt.org](https://llmstxt.org) can consume it identically.
///
/// If `entries` is empty the function still emits the three artifacts
/// (with empty bodies / an empty JSON array) so `--check` can detect
/// the transition from "some errors" to "none". This mirrors the
/// treatment of empty `llms-full.txt`.
pub fn render_error_catalog(entries: &[ErrorEntry], artifacts: &mut Vec<Artifact>) -> Result<()> {
    for entry in entries {
        artifacts.push(Artifact::in_out_dir(
            format!("errors/{}.md", entry.code),
            render_error_entry_md(entry),
        ));
    }

    artifacts.push(Artifact::in_out_dir(
        "errors/index.json",
        render_error_index_json(entries)?,
    ));

    artifacts.push(Artifact::in_out_dir(
        "llms-errors.txt",
        render_llms_errors_txt(entries),
    ));

    Ok(())
}

fn render_error_entry_md(entry: &ErrorEntry) -> String {
    let mut out = String::new();
    writeln!(&mut out, "# {}", entry.code).unwrap();
    out.push('\n');

    if let Some(msg) = &entry.message_template {
        writeln!(&mut out, "**Message:** `{msg}`").unwrap();
        out.push('\n');
    }
    if let Some(help) = &entry.help {
        writeln!(&mut out, "**Help:** {help}").unwrap();
        out.push('\n');
    }
    if let Some(url) = &entry.url {
        writeln!(&mut out, "**Reference:** <{url}>").unwrap();
        out.push('\n');
    }
    writeln!(&mut out, "**Defined in:** `{}`", entry.item_path).unwrap();
    out.push('\n');

    if !entry.docs.trim().is_empty() {
        writeln!(&mut out, "## Description").unwrap();
        out.push('\n');
        out.push_str(&entry.docs);
        if !entry.docs.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    if !entry.snippets.is_empty() {
        writeln!(&mut out, "## Snippets").unwrap();
        out.push('\n');
        for snippet in &entry.snippets {
            let heading = snippet_heading(snippet);
            writeln!(&mut out, "### {heading}").unwrap();
            out.push('\n');
            writeln!(&mut out, "```{}", snippet.lang).unwrap();
            out.push_str(&snippet.body);
            if !snippet.body.ends_with('\n') {
                out.push('\n');
            }
            writeln!(&mut out, "```").unwrap();
            out.push('\n');
        }
    }

    out
}

fn snippet_heading(snippet: &Snippet) -> String {
    if snippet.tags.is_empty() {
        "Example".to_owned()
    } else {
        // Capitalise each tag for a readable heading, e.g.
        // `fix` -> `Fix`, `runtime` -> `Runtime`.
        snippet
            .tags
            .iter()
            .map(|tag| {
                let mut chars = tag.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn render_error_index_json(entries: &[ErrorEntry]) -> Result<String> {
    // `ErrorEntry` derives `Serialize` in the error_catalog module,
    // so we can round-trip it directly. Sorting is done at the
    // `error_catalog::extract` layer, so callers already see stable
    // order here.
    let json = serde_json::to_string_pretty(entries)?;
    Ok(format!("{json}\n"))
}

fn render_llms_errors_txt(entries: &[ErrorEntry]) -> String {
    let mut out = String::new();
    writeln!(&mut out, "# Error Catalog").unwrap();
    out.push('\n');
    if entries.is_empty() {
        out.push_str("> No diagnostics are catalogued yet.\n\n");
        return out;
    }
    writeln!(
        &mut out,
        "> {n} diagnostic{s} catalogued from `#[derive(miette::Diagnostic)]` items.",
        n = entries.len(),
        s = if entries.len() == 1 { "" } else { "s" },
    )
    .unwrap();
    out.push('\n');

    for entry in entries {
        writeln!(&mut out, "## {}", entry.code).unwrap();
        out.push('\n');
        let summary = entry
            .message_template
            .as_deref()
            .or(entry.help.as_deref())
            .unwrap_or("(no message)");
        writeln!(
            &mut out,
            "- [{code} · {path}](errors/{code}.md): {summary}",
            code = entry.code,
            path = entry.item_path,
        )
        .unwrap();
        out.push('\n');
    }

    out
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
    llms_full_chunks(artifacts)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

/// Build the chunk list `llms-full.txt` is concatenated from: one
/// `(path, text)` pair per markdown artifact, in emission order. The
/// text includes the `<!-- path -->` header and trailing blank line, so
/// `text.len()` is exactly what the chunk contributes to the file.
fn llms_full_chunks(artifacts: &[Artifact]) -> Vec<(String, String)> {
    let mut chunks = Vec::new();
    for artifact in artifacts {
        if !artifact.path.ends_with(".md") {
            continue;
        }
        let mut text = String::new();
        writeln!(&mut text, "<!-- {} -->", artifact.path).unwrap();
        text.push_str(&artifact.body);
        if !artifact.body.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        chunks.push((artifact.path.clone(), text));
    }
    chunks
}

/// The size of one `llms-full.txt` chunk, as reported by
/// [`llms_full_chunk_sizes`] and [`LlmsFullReport::dropped`].
///
/// `bytes` counts the whole chunk as it lands in the file — the
/// `<!-- path -->` header and trailing blank line included — so the
/// numbers sum to the file size and match what a byte cap counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSize {
    /// Artifact path of the chunk's source (e.g. `aidoc-core/index.md`).
    pub path: String,
    /// Bytes this chunk contributes to `llms-full.txt`.
    pub bytes: usize,
}

/// What the `llms-full.txt` render did, sizes included.
///
/// Captured at render time — recomputing from a finished artifact set
/// would count chunks (error-catalog pages, overlay edits) that were
/// appended or modified *after* `llms-full.txt` was concatenated, and
/// the numbers would stop matching the file. This is the data source
/// for `--size-report` and for the front ends' truncation notice.
#[derive(Debug, Clone)]
pub struct LlmsFullReport {
    /// The cap that was applied, in bytes; `None` when no cap was
    /// configured (the default).
    pub cap_bytes: Option<usize>,
    /// Every chunk the file was assembled from, in emission order,
    /// dropped ones included.
    pub chunks: Vec<ChunkSize>,
    /// Chunks omitted to fit the cap, in emission order. Empty when no
    /// cap is set or the file fit without truncation.
    pub dropped: Vec<ChunkSize>,
    /// Size of the emitted `llms-full.txt`, notice included. Always
    /// `<= cap_bytes` when a cap is set.
    pub final_bytes: usize,
}

impl LlmsFullReport {
    /// True when the cap actually removed content.
    pub fn truncated(&self) -> bool {
        !self.dropped.is_empty()
    }
}

/// Per-chunk byte breakdown of `llms-full.txt` for a given artifact
/// set. This is the `--size-report` data source; it deliberately reuses
/// the same chunk builder as the renderers, so the numbers are the
/// ones the cap counts.
pub fn llms_full_chunk_sizes(artifacts: &[Artifact]) -> Vec<ChunkSize> {
    llms_full_chunks(artifacts)
        .into_iter()
        .map(|(path, text)| ChunkSize {
            path,
            bytes: text.len(),
        })
        .collect()
}

/// Render `llms-full.txt` under a byte cap.
///
/// Chunks are kept in emission order and dropped from the end, one
/// whole chunk at a time — the file is never cut mid-document. When
/// anything is dropped, a notice listing every omitted chunk is
/// appended to the file itself, so the artifact self-reports what is
/// missing; the returned [`LlmsFullReport`] carries the same facts
/// for the front end. The result is a pure function of the inputs, so
/// `--check` drift detection is unaffected.
///
/// A cap too small to hold even the empty-file-plus-notice case is a
/// config error: emitting a file that silently exceeds the cap the
/// operator wrote would defeat the one thing the cap promises.
pub fn render_llms_full_capped(
    artifacts: &[Artifact],
    cap: usize,
) -> Result<(String, LlmsFullReport)> {
    let mut kept = llms_full_chunks(artifacts);
    let chunks: Vec<ChunkSize> = kept
        .iter()
        .map(|(path, text)| ChunkSize {
            path: path.clone(),
            bytes: text.len(),
        })
        .collect();
    let mut kept_bytes: usize = kept.iter().map(|(_, text)| text.len()).sum();
    let mut dropped: Vec<ChunkSize> = Vec::new();

    loop {
        let notice_len = truncation_notice(cap, &dropped).len();
        if kept_bytes + notice_len <= cap {
            break;
        }
        let Some((path, text)) = kept.pop() else {
            return Err(crate::error::Error::Config {
                message: format!(
                    "llms-full-max-bytes = {cap} cannot fit any content: the truncation \
                     notice alone needs {notice_len} bytes — raise the cap"
                ),
            });
        };
        kept_bytes -= text.len();
        // Popped from the end, so insert at the front to keep the
        // dropped list in emission order.
        dropped.insert(
            0,
            ChunkSize {
                path,
                bytes: text.len(),
            },
        );
    }

    let mut body: String = kept.into_iter().map(|(_, text)| text).collect();
    body.push_str(&truncation_notice(cap, &dropped));
    let final_bytes = body.len();

    Ok((
        body,
        LlmsFullReport {
            cap_bytes: Some(cap),
            chunks,
            dropped,
            final_bytes,
        },
    ))
}

/// The in-file notice a truncated `llms-full.txt` ends with. Empty when
/// nothing was dropped, so an untruncated capped render is
/// byte-identical to the uncapped one.
fn truncation_notice(cap: usize, dropped: &[ChunkSize]) -> String {
    if dropped.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    writeln!(
        &mut out,
        "<!-- TRUNCATED: llms-full-max-bytes = {cap}; {n} chunk(s) omitted: -->",
        n = dropped.len(),
    )
    .unwrap();
    for chunk in dropped {
        writeln!(
            &mut out,
            "<!-- omitted: {path} ({bytes} bytes) -->",
            path = chunk.path,
            bytes = chunk.bytes,
        )
        .unwrap();
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

/// Render a [DeepWiki](https://deepwiki.com) manifest (`.devin/wiki.json`)
/// for a workspace.
///
/// Emits `repo_notes` (a single workspace-overview note built from the
/// first crate root doc) and `pages[]` (one page per crate, plus a
/// workspace root page whose children point at each crate). Modules
/// are intentionally left out of the page list at v0.1: DeepWiki's
/// default page cap is 30, and generating a page per module tends to
/// exceed that on multi-crate workspaces without adding much wiki
/// signal. A `--deepwiki-expand` option that walks modules is left as
/// a follow-up phase.
///
/// The output is hard-capped at 30 pages so callers that pass
/// unusually large workspaces still produce a spec-valid manifest;
/// dropped crates are silently truncated rather than errored on
/// (DeepWiki auto-crawl still covers them via GitHub).
pub fn render_deepwiki_manifest(workspace: &IndexedWorkspace) -> String {
    const DEEPWIKI_PAGE_CAP: usize = 30;

    let workspace_name = workspace_title(workspace);
    let workspace_summary_line = workspace_summary(workspace);

    let mut pages = Vec::new();
    pages.push(DeepWikiPage {
        title: workspace_name.clone(),
        purpose: match workspace_summary_line {
            Some(s) => format!("Workspace overview: {s}"),
            None => "Workspace overview.".to_owned(),
        },
        parent: None,
    });

    for krate in &workspace.crates {
        let crate_summary = krate
            .root_module_doc
            .as_deref()
            .and_then(first_line)
            .unwrap_or("(no crate documentation)");
        pages.push(DeepWikiPage {
            title: format!("crate:{}", krate.name),
            purpose: format!("{}: {crate_summary}", krate.name),
            parent: Some(workspace_name.clone()),
        });
    }

    pages.truncate(DEEPWIKI_PAGE_CAP);

    let repo_notes = vec![DeepWikiNote {
        content: match workspace_summary_line {
            Some(s) => format!("Workspace `{workspace_name}` — {s}"),
            None => format!("Workspace `{workspace_name}`."),
        },
        author: None,
    }];

    let manifest = DeepWikiManifest { repo_notes, pages };
    let json = serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| String::from("{}"));
    format!("{json}\n")
}

#[derive(Serialize)]
struct DeepWikiManifest {
    repo_notes: Vec<DeepWikiNote>,
    pages: Vec<DeepWikiPage>,
}

#[derive(Serialize)]
struct DeepWikiNote {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
}

#[derive(Serialize)]
struct DeepWikiPage {
    title: String,
    purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
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

/// Compute the on-disk slug for a module path.
///
/// Reserves the bare literal `"index"` so a top-level module named
/// `index` (e.g. `aidoc_core::index`) does not collide with the
/// crate-root `<crate_slug>/index.md` document. Colliding names are
/// prefixed with an underscore (`index` → `_index`).
fn module_slug(path: &str) -> String {
    let slug = path.replace("::", "__");
    if slug == "index" {
        "_index".to_owned()
    } else {
        slug
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A top-level module named `index` must not resolve to a slug
    /// that collides with the crate-root `<crate>/index.md` document.
    /// The `aidoc-core` crate itself contains an `index` module, so
    /// this regression is dogfood-visible.
    #[test]
    fn module_slug_reserves_index_for_crate_root() {
        assert_eq!(module_slug("index"), "_index");
        // Nested `sub::index` is not at slug level `index`, so it
        // stays as-is (no collision with the crate root).
        assert_eq!(module_slug("sub::index"), "sub__index");
        // Normal module names round-trip unchanged.
        assert_eq!(module_slug("config"), "config");
    }

    fn md(path: &str, body: &str) -> Artifact {
        Artifact::in_out_dir(path, body)
    }

    fn fixture() -> Vec<Artifact> {
        // The last chunk is deliberately the big one, so a cap that
        // holds the first two plus a truncation notice forces exactly
        // one drop.
        let beta = "beta body line\n".repeat(40);
        vec![
            md("a/index.md", "alpha body\n"),
            md("a/one.md", "one body, somewhat longer than alpha\n"),
            md("b/index.md", &beta),
            // Non-markdown artifacts must never appear in llms-full.
            Artifact::in_out_dir("api/a.json", "{}\n"),
        ]
    }

    #[test]
    fn capped_render_that_fits_is_byte_identical_to_uncapped() {
        let artifacts = fixture();
        let uncapped = render_llms_full(&artifacts);
        let (body, truncation) = render_llms_full_capped(&artifacts, uncapped.len()).unwrap();
        assert_eq!(body, uncapped);
        assert!(truncation.dropped.is_empty());
        assert_eq!(truncation.final_bytes, uncapped.len());
    }

    #[test]
    fn overflow_drops_whole_chunks_from_the_end_and_self_reports() {
        let artifacts = fixture();
        let sizes = llms_full_chunk_sizes(&artifacts);
        assert_eq!(sizes.len(), 3, "json must not count as a chunk");

        // Cap that fits the first two chunks plus a notice, but not the
        // third: generous headroom, then check the exact outcome.
        let cap = sizes[0].bytes + sizes[1].bytes + 200;
        let (body, truncation) = render_llms_full_capped(&artifacts, cap).unwrap();

        assert!(truncation.final_bytes <= cap);
        assert_eq!(body.len(), truncation.final_bytes);
        assert_eq!(
            truncation
                .dropped
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["b/index.md"],
        );
        // Kept chunks are intact (no mid-chunk cut) and in order.
        assert!(body.starts_with("<!-- a/index.md -->\nalpha body\n"));
        assert!(body.contains("<!-- a/one.md -->"));
        // The file itself says what is missing.
        assert!(body.contains(&format!(
            "<!-- TRUNCATED: llms-full-max-bytes = {cap}; 1 chunk(s) omitted: -->"
        )));
        assert!(body.contains(&format!(
            "<!-- omitted: b/index.md ({} bytes) -->",
            sizes[2].bytes
        )));
    }

    #[test]
    fn cap_too_small_for_any_content_is_a_config_error() {
        let artifacts = fixture();
        let err = render_llms_full_capped(&artifacts, 1).unwrap_err();
        assert!(
            matches!(err, crate::error::Error::Config { .. }),
            "got: {err}"
        );
    }

    #[test]
    fn truncation_is_deterministic() {
        let artifacts = fixture();
        let cap = 300;
        let first = render_llms_full_capped(&artifacts, cap).unwrap();
        let second = render_llms_full_capped(&artifacts, cap).unwrap();
        assert_eq!(first.0, second.0);
    }
}
