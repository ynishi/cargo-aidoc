# cargo-aidoc

Generate LLM-facing doc artifacts (`llms.txt` / narrative markdown /
deterministic JSON) from rustdoc JSON.

`cargo-aidoc` walks a Rust workspace with `cargo_metadata`, runs
`cargo +nightly rustdoc --output-format json` for each crate, and
projects the resulting API surface into artifacts that LLM-facing doc
services and tools can consume:

- **`llms.txt`** — top-level index following
  [llmstxt.org](https://llmstxt.org) (H1 title, blockquote summary,
  per-crate H2 bullet list linking to markdown / API JSON).
- **`<crate>/index.md`** — narrative markdown built from the crate
  root's `//!` doc comment plus a list of public modules.
- **`<crate>/<module>.md`** — per-module narrative + a public-item
  reference grouped by kind (functions / types / traits / constants /
  macros), alphabetized within each group.
- **`llms-full.txt`** — every markdown artifact concatenated with
  chunk headers, for context-hungry LLM callers.
- **`api/<crate>.json`** — key-sorted deterministic public-API surface
  (`{ crate, version, items: [{ path, kind, docs? }] }`) usable as a
  `--check --strict` drift target.

## Workspace layout

- **`aidoc-core`** — library. Holds all of the pipeline logic (index /
  generate / lint) shared by the two front ends.
- **`cargo-aidoc`** — cargo subcommand binary
  (`cargo aidoc [--check] [--strict] [--platform ...]`).
- **`aidoc-mcp`** — MCP server binary exposing three tools
  (`aidoc_info`, `aidoc_gen`, `aidoc_check`) over stdio for hosts like
  Claude Code.

## Quick start

```bash
# from a Rust workspace root
cargo install cargo-aidoc
cargo aidoc                           # write to <workspace>/docs/aidoc/
cargo aidoc --check --strict          # CI drift check, exit 2 on any change
cargo aidoc --platform context7       # also emit context7.json at the repo root
```

Exit code contract:

- `0` — clean run (or `--check` found no drift).
- `1` — pipeline error (rustdoc failed, I/O, config invalid).
- `2` — lint violation, or `--check` detected drift.

## External platform overlays (`--platform`)

Layer on top of the core output for specific LLM-doc services. Values
compose (`--platform context7,deepwiki,anthropic-style`) and can be
repeated:

- **`context7`** — emit `context7.json` at the repo root
  (`$schema` / `projectTitle` / `description` / `folders`).
- **`deepwiki`** — emit `.devin/wiki.json` at the repo root
  (`repo_notes` + one `pages[]` entry per crate, capped at 30 pages).
- **`anthropic-style`** — prepend a reverse cross-ref blockquote to
  every generated `.md` and to `llms-full.txt`, mirroring the pattern
  Anthropic uses on `platform.claude.com`.

Adding a new platform means adding a variant to `Platform` and an arm
to `platform::apply_overlays`; see `crates/aidoc-core/src/platform.rs`.

## Prerequisites

- A `nightly` Rust toolchain must be available to `rustup` — the
  pipeline shells out to `cargo +nightly rustdoc -- -Zunstable-options
  --output-format json`.
- The `rustdoc-types` crate is version-locked; if the nightly you use
  emits a different `format_version` you'll see a typed
  `FormatVersionMismatch` error rather than silent garbage.

## MCP server

`aidoc-mcp` is a stdio MCP server that exposes the same pipeline over
three tools. Every tool response is wrapped in a JSON envelope
(`{ ok, summary, diagnostics[], written[], diffs[], error? }`) that
mirrors algocline's `hub_dist` gendoc contract.

Register in a project-local `.mcp.json`:

```json
{
  "mcpServers": {
    "aidoc": { "command": "aidoc-mcp", "args": [] }
  }
}
```

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
