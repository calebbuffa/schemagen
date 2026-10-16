//! Data-driven, per-schema exceptions.
//!
//! This is the *table* half of customisation: entries keyed by a schema title
//! or a property that no general rule predicts, such as one schema whose Rust
//! name must be spelled differently, one property whose type the schema
//! understates, or one enum value a producer spells two ways.
//!
//! Anything that is a rule rather than an exception - how primitives map onto
//! Rust types, which schemas to emit - belongs in
//! [`GenerationPolicy`](crate::policy::GenerationPolicy) instead, where it is
//! type-checked. The split is deliberate: every deviation a consumer needs
//! that can be written as a table lives here, so adding one is a config edit,
//! not a code change.
//!
//! The format is strict. Unknown keys, unparseable Rust types and entries that
//! contradict each other are rejected at load time, and entries that match
//! nothing in the schemas are reported after generation, so a typo or a stale
//! entry never silently does nothing.
//!
//! # Format
//!
//! ```json
//! {
//!   "typeNaming": "title",
//!   "imports": ["crate::VertexAttribute"],
//!   "schemas": {
//!     "Mesh Primitive": {
//!       "overrideName": "Primitive",
//!       "propertyTypes": { "attributes": "SortedMap<VertexAttribute, u32>" },
//!       "propertyDefaults": { "mode": 4 },
//!       "propertyRequired": { "normal": false },
//!       "propertySkipIf": { "version": "is_zero" },
//!       "propertySkip": ["extras"],
//!       "extraFields": [{ "name": "data", "type": "bytes::Bytes", "skipSerde": true }]
//!     },
//!     "glTF Id": { "skip": true, "replaceWith": "u32" }
//!   },
//!   "fieldTypes": { "*.extras": "Option<Box<serde_json::Value>>" },
//!   "enumNames": { "HeaderValue.valueType": "HeaderValueType" },
//!   "enumAliases": { "*.resourcePattern": { "featureData": ["FeatureData"] } },
//!   "customTypes": {
//!     "Refine": { "kind": "enum", "variants": ["ADD", "REPLACE"] },
//!     "AnyValue": { "kind": "alias", "target": "serde_json::Value" }
//!   },
//!   "additionalSchemas": ["Subtree/subtree.schema.json"]
//! }
//! ```
//!
//! Keys of `schemas` are schema titles and match exactly: `"mesh"` does not
//! find a schema titled `Mesh`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

mod builder;

pub use builder::{
    ConfigBuilder, CustomEnumBuilder, ExtraFieldBuilder, IntoRustType, SchemaBuilder,
};

use crate::error::{Error, Result};
use crate::model::RustType;

/// How a type's Rust name is derived when nothing overrides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeNaming {
    /// From the schema `title`. A title shared by several schemas is
    /// qualified with its file name so the types stay distinct.
    #[default]
    Title,
    /// From the schema's file name, ignoring its title.
    FileStem,
}

/// Per-schema overrides, loaded from JSON alongside the schemas themselves.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// How type names are derived from schemas.
    #[serde(default)]
    pub type_naming: TypeNaming,

    /// Paths to `use` at the top of the generated module, written as they
    /// follow the keyword: `"crate::VertexAttribute"`, `"super::cmn::*"`.
    #[serde(default)]
    pub imports: Vec<String>,

    /// Per-schema overrides keyed by the schema `title`.
    #[serde(default)]
    pub schemas: BTreeMap<String, SchemaConfig>,

    /// Rust types for properties, keyed by `"OwnerType.property"` or by
    /// `"*.property"` to match the property on every type. Consulted after the
    /// owner's own `propertyTypes`.
    #[serde(default)]
    pub field_types: BTreeMap<String, RustType>,

    /// Types to generate that no schema describes: enums and aliases.
    #[serde(default)]
    pub custom_types: BTreeMap<String, CustomTypeConfig>,

    /// Additional root schema files to process, relative to the schema
    /// directory. These are seeded alongside the main root schema.
    #[serde(default)]
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
    #[serde(default)]
    pub enum_names: BTreeMap<String, String>,

    /// Extra accepted spellings for enum values, keyed by
    /// `"OwnerType.property"` or `"*.property"`, then by the schema's value.
    ///
    /// Serialization always writes the schema's value. This is how a consumer
    /// tolerates a producer whose output differs from its own specification
    /// without editing the schema.
    #[serde(default)]
    pub enum_aliases: BTreeMap<String, BTreeMap<String, Vec<String>>>,

    /// Gives every object struct `extensions` and `extras` members.
    #[serde(default)]
    pub extensible: bool,
}

