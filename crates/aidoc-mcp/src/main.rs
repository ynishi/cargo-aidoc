//! `aidoc-mcp` — Model Context Protocol server entry point.
//!
//! Exposes the aidoc-core pipeline as four MCP tools:
//!
//! - `aidoc_info` — report version and default configuration.
//! - `aidoc_gen` — run the pipeline and write artifacts to disk.
//! - `aidoc_check` — run the pipeline and diff against the on-disk
//!   copy without writing anything (read-only, matches `--check`).
//! - `aidoc_error` — fetch one or all catalogued diagnostics without
//!   writing any files.
//!
//! It also exposes two orientation guides as MCP resources:
//!
//! - `aidoc://guides/onboarding` — tool + resource map for callers.
//! - `aidoc://guides/error-catalog` — consumer contract for the
//!   error catalog (what a crate does to appear in it).
//!
//! Every tool response is wrapped in a JSON envelope containing `ok`,
//! a short human-readable `summary`, the full lint diagnostic list,
//! and (depending on the tool) the list of artifacts written, the list
//! of paths that would change, or the requested error entries. The
//! envelope shape mirrors algocline's `hub_dist` gendoc contract so
//! callers can reuse the same handling.

use std::path::PathBuf;

use aidoc_core::{Config, ErrorEntry, Level};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
        ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
        ServerCapabilities, ServerInfo,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

/// Bundled onboarding guide (surfaced via `aidoc://guides/onboarding`).
const GUIDE_ONBOARDING: &str = include_str!("../guides/onboarding.md");

/// Bundled error-catalog consumer-contract guide (surfaced via
/// `aidoc://guides/error-catalog`).
const GUIDE_ERROR_CATALOG: &str = include_str!("../guides/error-catalog.md");

/// Shared parameter shape for both `aidoc_gen` and `aidoc_check`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct RunParams {
    /// Workspace root (directory containing the top-level Cargo.toml).
    /// Defaults to the current working directory.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Output directory. Defaults to `<workspace_root>/docs/aidoc/`.
    #[serde(default)]
    pub out_dir: Option<String>,
    /// Promote lint warnings to errors so the envelope's `ok` flag
    /// reflects them.
    #[serde(default)]
    pub strict: bool,
    /// Additionally emit the error catalog (`errors/<CODE>.md`,
    /// `errors/index.json`, `llms-errors.txt`) built from every
    /// `#[derive(miette::Diagnostic)]` item in the workspace.
    #[serde(default)]
    pub errors: bool,
    /// Move the committed artifacts to this host's target.
    ///
    /// `aidoc_gen` otherwise refuses to overwrite artifacts generated
    /// for another target, because `cfg`-gated items differ between
    /// targets and the overwrite deletes what the other one documents.
    /// Ignored by `aidoc_check`, which never writes.
    #[serde(default)]
    pub retarget: bool,
}

/// Parameter shape for the `aidoc_error` tool.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ErrorParams {
    /// Workspace root (directory containing the top-level Cargo.toml).
    /// Defaults to the current working directory.
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// Fetch a single entry by its `code` (e.g. `"EBP001"`). Omit to
    /// receive a compact summary of every catalogued diagnostic.
    #[serde(default)]
    pub code: Option<String>,
}

/// The MCP server exposing aidoc's three tools.
#[derive(Clone)]
pub struct AidocServer {
    #[allow(dead_code, reason = "used by #[tool_router] / #[tool_handler] macros")]
    tool_router: ToolRouter<AidocServer>,
}

impl Default for AidocServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl AidocServer {
    /// Build a fresh server instance. Called once at startup; MCP has
    /// no per-connection state.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Report version and default configuration. Read-only.
    #[tool(description = "Report aidoc version and default configuration.")]
    async fn aidoc_info(&self) -> Result<CallToolResult, McpError> {
        let info = serde_json::json!({
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "default_out_dir": "docs/aidoc",
            "tools": ["aidoc_info", "aidoc_gen", "aidoc_check", "aidoc_error"],
            "resources": [
                "aidoc://guides/onboarding",
                "aidoc://guides/error-catalog",
            ],
        });
        Ok(text_result(info))
    }

