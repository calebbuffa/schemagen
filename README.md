# schemagen

Generate Rust data-model types from draft-07 JSON Schema files.

One JSON Schema tree plus one JSON config and an optional consumer policy become
idiomatic, deterministic Rust structs.

## What it produces

- A `struct` per schema object type, with doc comments from schema descriptions
- `#[serde(rename_all = "camelCase")]`, `#[serde(default)]`, and `skip_serializing_if`
- Cross-file and fragment `$ref` resolution, including RFC 6901 escaping
- Boolean schemas, nullable `anyOf`/`oneOf`, recursive object `allOf`, enums,
  fixed arrays, maps, defaults, and documented diagnostics
- Config-driven class/property names, type overrides, defaults, and custom
  numeric or string enums
- Policy-provided aliases, fields, attributes, and consumer-specific types

General unions and unsupported Draft-07 keywords produce diagnostics rather
than silently becoming generated structs. Consumers can explicitly choose a
lossy policy when their runtime model requires it.

## Library and binary

The library and binary use the same pipeline: `Graph`, `generate_types`, and
`render_module`. A consumer policy implements `GenerationPolicy` when the
target runtime needs types or fields not expressible by JSON Schema alone.

```sh
cargo run -- \
  --schema path/to/file.schema.json \
  --config my-config.json \
  --output src/
```

Referenced schemas are resolved relative to the root schema's directory.
