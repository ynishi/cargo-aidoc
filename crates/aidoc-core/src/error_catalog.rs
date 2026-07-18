//! Error-catalog extractor: build an LLM-facing catalog of every
//! diagnostic emitted by the workspace, straight from rustdoc JSON.
//!
//! ## Consumer contract (zero coupling)
//!
//! Consumer crates need **no dependency on `cargo-aidoc`**. To have a
//! struct or enum picked up as an error entry, the crate simply:
//!
//! 1. Derives `miette::Diagnostic` on the type (already the de facto
//!    Rust standard for structured diagnostics).
//! 2. Passes `code(...)` inside `#[diagnostic(...)]`. `help(...)` and
//!    `url(...)` are optional but recommended — they land verbatim in
//!    the catalog output.
//! 3. Optionally embeds fenced code blocks in the type's doc comment
//!    with a `code=<CODE>` tag in the info-string, e.g.
//!    ` ```ebp,code=EBP001 ` or ` ```ebp,code=EBP001,fix `. Any fence
//!    whose info-string contains `code=<this-entry's-code>` is
//!    collected as a snippet under that entry.
//!
//! This mirrors the `docs.rs` bridge model: cargo-aidoc is the reader,
//! consumers only ever emit information that already lives in their
//! rustdoc JSON. A future Layer 2 (`aidoc-attrs`) can add opt-in
//! `#[aidoc::...]` attributes gated behind `#[cfg_attr(aidoc, ...)]`
//! for cases miette doesn't cover, without changing this contract.
//!
//! ## Extraction pipeline
//!
//! [`extract`] walks every crate in the workspace, and for each rustdoc
//! item whose `attrs` contain a `#[diagnostic(...)]` line, produces one
//! [`ErrorEntry`]. Entries are sorted by their `code` for deterministic
//! output.
//!
//! The attribute parser (`parse_diagnostic_attr`) accepts the exact
//! literal form rustdoc emits — including the `\n` that rustdoc inserts
//! after commas inside the attribute — but is intentionally a
//! hand-rolled string reader, not a proc-macro / syn parser. The
//! surface area we care about is small (`code(NAME)` / `help("...")` /
//! `url("...")`), and pulling in `syn` at this layer would drag in a
//! large dependency tree for very little payoff.
//!
//! ## What is covered
//!
//! - **Struct-level** `#[diagnostic(code(...), ...)]` — one entry per
//!   struct.
//! - **Enum-level** `#[diagnostic(code(...), ...)]` — one entry per
//!   enum (rare; miette allows at most one code per enum).
//! - **Per-variant** `#[diagnostic(code(...), ...)]` on enum variants
//!   — one entry per catalogued variant. This is the dominant
//!   `thiserror + miette` shape and is what most real crates use;
//!   see `try_build_entry` and the walk driven by `walk_enum_variants`.
//!
//! ## Not covered by MVP
//!
//! - Variant-level `source_code` / `label` / `related` metadata from
//!   miette is intentionally not surfaced yet — those are
//!   runtime-facing, not spec-facing.
//! - Non-miette diagnostic frameworks (bevy_error, custom derives)
//!   would need their own attr shape; the extractor is written so
//!   another parser can be added alongside `parse_diagnostic_attr`
//!   without touching the walk.

use rustdoc_types::{Item, ItemEnum, Visibility};
use serde::Serialize;

use crate::index::{IndexedCrate, IndexedWorkspace};

/// One catalogued diagnostic: everything cargo-aidoc knows about a
/// single error code, sourced from a single item in the workspace.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    /// The stable error code (e.g. `"EBP001"`), taken verbatim from the
    /// `code(...)` argument of `#[diagnostic(...)]`.
    pub code: String,
    /// The crate the entry was defined in.
    pub crate_name: String,
    /// Fully-qualified item path within the crate
    /// (e.g. `aidoc_spike::MissingInitCtxSeed`).
    pub item_path: String,
    /// The `#[error("...")]` message template, if the type also derives
    /// `thiserror::Error`. Placeholder tokens (`{field}`) are preserved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_template: Option<String>,
    /// The `help("...")` string from `#[diagnostic]`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// The `url("...")` string from `#[diagnostic]`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Full doc body of the item (crate-side `///` / `#[doc = ...]`).
    /// Preserved verbatim so the catalog reader gets the original prose
    /// — including any fenced blocks not tagged with this code.
    pub docs: String,
    /// Fenced code blocks whose info-string carries `code=<this.code>`.
    /// Snippets are stored in the order they appear in `docs`.
    pub snippets: Vec<Snippet>,
}