    /// Run the pipeline and write artifacts to disk.
    #[tool(description = "Generate LLM-facing doc artifacts under out_dir.")]
    async fn aidoc_gen(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<CallToolResult, McpError> {
        let envelope = tokio::task::spawn_blocking(move || run_pipeline(params, false))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(text_result(
            serde_json::to_value(&envelope).unwrap_or_default(),
        ))
    }

    /// Run the pipeline and diff against the on-disk copy without
    /// writing anything. Read-only.
    #[tool(description = "Check for drift between generated artifacts and on-disk copy.")]
    async fn aidoc_check(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<CallToolResult, McpError> {
        let envelope = tokio::task::spawn_blocking(move || run_pipeline(params, true))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(text_result(
            serde_json::to_value(&envelope).unwrap_or_default(),
        ))
    }

    /// Fetch one or all catalogued diagnostics.
    ///
    /// This is a read-only lookup: nothing is written to disk. The
    /// extractor runs in-memory against fresh rustdoc JSON, so the
    /// caller sees whatever the source tree currently defines —
    /// useful when the on-disk `errors/index.json` is stale or absent.
    ///
    /// Response envelope:
    ///
    /// - `code` provided: `{ ok, summary, entries: [ErrorEntry] }`
    ///   with a single element if the code matched, else empty.
    /// - `code` omitted: `{ ok, summary, entries: [ErrorSummary] }`
    ///   where each entry is `{ code, item_path, message_template }`.
    #[tool(description = "Fetch one or all catalogued diagnostics by stable code.")]
    async fn aidoc_error(
        &self,
        Parameters(params): Parameters<ErrorParams>,
    ) -> Result<CallToolResult, McpError> {
        let envelope = tokio::task::spawn_blocking(move || fetch_errors(params))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(text_result(
            serde_json::to_value(&envelope).unwrap_or_default(),
        ))
    }
}

#[tool_handler]
impl ServerHandler for AidocServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_protocol_version(ProtocolVersion::LATEST)
        .with_instructions(
            "Generate LLM-facing doc artifacts (llms.txt / markdown / api.json / error \
             catalog) from rustdoc JSON. Tools: `aidoc_gen` (write), `aidoc_check` (drift), \
             `aidoc_error` (fetch one or all catalogued diagnostics), `aidoc_info` (metadata). \
             Read `aidoc://guides/onboarding` first for a tool map; read \
             `aidoc://guides/error-catalog` for the consumer contract."
                .to_owned(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: vec![
                text_resource(
                    "aidoc://guides/onboarding",
                    "onboarding",
                    "Tool and resource map for the aidoc MCP server.",
                ),
                text_resource(
                    "aidoc://guides/error-catalog",
                    "error-catalog",
                    "Consumer contract: how a crate joins the error catalog.",
                ),
            ],
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let body = match request.uri.as_str() {
            "aidoc://guides/onboarding" => GUIDE_ONBOARDING,
            "aidoc://guides/error-catalog" => GUIDE_ERROR_CATALOG,
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown resource uri: {other}"),
                    None,
                ));
            }
        };
        let contents = ResourceContents::text(body, request.uri);
        Ok(ReadResourceResult::new(vec![contents]))
    }
}

fn text_resource(uri: &str, name: &str, description: &str) -> Resource {
    Resource::new(uri.to_owned(), name.to_owned())
        .with_description(description.to_owned())
        .with_mime_type("text/markdown")
}

/// The JSON envelope returned by `aidoc_gen` and `aidoc_check`.
///
/// `ok = false` means the pipeline itself failed (rustdoc, I/O, config)
/// or, in check mode, the on-disk tree differs from what would be
/// generated. Lint diagnostics ride the envelope but do not flip `ok`
/// unless `strict = true` was set.
#[derive(Debug, Default, Serialize)]
struct Envelope {
    ok: bool,
    summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<DiagnosticView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    written: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diffs: Vec<String>,
    /// Set when the committed artifacts describe a different target
    /// than this host documented.
    ///
    /// Present on both tools and on both meanings — `aidoc_check` could
    /// not answer, `aidoc_gen` refused to write — because a caller that
    /// only reads `ok` would otherwise see a plain failure and retry
    /// the thing that cannot work here.
    #[serde(skip_serializing_if = "Option::is_none")]
    target_mismatch: Option<TargetMismatchView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The two triples behind a [`Envelope::target_mismatch`].
#[derive(Debug, Serialize)]
struct TargetMismatchView {
    /// Triple the committed artifacts describe.
    recorded: String,
    /// Triple this run documented.
    generated: String,
}

#[derive(Debug, Serialize)]
struct DiagnosticView {
    level: &'static str,
    code: &'static str,
    location: String,
    message: String,
}

fn run_pipeline(params: RunParams, check: bool) -> Envelope {
    let workspace_root = params
        .workspace_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let out_dir = params
        .out_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("docs/aidoc"));

    let config = Config {
        strict: params.strict,
        check,
        out_dir: out_dir.clone(),
        emit_error_catalog: params.errors,
        ..Config::default()
    };

    let report = match aidoc_core::run(&workspace_root, &config) {
        Ok(r) => r,
        Err(err) => {
            return Envelope {
                ok: false,
                summary: format!("aidoc: pipeline failed: {err}"),
                error: Some(err.to_string()),
                ..Envelope::default()
            };
        }
    };

    let diagnostics: Vec<DiagnosticView> = report
        .diagnostics
        .iter()
        .map(|d| DiagnosticView {
            level: match d.level {
                Level::Warn => "warn",
                Level::Error => "error",
            },
            code: d.code,
            location: d.location.clone(),
            message: d.message.clone(),
        })
        .collect();

