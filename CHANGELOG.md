# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [0.3.0] — 2026-08-15

### Added

- **`aidoc-manifest.json`**, written beside the artifacts, recording the
  target triple they describe. rustdoc resolves `cfg` before it emits
  anything, so a module behind `#[cfg(target_os = "macos")]` is in the
  payload on a Mac and absent everywhere else: the artifact set is a
  property of the source *and* the host that documented it. Without the
  record, neither front end could tell "somebody added a module and did
  not regenerate" from "this host resolves `cfg` differently", and only
  the first is drift. The file carries the triple and a schema version
  and nothing else — a generator version or a timestamp would rewrite a
  committed file on every run and turn the drift check into noise.
- **`--retarget`** (`retarget` on the MCP tools) moves the committed
  artifacts to the running host's target, for a project moving its
  canonical target on purpose.
- **`manifest` module** in `aidoc-core`: `Manifest`, `TargetVerdict`,
  `manifest::verdict`, and `aidoc_core::target_verdict` — the comparison
  both front ends run, in one place, so they cannot answer differently.
- **`Report::target`** carries the triple the run documented, so a front
  end can ask without re-running the index stage.
- **`IndexedWorkspace::target_triple`** reads it off the payload's own
  `Crate::target::triple` rather than off this process's `cfg`, so a
  future `--target` would record the right answer without touching this.

### Changed

- **A generate run against artifacts recorded for another target now
  refuses to write** (exit 1) instead of overwriting them. This is the
  failure the record exists for: the overwrite drops every item only the
  other target documents, and a diff of that deletion is shaped exactly
  like an ordinary regeneration, so it ships. Observed in
  [asterism](https://github.com/ynishi/asterism) — artifacts generated
  on macOS, regenerated once from Linux, two `#[cfg(target_os =
  "macos")]` modules silently gone from the committed inventory, and the
  macOS CI then red on a drift that could not be reproduced from Linux.
- **`--check` against artifacts recorded for another target now exits 3**
  ("not checked") rather than 2 ("drift"). The two want opposite
  reactions — regenerate, versus this host cannot say — and a caller
  that treats any non-zero code as failure keeps its old behaviour minus
  the false red. The MCP envelope grows `target_mismatch` for the same
  distinction.
- An artifact set with no manifest is treated as permission to proceed,
  not as a mismatch; otherwise the fence would lock every existing
  repository out of ever recording a triple. The consequence, worth
  stating: the *first* generation after upgrading is unfenced, so it
  should be run on the host the artifacts already belong to.

## [0.2.2] — 2026-08-15

### Added

- **`REQUIRED_NIGHTLY`**, public, and rustdoc now runs under it instead
  of `nightly`. Every nightly emits exactly one rustdoc JSON
  `format_version` and it changes whenever rustdoc's types do, so
  asking for `nightly` asked for "whatever the schema is today" — a
  moving target a fixed `rustdoc-types` cannot hit for long. A
  consumer's CI installed the current nightly and the run failed on
  `expected 60, found 61`: two tools disagreeing, in a run about
  neither. The pin travels with the `rustdoc-types` version; bump them
  together. Public so a consumer installs what the binary asks for
  rather than copying a date that then rots:
  `rustup toolchain install "$(cargo aidoc --print-required-toolchain)"`.
  Same shape as `public_api::MINIMUM_NIGHTLY_RUST_VERSION`.
- **`--print-required-toolchain`** prints it and exits, before anything
  that needs a workspace.
- **`--toolchain` / `Config::toolchain`** overrides the pin, for trying
  a format this build does not yet read.

### Changed

- rustdoc is spawned as `rustup run <toolchain> cargo rustdoc` rather
  than `cargo +<toolchain> rustdoc`. Equivalent where the rustup proxy
  is what gets spawned, and reliable where it is not — a `+toolchain`
  argument reaching a real `cargo` is an unknown subcommand, which is
  how the same call fails on Windows for other rustdoc-JSON consumers.
- The format-mismatch error names the toolchain it ran under, the one
  this build reads, and the `rustup` command that fixes it. Previously
  it said `expected 60, found 61`, from which the answer is not
  discoverable.

## [0.2.1] — 2026-08-15

### Added

- **`--title`** (`Config::title`) pins the `llms.txt` H1. Without it the
  title is the workspace root directory's basename, which makes a
  committed artifact depend on what the checkout happens to be called: a
  git worktree named after its branch bakes the branch slug into
  `llms.txt`, and a `--check` run from a normally-named clone then
  reports drift. Virtual workspaces have no root package name to fall
  back on, so a stable title has to come from the caller.

### Fixed

- **A `[lib] name` override no longer breaks the run.** The rustdoc JSON
  payload's filename comes from the *target* name, not the package
  name, and the two coincide often enough that the package name was
  used. Tauri v2 apps ship `<pkg>_lib` to dodge the bin/lib filename
  clash on Windows, and against such a workspace the run failed looking
  for a payload that was never going to be at that path. `Target::Lib`
  now carries the target name and the lookup uses it.

## [0.2.0] — 2026-07-18

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
