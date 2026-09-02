use serde::Deserialize;
use std::collections::HashMap;

/// Generator configuration - equivalent to the Node.js `glTF.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Per-class overrides keyed by the schema `title`.
    #[serde(default)]
    pub classes: HashMap<String, ClassConfig>,

    /// Custom type definitions (enums, etc.) to generate at module level.
    #[serde(default, rename = "customTypes")]
    pub custom_types: HashMap<String, CustomTypeConfig>,

    /// Additional root schema files to process, relative to `--schema-dir`.
    /// These are seeded into the BFS queue alongside the main root schema.
    #[serde(default, rename = "additionalSchemas")]
    pub additional_schemas: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassConfig {
    /// Rename the schema title to this Rust type name.
    pub override_name: Option<String>,

    /// If true, this type will NOT be generated - it is either a base class
    /// whose properties are inlined, or a type alias (like `serde_json::Value`).
    #[serde(default)]
    pub skip: bool,

    /// Per-property type overrides: property name -> Rust type string.
    /// Example: { "componentType": "u32", "mode": "u32" }
    /// If the value starts with "Option<", the field is treated as optional
    /// (skip_serializing_if = "Option::is_none" is emitted automatically).
    #[serde(default)]
    pub property_overrides: HashMap<String, String>,

    /// Per-property JSON defaults used when the schema omits a semantic default.
    #[serde(default, rename = "propertyDefaults")]
    pub property_defaults: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTypeConfig {
    /// Type kind: "enum" for now.
    pub kind: String,

    /// For enums: list of variant names.
    #[serde(default)]
    pub variants: Vec<String>,

    /// Doc comment for the type.
    #[serde(default)]
    pub doc: Option<String>,

    /// For numeric enums: deserialize from u32 indices instead of string names.
    #[serde(default)]
    pub numeric: bool,

    /// For numeric enums: maps variant names to actual numeric values.
    /// If not provided, uses 0-based indices.
    #[serde(default)]
    pub numeric_values: std::collections::HashMap<String, u32>,

    /// Default variant name (PascalCase). If not provided, defaults to the
    /// first variant in the `variants` list.
    #[serde(default)]
    pub default: Option<String>,

    /// Human-readable explanation of why this custom type exists (e.g. which
    /// schema field it overrides, or which extension requires it).
    /// This is emitted as a comment in the generated code and in MANIFEST.md.
    #[serde(default)]
    pub origin: Option<String>,
}