/// Keys that older versions accepted and that now belong to
/// [`GenerationPolicy::settings`](crate::policy::GenerationPolicy::settings).
const RELOCATED_SETTINGS: [&str; 5] = [
    "numberType",
    "integerType",
    "stringType",
    "mapRepresentation",
    "inferOpenEnums",
];

impl Config {
    /// Parses and validates a configuration document.
    pub fn from_json_str(text: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| Error::config(format!("not valid JSON: {error}")))?;
        if let Some(object) = value.as_object() {
            let stale: Vec<_> = RELOCATED_SETTINGS
                .iter()
                .filter(|key| object.contains_key(**key))
                .copied()
                .collect();
            if !stale.is_empty() {
                return Err(Error::config(format!(
                    "{} now belong to GenerationPolicy::settings; \
                     remove them from the config and return a TypeSettings instead",
                    stale.join(", ")
                )));
            }
        }
        let config: Self =
            serde_json::from_value(value).map_err(|error| Error::config(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Reads, parses and validates the configuration at `path`.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::from_json_str(&text)
            .map_err(|error| Error::config(format!("{}: {error}", path.display())))
    }

    /// Finds the overrides configured for a schema titled `title`. Matching is
    /// exact: titles that differ only in case are different schemas, and a
    /// fuzzy match would rename one silently. Returns the matching key too,
    /// so callers can track which entries are used.
    pub fn schema(&self, title: &str) -> Option<(&str, &SchemaConfig)> {
        self.schemas
            .get_key_value(title)
            .map(|(key, schema)| (key.as_str(), schema))
    }

    /// Checks the entries that deserialization alone cannot.
    pub fn validate(&self) -> Result<()> {
        for import in &self.imports {
            syn::parse_str::<syn::ItemUse>(&format!("use {import};")).map_err(|error| {
                Error::config(format!("imports: `{import}` is not a use path: {error}"))
            })?;
        }
        let owned_keys = self
            .field_types
            .keys()
            .chain(self.enum_names.keys())
            .chain(self.enum_aliases.keys());
        for key in owned_keys {
            let well_formed = key
                .split_once('.')
                .is_some_and(|(owner, property)| !owner.is_empty() && !property.is_empty());
            if !well_formed {
                return Err(Error::config(format!(
                    "`{key}` must be written `OwnerType.property` (or `*.property`)"
                )));
            }
        }
        for key in self.enum_names.keys() {
            if key.starts_with("*.") {
                return Err(Error::config(format!(
                    "enumNames key `{key}` must name one owner; wildcards would give \
                     several enums the same name"
                )));
            }
        }
        for (title, schema) in &self.schemas {
            schema.validate(title)?;
        }
        for (name, custom) in &self.custom_types {
            custom.validate(name)?;
        }
        Ok(())
    }
}

/// Overrides for one schema, keyed by its title in [`Config::schemas`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SchemaConfig {
    /// The Rust type name to use instead of the derived one.
    #[serde(default)]
    pub override_name: Option<String>,

    /// When true this schema generates no type: it is a base schema whose
    /// properties are inlined, or a type provided elsewhere. References to it
    /// become `replaceWith` when set, otherwise `serde_json::Value`.
    #[serde(default)]
    pub skip: bool,

    /// The type that references to this (skipped) schema resolve to, such as
    /// `u32` for an id schema. Implies `skip`.
    #[serde(default)]
    pub replace_with: Option<RustType>,

    /// Per-property Rust types, replacing what the schema implies.
    #[serde(default)]
    pub property_types: BTreeMap<String, RustType>,

    /// Per-property JSON defaults used when the schema omits one.
    #[serde(default)]
    pub property_defaults: BTreeMap<String, serde_json::Value>,

    /// Per-property `skip_serializing_if` function paths.
    #[serde(default)]
    pub property_skip_if: BTreeMap<String, String>,

    /// Overrides whether a property is required when deserializing, replacing
    /// the schema's `required` list. `false` accepts documents that omit a
    /// property the schema demands, which is how a consumer tolerates a
    /// producer that deviates from its own specification. The field becomes
    /// an `Option`.
    #[serde(default)]
    pub property_required: BTreeMap<String, bool>,

    /// Properties to leave out of the generated struct.
    #[serde(default)]
    pub property_skip: Vec<String>,

