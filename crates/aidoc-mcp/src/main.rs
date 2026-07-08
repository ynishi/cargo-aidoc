//! `aidoc-mcp` — Model Context Protocol server entry point.
//!
//! Exposes the aidoc-core pipeline as three MCP tools:
//!
//! - `aidoc_info` — report version and default configuration.
//! - `aidoc_gen` — run the pipeline and write artifacts to disk.
//! - `aidoc_check` — run the pipeline and diff against the on-disk
//!   copy without writing anything (read-only, matches `--check`).
//!
//! Every response is wrapped in a JSON envelope containing `ok`, a
//! short human-readable `summary`, the full lint diagnostic list, and
//! (depending on the tool) the list of artifacts written or the list of
//! paths that would change. The envelope shape mirrors algocline's
//! `hub_dist` gendoc contract so callers can reuse the same handling.

use std::path::PathBuf;

use aidoc_core::{Config, Level};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

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
            "tools": ["aidoc_info", "aidoc_gen", "aidoc_check"],
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
        Ok(text_result(serde_json::to_value(&envelope).unwrap_or_default()))
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
        Ok(text_result(serde_json::to_value(&envelope).unwrap_or_default()))
    }
}

#[tool_handler]
impl ServerHandler for AidocServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(
                "Generate LLM-facing doc artifacts (llms.txt / markdown / api.json) from \
                 rustdoc JSON. Use `aidoc_gen` to write, `aidoc_check` for drift detection, \
                 and `aidoc_info` for metadata."
                    .to_owned(),
            )
    }
}

/// The JSON envelope returned by `aidoc_gen` and `aidoc_check`.
///
/// `ok = false` means the pipeline itself failed (rustdoc, I/O, config)
/// or, in check mode, the on-disk tree differs from what would be
/// generated. Lint diagnostics ride the envelope but do not flip `ok`
/// unless `strict = true` was set.
#[derive(Debug, Serialize)]
struct Envelope {
    ok: bool,
    summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<DiagnosticView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    written: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diffs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
        ..Config::default()
    };

    let report = match aidoc_core::run(&workspace_root, &config) {
        Ok(r) => r,
        Err(err) => {
            return Envelope {
                ok: false,
                summary: format!("aidoc: pipeline failed: {err}"),
                diagnostics: Vec::new(),
                written: Vec::new(),
                diffs: Vec::new(),
                error: Some(err.to_string()),
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

    if check {
        let diffs = match aidoc_core::diff_report(&report, &out_dir) {
            Ok(d) => d,
            Err(err) => {
                return Envelope {
                    ok: false,
                    summary: format!("aidoc: diff failed: {err}"),
                    diagnostics,
                    written: Vec::new(),
                    diffs: Vec::new(),
                    error: Some(err.to_string()),
                };
            }
        };
        let ok = diffs.is_empty() && !report.has_errors();
        let summary = if diffs.is_empty() {
            format!("aidoc: check clean ({} artifact(s))", report.artifacts.len())
        } else {
            format!("aidoc: {} artifact(s) would change", diffs.len())
        };
        Envelope {
            ok,
            summary,
            diagnostics,
            written: Vec::new(),
            diffs,
            error: None,
        }
    } else {
        if let Err(err) = aidoc_core::write_report(&report, &out_dir) {
            return Envelope {
                ok: false,
                summary: format!("aidoc: write failed: {err}"),
                diagnostics,
                written: Vec::new(),
                diffs: Vec::new(),
                error: Some(err.to_string()),
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
            diffs: Vec::new(),
            error: None,
        }
    }
}

/// Wrap a serde_json::Value in the MCP text-content shape.
fn text_result(value: serde_json::Value) -> CallToolResult {
    let body = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    CallToolResult::success(vec![ContentBlock::text(body)])
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = AidocServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
