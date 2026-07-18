# aidoc-mcp — Onboarding

You are calling the **aidoc** MCP server. It projects a Rust workspace's
public API surface into LLM-friendly artifacts (a `llms.txt` index,
per-crate narrative Markdown, a deterministic `api/<crate>.json`, and,
optionally, an **error catalog**). The pipeline reads rustdoc JSON —
consumers do not depend on cargo-aidoc.

## Tools

| Tool | Purpose | Read/Write |
|---|---|---|
| `aidoc_info` | Report version and default configuration | Read |
| `aidoc_gen` | Run the pipeline and write artifacts to `out_dir` | Write |
| `aidoc_check` | Run the pipeline and diff against the on-disk copy | Read |
| `aidoc_error` | Fetch one or all catalogued diagnostics by `code` | Read |

Every tool response is a JSON envelope: `{ ok, summary, ... }`. `ok = false`
means the pipeline failed or (in `aidoc_check`) drift was detected.

## Resources

The server exposes short reference docs as MCP resources. Read them with
`resources/read` when you need orientation without a round-trip:

- `aidoc://guides/onboarding` — this page.
- `aidoc://guides/error-catalog` — how the error catalog works and what
  a consumer crate has to do to appear in it.

## When to prefer `aidoc_error` over reading files

If the workspace already has `errors/index.json` on disk, reading it
directly is fine. Use `aidoc_error` when:

- You have a single error code and only want that one entry (avoids
  loading the full index).
- The catalog hasn't been generated yet — `aidoc_error` runs the
  pipeline in-memory and returns the fresh result without writing.

## Output directory & `.gitignore`

`aidoc_gen` writes under `<workspace_root>/docs/aidoc/` by default. Whether
that directory is committed or ignored is a **consumer decision** —
cargo-aidoc does not touch `.gitignore`. Two common patterns:

- **Commit `docs/aidoc/`** and run `aidoc_check --strict` in CI so
  drift shows up as a review comment. Best for LLM-facing repos.
- **Ignore `docs/aidoc/`** (add it to `.gitignore`) and regenerate on
  demand. Best for repos where doc artefacts are large or where a
  downstream service crawls them out-of-band.

## Typical loop for an AI editing a DSL crate

1. See a compiler / runtime error with a stable code (e.g. `EBP001`).
2. Call `aidoc_error` with that code to get the entry: message, help,
   URL, description, tagged snippets (including any labelled `fix`).
3. Apply the fix inline; no source-code grep needed.