    /// Members the schema does not describe.
    #[serde(default)]
    pub extra_fields: Vec<ExtraFieldConfig>,
}

impl SchemaConfig {
    /// Whether the schema generates no type.
    pub fn is_skipped(&self) -> bool {
        self.skip || self.replace_with.is_some()
    }

    fn validate(&self, title: &str) -> Result<()> {
        if self.skip && self.override_name.is_some() && self.replace_with.is_none() {
            return Err(Error::config(format!(
                "schemas.{title}: `skip` with `overrideName` names a type that is never \
                 generated; use `replaceWith` to say what references become"
            )));
        }
        if self.is_skipped()
            && (!self.property_types.is_empty()
                || !self.property_defaults.is_empty()
                || !self.extra_fields.is_empty())
        {
            return Err(Error::config(format!(
                "schemas.{title}: a skipped schema has no properties to configure"
            )));
        }
        for field in &self.property_skip {
            if self.property_types.contains_key(field) {
                return Err(Error::config(format!(
                    "schemas.{title}: `{field}` is both skipped and given a type"
                )));
            }
        }
        for field in &self.extra_fields {
            if syn::parse_str::<syn::Ident>(&field.name).is_err() {
                return Err(Error::config(format!(
                    "schemas.{title}: extra field `{}` is not a valid identifier",
                    field.name
                )));
            }
        }
        Ok(())
    }
}

/// A member added to a struct that the schema does not describe.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExtraFieldConfig {
    /// The Rust field name.
    pub name: String,
    /// The field's type. Must implement `Default`.
    #[serde(rename = "type")]
    pub ty: RustType,
    /// Documentation for the field.
    #[serde(default)]
    pub doc: Option<String>,
    /// Excludes the field from (de)serialization. Otherwise it is read as
    /// `#[serde(default)]`.
    #[serde(default)]
    pub skip_serde: bool,
}

/// What a [`CustomTypeConfig`] generates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomKind {
    /// An enum with the listed variants.
    Enum,
    /// A `type` alias for `target`.
    Alias,
}

/// A type to generate that no schema describes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CustomTypeConfig {
    /// What to generate.
    pub kind: CustomKind,

    /// Enums: the variant names, as they appear on the wire for string enums.
    #[serde(default)]
    pub variants: Vec<String>,

    /// Aliases: the aliased type.
    #[serde(default)]
    pub target: Option<RustType>,

    /// Doc comment for the type.
    #[serde(default)]
    pub doc: Option<String>,

    /// Enums: (de)serialize as integers instead of names.
    #[serde(default)]
    pub numeric: bool,

    /// Numeric enums: the value of each variant. Variants not listed take
    /// their 0-based position.
    #[serde(default)]
    pub numeric_values: BTreeMap<String, i64>,

    /// Enums: the variant `Default` returns, as listed in `variants`. The
    /// first one when omitted.
    #[serde(default)]
    pub default: Option<String>,

    /// Why this type exists: which schema field it overrides, or which
    /// extension requires it. Recorded in the generation manifest.
    #[serde(default)]
    pub origin: Option<String>,
}

