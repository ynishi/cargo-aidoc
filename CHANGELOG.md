# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-07-08

Initial release. Three crates ship together at the same version.

### Added

- **`aidoc-core`** — pipeline library exposing:
  - `Config` / `Preset::Publish` / `Platform` (Context7, DeepWiki,
    AnthropicStyle) / `UnknownPlatform`.
  - `Error` enum with a `Generate` variant that carries an
    index-stage summary (facade error contract ported from
    algocline's `hub_dist`).
  - `IndexedWorkspace::build` — walks the workspace via
    `cargo_metadata`, shells out to
    `cargo +nightly rustdoc -- -Zunstable-options --output-format json`
    for every crate, and parses the result via `rustdoc-types`.
    Format-version drift surfaces as a typed
    `Error::FormatVersionMismatch`.
  - `generate::render_all` — emits the Preset::Publish artifact set
    (`llms.txt`, per-crate / per-module markdown, `llms-full.txt`,
    per-crate deterministic `api/<crate>.json`).
  - `lint::lint` — four initial checks (`missing-crate-doc`,
    `short-crate-doc`, `missing-module-doc`, `llms-full-too-large`).
  - `platform::apply_overlays` — Context7 (`context7.json`),
    DeepWiki (`.devin/wiki.json`), and anthropic-style (reverse
    cross-ref hint) overlays.
  - `run` / `write_report` / `diff_report` facade for front ends.
- **`cargo-aidoc`** — cargo subcommand
  (`cargo aidoc [--workspace-root PATH] [--out-dir PATH] [--check]
  [--strict] [--platform NAME[,NAME...]]`) with the documented exit
  code contract (0 = clean, 1 = pipeline error, 2 = lint / drift).
- **`aidoc-mcp`** — MCP stdio server exposing three tools
  (`aidoc_info`, `aidoc_gen`, `aidoc_check`). Every response is
  wrapped in the same `{ ok, summary, diagnostics[], written[],
  diffs[], error? }` envelope.

### Known limitations

- MCP protocol `resources/*` (`resources/list` / `resources/read`)
  is not yet implemented; the server declares only `tools`
  capability. Tracked as a follow-up.
- The `aidoc-mcp` `RunParams` schema does not yet expose the
  `--platform` overlay list; platform overlays are only reachable
  from the CLI at this release.
- `rustdoc-types` is version-locked to what the maintainers of that
  crate ship. If the nightly toolchain a user runs emits a different
  `format_version`, the pipeline exits with a typed
  `FormatVersionMismatch` error rather than best-effort parsing.
