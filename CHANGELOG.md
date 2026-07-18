# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Error catalog** — opt-in via `Config::emit_error_catalog` (`--errors`
  on the CLI, `RunParams.errors` on `aidoc_gen` / `aidoc_check`). Emits
  one narrative `errors/<CODE>.md` per catalogued diagnostic, a
  deterministic `errors/index.json`, and a top-level `llms-errors.txt`
  following the [llmstxt.org](https://llmstxt.org) shape. The
  consumer contract is zero-coupling: `#[derive(miette::Diagnostic)]`
  plus doc fences whose info-string carries `code=<CODE>` is enough —
  no dependency on cargo-aidoc from the consumer crate.
- **Three miette shapes catalogued** — struct-level, enum-level (one
  code per enum), and per-variant (the dominant `thiserror + miette`
  shape). Variant entries record the fully-qualified
  `<crate>::<enum>::<Variant>` path.
- **`aidoc_error` MCP tool** — fetch one or all catalogued diagnostics
  in-memory (no writes). Passing a `code` returns the full
  `ErrorEntry`; omitting `code` returns a compact
  `{code, item_path, message_template}` summary of every entry.
- **MCP resources** — `aidoc://guides/onboarding` (tool + resource map)
  and `aidoc://guides/error-catalog` (consumer contract). Server
  capabilities now advertise `resources` alongside `tools`, and the
  server instructions point callers at the guides.
- **`aidoc_info`** now reports both `tools` and `resources` for
  one-shot server-surface introspection.
- **onboarding guide** documents the decision boundary between
  committing `docs/aidoc/` and adding it to `.gitignore`.

### Changed

- **`aidoc_check` / `cargo aidoc --check` share summary logic** via
  the new `aidoc_core::classify_diffs` and
  `DiffSummary::summary_message`. The CLI and MCP tool now emit the
  same actionable message and distinguish "missing on disk" (`run
  aidoc_gen to write them`) from "modified on disk" (`run aidoc_gen
  to update`) — critical for the partial-uninit case where
  `docs/aidoc/` exists from an earlier run but the error catalog
  subset has never been written.
- **`aidoc_core::diff_report`** is now a wrapper over `classify_diffs`
  for backwards compatibility; new callers should prefer
  `classify_diffs` for categorised output.

### Fixed

- **Module-slug collision** — a top-level module literally named
  `index` no longer collides with the crate-root `<crate>/index.md`
  document. The module resolves to `<crate>/_index.md` so both
  artefacts co-exist.

### Known limitations

- The `aidoc-mcp` `RunParams` schema does not yet expose the
  `--platform` overlay list; platform overlays remain CLI-only.
  Tracked as a follow-up.
- `rustdoc-types` is version-locked to what the maintainers of that
  crate ship. If the nightly toolchain a user runs emits a different
  `format_version`, the pipeline exits with a typed
  `FormatVersionMismatch` error rather than best-effort parsing.

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