/// A fenced code block extracted from an error's doc comment.
#[derive(Debug, Clone, Serialize)]
pub struct Snippet {
    /// The info-string's language token (the first comma-separated
    /// segment): `ebp`, `rust`, `lua`, etc.
    pub lang: String,
    /// Bare (non-`key=value`) tags in the info-string, e.g. `"fix"` in
    /// ` ```ebp,code=EBP001,fix `. Preserved in source order.
    pub tags: Vec<String>,
    /// The fence body, joined with `\n` and without the surrounding
    /// ` ``` ` lines.
    pub body: String,
}

/// Walk the workspace and produce every catalogued diagnostic.
///
/// Entries are sorted by [`code`](ErrorEntry::code) for deterministic
/// `--check` behaviour. Items lacking a parseable `code(...)` are
/// silently skipped; a future lint stage can surface those as warnings.
pub fn extract(workspace: &IndexedWorkspace) -> Vec<ErrorEntry> {
    let mut out = Vec::new();
    for krate in &workspace.crates {
        walk_crate(krate, &mut out);
    }
    out.sort_by(|a, b| a.code.cmp(&b.code));
    out
}

fn walk_crate(krate: &IndexedCrate, out: &mut Vec<ErrorEntry>) {
    let index = &krate.crate_data.index;
    let Some(root_item) = index.get(&krate.crate_data.root) else {
        return;
    };
    let ItemEnum::Module(root_module) = &root_item.inner else {
        return;
    };

    // Crate-root path segment matches the rustdoc convention
    // (underscore-normalised crate name), so a downstream link renderer
    // can turn `aidoc_spike::MissingInitCtxSeed` into a valid rustdoc
    // URL without further munging.
    let root_slug = krate.name.replace('-', "_");
    walk_module(krate, index, root_module, root_slug, out);
}

fn walk_module(
    krate: &IndexedCrate,
    index: &std::collections::HashMap<rustdoc_types::Id, Item>,
    module: &rustdoc_types::Module,
    prefix: String,
    out: &mut Vec<ErrorEntry>,
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

        // Struct-level or enum-level `#[diagnostic(code(...))]`.
        if let Some(entry) = try_build_entry(krate, child, &path) {
            out.push(entry);
        }

        match &child.inner {
            ItemEnum::Module(child_module) => {
                walk_module(krate, index, child_module, path, out);
            }
            ItemEnum::Enum(child_enum) => {
                walk_enum_variants(krate, index, child_enum, path, out);
            }
            _ => {}
        }
    }
}

/// Walk every variant of an enum and try to catalogue each one whose
/// `attrs` carry `#[diagnostic(code(...))]`.
///
/// This is the dominant `thiserror + miette` shape:
///
/// ```rust,ignore
/// #[derive(Debug, Error, Diagnostic)]
/// pub enum EvalError {
///     #[error("...")]
///     #[diagnostic(code(E001), url("..."))]
///     PathNotFound { .. },
///     #[error("...")]
///     #[diagnostic(code(E002))]
///     TypeMismatch { .. },
/// }
/// ```
///
/// Each variant becomes its own [`ErrorEntry`] with `item_path` of the
/// form `<crate>::<enum_path>::<VariantName>`. Variants without a
/// `#[diagnostic(code(...))]` are silently skipped.
fn walk_enum_variants(
    krate: &IndexedCrate,
    index: &std::collections::HashMap<rustdoc_types::Id, Item>,
    enum_: &rustdoc_types::Enum,
    enum_path: String,
    out: &mut Vec<ErrorEntry>,
) {
    for variant_id in &enum_.variants {
        let Some(variant) = index.get(variant_id) else {
            continue;
        };
        let Some(name) = variant.name.as_deref() else {
            continue;
        };
        let path = format!("{enum_path}::{name}");
        if let Some(entry) = try_build_entry(krate, variant, &path) {
            out.push(entry);
        }
    }
}

