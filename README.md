# cargo-aidoc

Generate LLM-facing doc artifacts (llms.txt / markdown / machine JSON) from rustdoc JSON.

Status: early scaffold (pre-implementation). No pipeline logic exists yet —
this repository currently defines the workspace layout and crate
boundaries only.

## Planned features

- `llms.txt` generation from a crate's public API surface
- Narrative markdown documentation that mirrors the crate/module structure
- Deterministic, machine-readable JSON output for diffing and CI checks
- `cargo aidoc --check --strict` mode for CI doc-coverage enforcement
- An MCP server exposing the same pipeline as callable tools

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