    // The same fence `cargo aidoc` applies, for the same reason: on a
    // target mismatch a diff answers a different question than the one
    // asked, and a write deletes the recorded target's items. Kept in
    // both front ends rather than inside `aidoc_core::run` because only
    // the front end knows whether the caller asked to write.
    match aidoc_core::target_verdict(&report, &out_dir) {
        Ok(aidoc_core::TargetVerdict::Mismatch {
            recorded,
            generated,
        }) if check || !params.retarget => {
            let summary = if check {
                format!(
                    "aidoc: NOT CHECKED — the committed artifacts describe {recorded}, this \
                     host documented {generated}. Re-run on {recorded}."
                )
            } else {
                format!(
                    "aidoc: refusing to write — the committed artifacts describe {recorded}, \
                     this host documented {generated}. Regenerate on {recorded}, or pass \
                     retarget to move them here."
                )
            };
            return Envelope {
                ok: false,
                summary,
                diagnostics,
                target_mismatch: Some(TargetMismatchView {
                    recorded,
                    generated,
                }),
                ..Envelope::default()
            };
        }
        Ok(_) => {}
        Err(err) => {
            return Envelope {
                ok: false,
                summary: format!("aidoc: target check failed: {err}"),
                diagnostics,
                error: Some(err.to_string()),
                ..Envelope::default()
            };
        }
    }

    if check {
        // Shared with `cargo aidoc --check` via aidoc_core so both
        // front ends produce the same actionable summary — critically,
        // the same distinction between "not on disk yet" and
        // "modified" for partial-uninit cases.
        let summary_data = match aidoc_core::classify_diffs(&report, &out_dir, &workspace_root) {
            Ok(s) => s,
            Err(err) => {
                return Envelope {
                    ok: false,
                    summary: format!("aidoc: diff failed: {err}"),
                    diagnostics,
                    error: Some(err.to_string()),
                    ..Envelope::default()
                };
            }
        };
        let ok = summary_data.is_empty() && !report.has_errors();
        let summary = summary_data.summary_message(report.artifacts.len());
        let diffs: Vec<String> = summary_data.paths().cloned().collect();
        Envelope {
            ok,
            summary,
            diagnostics,
            diffs,
            ..Envelope::default()
        }
    } else {
        if let Err(err) = aidoc_core::write_report(&report, &out_dir, &workspace_root) {
            return Envelope {
                ok: false,
                summary: format!("aidoc: write failed: {err}"),
                diagnostics,
                error: Some(err.to_string()),
                ..Envelope::default()
            };
        }
        let written = report
            .artifacts
            .iter()
            .map(|a| a.path.clone())
            .collect::<Vec<_>>();
        let ok = !report.has_errors();
        let summary = format!(
            "aidoc: wrote {} artifact(s) to {}",
            written.len(),
            out_dir.display()
        );
        Envelope {
            ok,
            summary,
            diagnostics,
            written,
            ..Envelope::default()
        }
    }
}

/// Wrap a serde_json::Value in the MCP text-content shape.
fn text_result(value: serde_json::Value) -> CallToolResult {
    let body = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    CallToolResult::success(vec![ContentBlock::text(body)])
}

/// Envelope returned by `aidoc_error`.
///
/// Same shape as the `Envelope` returned by `aidoc_gen` / `aidoc_check`
/// in the `ok` / `summary` fields, but carries either full entries or
/// compact summaries in `entries`.
#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    ok: bool,
    summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entries: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorSummary {
    code: String,
    item_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_template: Option<String>,
}

fn fetch_errors(params: ErrorParams) -> ErrorEnvelope {
    let workspace_root = params
        .workspace_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let workspace = match aidoc_core::IndexedWorkspace::build(&workspace_root, &Config::default()) {
        Ok(w) => w,
        Err(err) => {
            return ErrorEnvelope {
                ok: false,
                summary: format!("aidoc_error: index failed: {err}"),
                entries: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let all: Vec<ErrorEntry> = aidoc_core::error_catalog::extract(&workspace);

    match params.code.as_deref() {
        Some(code) => match all.into_iter().find(|e| e.code == code) {
            Some(entry) => {
                let value = serde_json::to_value(&entry).unwrap_or_default();
                ErrorEnvelope {
                    ok: true,
                    summary: format!("aidoc_error: found `{code}`"),
                    entries: vec![value],
                    error: None,
                }
            }
            None => ErrorEnvelope {
                ok: false,
                summary: format!("aidoc_error: no entry with code `{code}`"),
                entries: Vec::new(),
                error: Some(format!("code not found: {code}")),
            },
        },
        None => {
            let n = all.len();
            let entries = all
                .into_iter()
                .map(|e| {
                    let summary = ErrorSummary {
                        code: e.code,
                        item_path: e.item_path,
                        message_template: e.message_template,
                    };
                    serde_json::to_value(&summary).unwrap_or_default()
                })
                .collect();
            ErrorEnvelope {
                ok: true,
                summary: format!("aidoc_error: {n} entry(ies)"),
                entries,
                error: None,
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = AidocServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
