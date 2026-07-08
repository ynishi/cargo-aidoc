# cargo-aidoc

Generates LLM-facing documentation artifacts (llms.txt, narrative markdown,
deterministic JSON) from rustdoc JSON output. The workspace is split into a
core library (`aidoc-core`) and two thin front ends: a `cargo aidoc`
subcommand (`cargo-aidoc`) and an MCP server (`aidoc-mcp`).

## Doc language policy

Doc committed to the repo (`README.md`, `CHANGELOG.md`, crate `description`
fields, source doc comments) is written in English. This `CLAUDE.md` file
and anything under `workspace/` may be written in Japanese.

## Design source of truth

New design / architecture documentation lives in crate-root doc comments
(`//!` in each `src/lib.rs` / `src/main.rs`), not in separate
`docs/design/*.md` files. See `code-doc-as-design-discipline.md` for the
rationale.

## Spec origin

Original spec: mini-app issue `38cda5f3`.
