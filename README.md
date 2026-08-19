# cargo-aidoc

Generate LLM-facing doc artifacts (`llms.txt` / narrative markdown /
deterministic JSON) from rustdoc JSON.

`cargo-aidoc` walks a Rust workspace with `cargo_metadata`, runs
`rustdoc --output-format json` for each crate under a pinned nightly, and
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
- **`aidoc-manifest.json`** — which target triple the artifacts above
  describe. See [One target per artifact set](#one-target-per-artifact-set).

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
- `1` — pipeline error (rustdoc failed, I/O, config invalid), or a write
  refused because the committed artifacts describe another target.
- `2` — lint violation, or `--check` detected drift.
- `3` — `--check` could not answer: the committed artifacts describe
  another target.

## Excluding crates (`[workspace.metadata.aidoc]`)

Keep a crate family out of the artifact set — a pre-v0 plane, generated
code, anything whose narrative would push `llms-full.txt` over its
512 KiB soft cap — by listing package names in the workspace manifest:

```toml
[workspace.metadata.aidoc]
exclude = ["teams-core", "teams-infra"]
```

Every entry must name a package the workspace actually has: a typo, or
an entry left behind after a crate is renamed or removed, fails the run
with a config error rather than excluding nothing — the one failure
mode an exclude list cannot afford is being silently out of effect.

## One target per artifact set

rustdoc resolves `cfg` before it emits anything, so a module behind
`#[cfg(target_os = "macos")]` is in the JSON payload on a Mac and absent
everywhere else. The artifacts are therefore a property of the source
*and* the host that documented it, and `aidoc-manifest.json` records
which host that was.

Both front ends compare that record against the payload's own target
before doing anything:

```bash
cargo aidoc            # on another target: refuses to write, exit 1
cargo aidoc --check    # on another target: "NOT CHECKED", exit 3
cargo aidoc --retarget # move the artifacts to this target, on purpose
```

The refusal is the point. Regenerating from the wrong host deletes every
item only the recorded target documents, and that deletion is shaped
exactly like an ordinary regeneration in the diff — which is how it gets
committed and reviewed without anybody seeing it.

Two consequences worth stating:

- An artifact set with **no** manifest (committed before 0.3.0) is
  treated as permission to proceed. So the first `cargo aidoc` after
  upgrading is unfenced — run it on the host the artifacts belong to.
- Pick the target your CI runs the check on. Everyone else regenerates
  there, or the check has nothing to say.

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

**One dated nightly, and this binary tells you which.**

```bash
rustup toolchain install "$(cargo aidoc --print-required-toolchain)"
```

rustdoc's JSON payload carries a `format_version`; every nightly emits
exactly one, and it changes whenever rustdoc's types do. This crate
parses with a fixed `rustdoc-types`, so the two are one pair — which is
why the toolchain is pinned (`aidoc_core::REQUIRED_NIGHTLY`) rather than
being the `nightly` channel. Asking for the channel asks for whatever
the schema is today, and a CI job that installs the current nightly
eventually fails on a disagreement between two tools rather than on the
code under test.

Read the pin from the binary rather than copying the date: a copy goes
stale silently, and this moves whenever `rustdoc-types` does. Consumers
who want it at compile time can use `aidoc_core::REQUIRED_NIGHTLY`.

`--toolchain <name>` overrides it — the escape hatch for trying a format
this build does not yet read. A mismatch is a typed
`FormatVersionMismatch` naming both toolchains and the `rustup` command
that resolves it, never silent garbage.

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
