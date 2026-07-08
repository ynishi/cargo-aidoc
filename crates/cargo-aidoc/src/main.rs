//! `cargo aidoc` — cargo subcommand entry point.
//!
//! This binary is a thin CLI wrapper around the pipeline implemented in
//! `aidoc-core`. It parses `cargo aidoc <args>` invocations (cargo strips
//! the leading `aidoc` argument when invoking `cargo-aidoc` as a
//! subcommand) and delegates to the core library for indexing,
//! generation, and linting. No pipeline logic should live in this crate.

fn main() {
    eprintln!("cargo-aidoc: not yet implemented");
    std::process::exit(1);
}