fn try_build_entry(krate: &IndexedCrate, item: &Item, item_path: &str) -> Option<ErrorEntry> {
    let diag_attr = item.attrs.iter().find_map(find_diagnostic_line)?;
    let parsed = parse_diagnostic_attr(&diag_attr)?;
    let code = parsed.code?;

    let message_template = item
        .attrs
        .iter()
        .find_map(find_error_line)
        .and_then(|s| parse_error_attr(&s));

    let docs = item.docs.clone().unwrap_or_default();
    let snippets = extract_snippets(&docs, &code);

    Some(ErrorEntry {
        code,
        crate_name: krate.name.clone(),
        item_path: item_path.to_owned(),
        message_template,
        help: parsed.help,
        url: parsed.url,
        docs,
        snippets,
    })
}

/// rustdoc emits `Item::attrs` as a `Vec<AttributeInfo>` in newer
/// format versions; older versions used `Vec<String>`. In both cases
/// the payload our extractor cares about is available as a string.
///
/// We accept both a raw `String` and a JSON-serialised object shape
/// (`{"other": "#[diagnostic(...)]"}`) so we don't have to pin to one
/// specific rustdoc-types major.
fn find_diagnostic_line(attr: &rustdoc_types::Attribute) -> Option<String> {
    let raw = attr_as_string(attr)?;
    let trimmed = raw.trim();
    if trimmed.starts_with("#[diagnostic(") {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn find_error_line(attr: &rustdoc_types::Attribute) -> Option<String> {
    let raw = attr_as_string(attr)?;
    let trimmed = raw.trim();
    if trimmed.starts_with("#[error(") {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

/// Best-effort stringification of a rustdoc `Attribute`. The type is
/// non-exhaustive across rustdoc-types versions; we serialise to JSON
/// and pull the payload out of the shape we know.
fn attr_as_string(attr: &rustdoc_types::Attribute) -> Option<String> {
    // Fast path: some rustdoc-types versions expose an `Other(String)`
    // variant. Debug-format is stable enough for our string starts-with
    // checks, but we go through JSON to stay version-agnostic.
    let value = serde_json::to_value(attr).ok()?;
    if let Some(s) = value.as_str() {
        return Some(s.to_owned());
    }
    if let Some(obj) = value.as_object()
        && let Some(other) = obj.get("other").and_then(|v| v.as_str())
    {
        return Some(other.to_owned());
    }
    None
}

#[derive(Debug, Default)]
struct ParsedDiagnostic {
    code: Option<String>,
    help: Option<String>,
    url: Option<String>,
}

/// Parse the interior of `#[diagnostic(...)]`.
///
/// Accepts the exact shape rustdoc emits, including the `\n` that
/// rustdoc inserts after commas between arguments. We recognise the
/// three arguments cargo-aidoc uses today (`code`, `help`, `url`) and
/// silently skip any others so the parser stays forward-compatible
/// with new miette attributes.
fn parse_diagnostic_attr(raw: &str) -> Option<ParsedDiagnostic> {
    let inner = raw.strip_prefix("#[diagnostic(")?.strip_suffix(")]")?;
    let mut out = ParsedDiagnostic::default();

    for arg in split_top_level_args(inner) {
        let arg = arg.trim();
        if let Some(code) = arg
            .strip_prefix("code(")
            .and_then(|s| s.strip_suffix(')'))
        {
            out.code = Some(code.trim().to_owned());
        } else if let Some(help) = arg
            .strip_prefix("help(")
            .and_then(|s| s.strip_suffix(')'))
            .and_then(strip_quotes)
        {
            out.help = Some(help);
        } else if let Some(url) = arg.strip_prefix("url(").and_then(|s| s.strip_suffix(')')).and_then(strip_quotes) {
            out.url = Some(url);
        }
    }

    Some(out)
}

fn parse_error_attr(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix("#[error(")?.strip_suffix(")]")?;
    strip_quotes(inner.trim())
}

/// Strip a surrounding pair of `"..."` and unescape the two escapes
/// rustdoc's attr serialiser emits (`\\` and `\"`).
fn strip_quotes(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

/// Split the interior of a `foo(a, b(x, y), c)` argument list on
/// top-level commas, respecting parentheses and `"..."` string literals.
fn split_top_level_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;

    for ch in s.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if in_string {
            if ch == '\\' {
                current.push(ch);
                escape = true;
            } else {
                current.push(ch);
                if ch == '"' {
                    in_string = false;
                }
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Walk `docs` and collect every fenced code block whose info-string
/// contains `code=<target_code>` as a comma-separated tag.
///
/// Recognised info-string shape:
///
/// ```text
/// <lang>[,tag_or_kv]*
/// ```
///
/// Where each `tag_or_kv` is either a bare identifier (`fix`) or a
/// `key=value` pair (`code=EBP001`). Only `key=value` pairs go into
/// `key=value` matching; bare tokens land in [`Snippet::tags`].
fn extract_snippets(docs: &str, target_code: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut lines = docs.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        if info.is_empty() {
            // Bare fence with no info-string — not tagged, skip.
            skip_to_fence_close(&mut lines);
            continue;
        }

        let parts: Vec<&str> = info.split(',').map(str::trim).collect();
        let lang = parts[0].to_owned();

        let mut tags = Vec::new();
        let mut has_target_code = false;

        for part in parts.iter().skip(1) {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim() == "code" && v.trim() == target_code {
                    has_target_code = true;
                }
                // Other kv pairs currently ignored; a later phase can
                // surface them as structured tags without breaking the
                // wire format.
            } else if !part.is_empty() {
                tags.push((*part).to_owned());
            }
        }

        let body = collect_fence_body(&mut lines);

        if has_target_code {
            out.push(Snippet { lang, tags, body });
        }
    }

    out
}

fn collect_fence_body<'a>(lines: &mut std::iter::Peekable<std::str::Lines<'a>>) -> String {
    let mut body = String::new();
    let mut first = true;
    for line in lines.by_ref() {
        if line.trim_start().starts_with("```") {
            break;
        }
        if !first {
            body.push('\n');
        }
        body.push_str(line);
        first = false;
    }
    body
}

fn skip_to_fence_close<'a>(lines: &mut std::iter::Peekable<std::str::Lines<'a>>) {
    for line in lines.by_ref() {
        if line.trim_start().starts_with("```") {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_quotes_handles_escaped_quotes() {
        assert_eq!(
            strip_quotes(r#""hello \"world\"""#).as_deref(),
            Some("hello \"world\"")
        );
    }

    #[test]
    fn split_top_level_respects_parens_and_strings() {
        let got = split_top_level_args(r#"code(EBP001), help("has, comma"), url("x")"#);
        assert_eq!(
            got.iter().map(|s| s.trim()).collect::<Vec<_>>(),
            vec!["code(EBP001)", r#"help("has, comma")"#, r#"url("x")"#]
        );
    }

    #[test]
    fn parse_diagnostic_attr_extracts_all_three_fields() {
        let raw = "#[diagnostic(code(EBP001),\nhelp(\"seed context.d\"),\nurl(\"mse://guides/errors/EBP001\"))]";
        let parsed = parse_diagnostic_attr(raw).expect("must parse");
        assert_eq!(parsed.code.as_deref(), Some("EBP001"));
        assert_eq!(parsed.help.as_deref(), Some("seed context.d"));
        assert_eq!(parsed.url.as_deref(), Some("mse://guides/errors/EBP001"));
    }

    #[test]
    fn parse_diagnostic_attr_missing_fields_ok() {
        let raw = "#[diagnostic(code(EBP002))]";
        let parsed = parse_diagnostic_attr(raw).expect("must parse");
        assert_eq!(parsed.code.as_deref(), Some("EBP002"));
        assert!(parsed.help.is_none());
        assert!(parsed.url.is_none());
    }

    #[test]
    fn parse_error_attr_returns_message_template() {
        let raw = r#"#[error("stage `{stage}` referenced by pipeline has no seed in context.d")]"#;
        assert_eq!(
            parse_error_attr(raw).as_deref(),
            Some("stage `{stage}` referenced by pipeline has no seed in context.d")
        );
    }

    #[test]
    fn extract_snippets_picks_matching_code_tag_and_records_bare_tags() {
        let docs = concat!(
            "Some prose.\n",
            "\n",
            "```ebp,code=EBP001\n",
            "bad snippet\n",
            "line 2\n",
            "```\n",
            "\n",
            "```ebp,code=EBP001,fix\n",
            "good snippet\n",
            "```\n",
            "\n",
            "```rust,code=EBP002\n",
            "different code, must be skipped\n",
            "```\n",
        );
        let snippets = extract_snippets(docs, "EBP001");
        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].lang, "ebp");
        assert_eq!(snippets[0].tags, Vec::<String>::new());
        assert_eq!(snippets[0].body, "bad snippet\nline 2");
        assert_eq!(snippets[1].lang, "ebp");
        assert_eq!(snippets[1].tags, vec!["fix".to_owned()]);
        assert_eq!(snippets[1].body, "good snippet");
    }
}
