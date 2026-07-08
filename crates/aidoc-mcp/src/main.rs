//! `aidoc-mcp` — Model Context Protocol server entry point.
//!
//! This binary is a thin MCP server wrapper over the facade exposed by
//! `aidoc-core`. It is intended to expose the same index / generate / lint
//! pipeline as `cargo-aidoc`, but reachable over MCP tool calls instead of
//! the command line. No pipeline logic should live in this crate.

fn main() {
    eprintln!("aidoc-mcp: not yet implemented");
    std::process::exit(1);
}
