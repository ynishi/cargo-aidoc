# aidoc-mcp — Error Catalog Guide

The error catalog is an LLM-facing index of every diagnostic a
workspace's crates can emit, sourced from rustdoc JSON. Its purpose
is to let an AI (or human reader) resolve a stable error code into a
complete "how do I fix it" package — message, help text, reference URL,
description, and tagged code snippets — **without reading source**.

## Consumer contract (zero coupling)

A crate joins the catalog by doing three things. **No dependency on
`cargo-aidoc` is required** — the extractor reads rustdoc JSON.

### 1. Derive `miette::Diagnostic`

```rust
use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
#[error("stage `{stage}` referenced by pipeline has no seed in context.d")]
#[diagnostic(
    code(EBP001),
    help("seed every stage name in `context.d` at swarm_run time"),
    url("mse://guides/errors/EBP001")
)]
pub struct MissingInitCtxSeed {
    pub stage: String,
}
```

The extractor recognises three arguments inside `#[diagnostic(...)]`:

- `code(NAME)` — **required**. Becomes `ErrorEntry.code`.
- `help("...")` — optional. Becomes `ErrorEntry.help`.
- `url("...")` — optional. Becomes `ErrorEntry.url`. Free-form scheme
  (`https://`, `mse://guides/...`, etc).

The `#[error("...")]` line from `thiserror` is picked up too and lands
in `ErrorEntry.message_template` with placeholder tokens (`{field}`)
preserved.

### 2. Write a doc comment on the type

The full doc body lands in `ErrorEntry.docs`. Write it as narrative
prose describing what went wrong and why; the reader treats it as the
long-form description.

### 3. Tag examples with the error code

Fenced code blocks whose info-string carries `code=<this-error's-code>`
are extracted as [`Snippet`]s. Any bare (non-`key=value`) tag becomes a
snippet heading — `fix` is the recommended convention for the
correction path.

````markdown
/// ```ebp,code=EBP001
/// -- BAD: pipeline references `ingest` but context has no seed for it.
/// local flow = B.pipeline({
///     { name = "ingest", input = { path = "$.d.ingest" } },
/// })
/// ```
///
/// ```ebp,code=EBP001,fix
/// -- GOOD: seed every stage in context.d at launch.
/// swarm_run(flow, {
///     context = { d = { ingest = "..." } },
/// })
/// ```
````

The info-string shape is `<lang>[,tag_or_kv]*`. Any `key=value` pair
other than `code=` is currently ignored but preserved for future use
(so it's safe to add new tags now — they simply won't affect grouping).

## Output artefacts

When `aidoc_gen` runs with the error catalog enabled it emits:

- `errors/<CODE>.md` — one narrative page per catalogued diagnostic,
  containing message, help, reference URL, defined-in path, description,
  and every tagged snippet grouped by its bare tags.
- `errors/index.json` — deterministic JSON array. Wire format:

  ```json
  [{
    "code": "EBP001",
    "crate_name": "aidoc-spike",
    "item_path": "aidoc_spike::MissingInitCtxSeed",
    "message_template": "stage `{stage}` referenced by pipeline …",
    "help": "seed every stage name …",
    "url": "mse://guides/errors/EBP001",
    "docs": "…full doc body…",
    "snippets": [
      { "lang": "ebp", "tags": [], "body": "-- BAD: …" },
      { "lang": "ebp", "tags": ["fix"], "body": "-- GOOD: …" }
    ]
  }]
  ```

- `llms-errors.txt` — top-level index in the shape of `llms.txt` so
  LLM tooling already conditioned on [llmstxt.org](https://llmstxt.org)
  can consume it without a new parser.

### When the catalog is empty

If the workspace has no `#[derive(miette::Diagnostic)]` items, the
generator emits `errors/index.json` as `[]` and `llms-errors.txt`
with a "no diagnostics catalogued yet" placeholder, but does **not**
create any `errors/<CODE>.md` files. That is the intended shape — a
zero-state catalog is a single deterministic pair of files, not an
empty directory.

## Fetching a single entry

Call `aidoc_error` with `{ "code": "EBP001" }` to get the single entry
without loading the full index. Omit `code` to get a compact summary
of every catalogued diagnostic.

## What is covered

The catalog picks up three shapes:

1. **Struct-level** `#[derive(miette::Diagnostic)]` with
   `#[diagnostic(code(...), ...)]` — one entry per struct.
2. **Enum-level** `#[diagnostic(code(...), ...)]` on a
   `#[derive(Diagnostic)]` enum — one entry per enum (rare; miette
   permits at most one code per enum).
3. **Per-variant** `#[diagnostic(code(...), ...)]` on an enum's
   variants — one entry per catalogued variant. This is the
   dominant `thiserror + miette` shape and works out of the box; the
   `item_path` becomes `<crate>::<enum_path>::<VariantName>`.
   Variants without a `#[diagnostic]` line are silently skipped.

## Not covered by the current version

- `source_code` / `label` / `related` metadata from miette (variant-
  or struct-level) is not surfaced yet — those are runtime-facing.
- Non-miette diagnostic frameworks would need their own parser. The
  extractor is written so another parser can be added alongside the
  miette one without touching the crate walk.
