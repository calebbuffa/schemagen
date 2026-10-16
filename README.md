# schemagen

Generate Rust data-model types from draft-07 JSON Schema files.

Schemas + a strict JSON config + an optional Rust `GenerationPolicy` produce
deterministic, lint-clean, idiomatic serde types. Most schema-specific
decisions live in the config, so adopting a new schema rarely needs code.

## Pipeline

`Graph` (load) -> `resolve::generate` (IR) -> `render_module(s)` (tokens -> text).
`driver::run` (feature `cli`, default) wires these into the command line.

## CLI

```sh
schemagen --schema schema/ --schema extra.schema.json \
  --search-path vendor/ --config config.json \
  --output src/generated.rs [--manifest manifest.json] \
  [--module-doc "text"] [--check] [--deny-warnings]
```

- `--schema` file or directory, repeatable (required); `--search-path` adds
  `$ref` resolution roots.
- `--output` is a single file. `--check` fails if the file would change
  (`.rs` is compared as tokens, so formatting is not drift).
- Errors always fail; warnings fail only with `--deny-warnings`.
- Unused config entries are reported as warnings.

## Config (strict: unknown keys are rejected)

Top level: `typeNaming` (`title` | `fileStem`), `imports` (`use` paths such as
`super::cmn::*`), `classes`, `fieldTypes`, `customTypes`, `additionalSchemas`,
`enumNames`, `enumAliases`, `extensible`.

- `classes.<Title>`: `overrideName`, `skip`, `replaceWith`, `propertyTypes`,
  `propertyDefaults`, `propertySkipIf`, `propertyRequired`, `propertySkip`,
  `extraFields` (`name`, `type`, `doc`, `skipSerde`).
- `fieldTypes`: `"Owner.prop"` or `"*.prop"` → Rust type. Owners are Rust
  type names; class keys are schema titles.
- `customTypes`: `kind` enum/alias, `variants`, `numeric`, `numericValues`,
  `target`, `default`, `doc`.
- `enumNames`, `enumAliases`: rename inferred enums / add serde aliases.

Precedence for field types: policy → class `propertyTypes` → `fieldTypes`
(`Owner.prop`, then `*.prop`) → inference. Required: class
`propertyRequired` → policy → schema `required`.

Representation choices (numbers, strings, maps, open enums) are
`GenerationSettings` on the policy, not config.

## Policy

`GenerationPolicy` (all methods defaulted): `type_name`, `field_type`,
`field_required`, `skip_serializing_if`, `should_generate`,
`additional_definitions`, struct augmentation. Use it only for logic that
cannot be data.

## Generated runtime helpers

`EmptyCollection`, `BoxedSlice`, `SortedMap`, `InlineVec` are real Rust
sources (`src/runtime`), embedded with `include_str!` and emitted only when
used. Multi-module output (`render_modules`) emits them once.

## Migrating from 0.1

- Config: `propertyOverrides` → `propertyTypes`, `rustType` → `type`;
  `extensionTrait`/`propertySkipIf` rules moved to policy/class config.
- Number/string/map/open-enum settings moved from config to policy settings.
- `--output` is a file; custom numeric enums are `i64`-based (no `serde_repr`);
  custom aliases are `pub type`; closed enums derive `Hash`; extensible
  `extensions`/`extras` skip serializing when empty.
