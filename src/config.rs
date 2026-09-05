//! Data-driven, per-schema exceptions.
//!
//! This is the *table* half of customisation: entries keyed by a schema title
//! that no general rule predicts, such as one schema whose Rust name must be
//! spelled differently or one property whose type the schema understates.
//!
//! Anything that is a rule rather than an exception — how primitives map onto
//! Rust types, how names are derived, which schemas to emit — belongs in
//! [`GenerationPolicy`] instead, where it is type-checked. See that module's
//! documentation for the reasoning.
//!
//! [`GenerationPolicy`]: crate::policy::GenerationPolicy

use serde::Deserialize;
use std::collections::HashMap;

/// Per-schema overrides, loaded from JSON alongside the schemas themselves.
#[derive(Debug, Clone, Default, Deserialize)]
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

    /// Explicit names for enums generated from a property, keyed by
    /// `"OwnerType.propertyName"`.
    ///
    /// Enums that a schema declares inline have no name of their own, so one
    /// is derived from the owning type and property. That derivation is
    /// mechanical and sometimes produces a name that is either redundant
    /// (`HeaderValueValueType`) or too generic to survive alongside its peers
    /// (`ValueType`). Naming such an enum here pins it, and the chosen name is
    /// reserved so no other enum can shorten onto it.
    #[serde(default, rename = "enumNames")]
    pub enum_names: HashMap<String, String>,

    /// Settings that used to live here and now belong to
    /// [`GenerationPolicy::settings`].
    ///
    /// Deserialising them rather than ignoring them turns a stale config into
    /// an error at load time. Without this a leftover `"stringType"` entry
    /// would parse cleanly and do nothing, and the generator would quietly
    /// emit larger types than the author asked for.
    ///
    /// [`GenerationPolicy::settings`]: crate::policy::GenerationPolicy::settings
    #[serde(flatten)]
    relocated: RelocatedSettings,
}

impl Config {
    /// Rejects a config that still carries settings which have moved into
    /// [`GenerationPolicy`](crate::policy::GenerationPolicy).
    pub fn validate(&self) -> Result<(), String> {
        let stale = [
            ("numberType", self.relocated.number_type.is_some()),
            ("integerType", self.relocated.integer_type.is_some()),
            ("stringType", self.relocated.string_type.is_some()),
            (
                "mapRepresentation",
                self.relocated.map_representation.is_some(),
            ),
            ("inferOpenEnums", self.relocated.infer_open_enums.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, present)| present.then_some(name))
        .collect::<Vec<_>>();
        if stale.is_empty() {
            return Ok(());
        }
        Err(format!(
            "config sets {}, which now belong to GenerationPolicy::settings; \
             remove them from the config and return a TypeSettings instead",
            stale.join(", ")
        ))
    }
}

/// Settings that have moved out of the config, retained only so that a config
/// still carrying them fails loudly instead of silently losing them.
#[derive(Debug, Clone, Default, Deserialize)]
struct RelocatedSettings {
    #[serde(default, rename = "numberType")]
    number_type: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "integerType")]
    integer_type: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "stringType")]
    string_type: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "mapRepresentation")]
    map_representation: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "inferOpenEnums")]
    infer_open_enums: Option<serde::de::IgnoredAny>,
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

#[cfg(test)]
mod tests {
    use super::Config;

    fn config(json: &str) -> Config {
        serde_json::from_str(json).expect("config must parse")
    }

    #[test]
    fn a_config_of_pure_exceptions_is_valid() {
        assert!(
            config(r#"{"classes": {"Tile": {"skip": true}}}"#)
                .validate()
                .is_ok()
        );
        assert!(config("{}").validate().is_ok());
    }

    #[test]
    fn settings_that_moved_to_the_policy_are_rejected_rather_than_ignored() {
        // These would otherwise parse cleanly and do nothing, quietly costing
        // the representation the author asked for.
        for (json, expected) in [
            (r#"{"stringType": "Box<str>"}"#, "stringType"),
            (
                r#"{"mapRepresentation": "sorted-slice"}"#,
                "mapRepresentation",
            ),
            (r#"{"numberType": "f32"}"#, "numberType"),
            (r#"{"integerType": "u32"}"#, "integerType"),
            (r#"{"inferOpenEnums": true}"#, "inferOpenEnums"),
        ] {
            let error = config(json).validate().expect_err(json);
            assert!(error.contains(expected), "{json}: {error}");
            assert!(error.contains("GenerationPolicy"), "{json}: {error}");
        }
    }
}