impl CustomTypeConfig {
    fn validate(&self, name: &str) -> Result<()> {
        let fail = |message: String| Err(Error::config(format!("customTypes.{name}: {message}")));
        match self.kind {
            CustomKind::Alias => {
                if self.target.is_none() {
                    return fail("an alias needs a `target`".into());
                }
                if !self.variants.is_empty() || self.numeric {
                    return fail("an alias has no variants".into());
                }
            }
            CustomKind::Enum => {
                if self.target.is_some() {
                    return fail("an enum has no `target`".into());
                }
                if self.variants.is_empty() {
                    return fail("an enum needs at least one variant".into());
                }
                if let Some(default) = &self.default
                    && !self.variants.contains(default)
                {
                    return fail(format!("default `{default}` is not one of the variants"));
                }
                if !self.numeric && !self.numeric_values.is_empty() {
                    return fail("`numericValues` requires `numeric`".into());
                }
                if let Some(unknown) = self
                    .numeric_values
                    .keys()
                    .find(|key| !self.variants.contains(key))
                {
                    return fail(format!("`numericValues` names unknown variant `{unknown}`"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> Result<Config> {
        Config::from_json_str(json)
    }

    #[test]
    fn a_config_of_pure_exceptions_is_valid() {
        assert!(config(r#"{"schemas": {"Tile": {"skip": true}}}"#).is_ok());
        assert!(config("{}").is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        for json in [
            r#"{"schemaz": {}}"#,
            r#"{"classes": {}}"#,
            r#"{"schemas": {"Tile": {"propertyOverrides": {}}}}"#,
            r#"{"customTypes": {"A": {"kind": "enum", "variants": ["x"], "colour": 1}}}"#,
            r#"{"schemas": {"Tile": {"extraFields": [{"name": "a", "type": "u8", "rustType": "u8"}]}}}"#,
        ] {
            let error = config(json).expect_err(json).to_string();
            assert!(error.contains("unknown field"), "{json}: {error}");
        }
    }

    #[test]
    fn settings_that_moved_to_the_policy_are_rejected_with_an_explanation() {
        for key in RELOCATED_SETTINGS {
            let error = config(&format!(r#"{{"{key}": true}}"#))
                .expect_err(key)
                .to_string();
            assert!(error.contains(key), "{key}: {error}");
            assert!(error.contains("GenerationPolicy"), "{key}: {error}");
        }
    }

    #[test]
    fn invalid_rust_types_fail_at_load_time() {
        let error = config(r#"{"schemas": {"Tile": {"propertyTypes": {"a": "&str"}}}}"#)
            .expect_err("a reference type is not modelled")
            .to_string();
        assert!(error.contains("&str"), "{error}");
    }

    #[test]
    fn schema_lookup_is_exact() {
        let config = config(r#"{"schemas": {"buffer": {}, "Buffer": {"skip": true}}}"#).unwrap();
        assert_eq!(config.schema("Buffer").unwrap().0, "Buffer");
        assert_eq!(config.schema("buffer").unwrap().0, "buffer");
        assert!(config.schema("BUFFER").is_none());
        assert!(config.schema("Mesh").is_none());
    }

    #[test]
    fn replace_with_implies_skip() {
        let config = config(r#"{"schemas": {"glTF Id": {"replaceWith": "u32"}}}"#).unwrap();
        assert!(config.schema("glTF Id").unwrap().1.is_skipped());
    }

    #[test]
    fn contradictory_schema_entries_are_rejected() {
        for json in [
            r#"{"schemas": {"A": {"skip": true, "overrideName": "B"}}}"#,
            r#"{"schemas": {"A": {"skip": true, "propertyTypes": {"x": "u8"}}}}"#,
            r#"{"schemas": {"A": {"propertySkip": ["x"], "propertyTypes": {"x": "u8"}}}}"#,
            r#"{"schemas": {"A": {"extraFields": [{"name": "not ident", "type": "u8"}]}}}"#,
        ] {
            assert!(config(json).is_err(), "{json}");
        }
    }

    #[test]
    fn keys_naming_an_owner_and_property_must_have_both() {
        for json in [
            r#"{"fieldTypes": {"extras": "u8"}}"#,
            r#"{"enumNames": {".x": "A"}}"#,
            r#"{"enumAliases": {"Owner.": {}}}"#,
            r#"{"enumNames": {"*.x": "A"}}"#,
        ] {
            assert!(config(json).is_err(), "{json}");
        }
        assert!(config(r#"{"fieldTypes": {"*.extras": "u8"}}"#).is_ok());
    }

    #[test]
    fn imports_must_be_use_paths() {
        assert!(config(r#"{"imports": ["crate::A", "super::cmn::*"]}"#).is_ok());
        assert!(config(r#"{"imports": ["not a path"]}"#).is_err());
    }

    #[test]
    fn custom_types_are_cross_checked() {
        for json in [
            r#"{"customTypes": {"A": {"kind": "alias"}}}"#,
            r#"{"customTypes": {"A": {"kind": "enum"}}}"#,
            r#"{"customTypes": {"A": {"kind": "enum", "variants": ["x"], "default": "y"}}}"#,
            r#"{"customTypes": {"A": {"kind": "enum", "variants": ["x"], "numericValues": {"x": 1}}}}"#,
            r#"{"customTypes": {"A": {"kind": "enum", "variants": ["x"], "numeric": true, "numericValues": {"z": 1}}}}"#,
            r#"{"customTypes": {"A": {"kind": "alias", "target": "u8", "variants": ["x"]}}}"#,
        ] {
            assert!(config(json).is_err(), "{json}");
        }
        assert!(
            config(r#"{"customTypes": {"A": {"kind": "alias", "target": "serde_json::Value"}}}"#)
                .is_ok()
        );
    }
}
