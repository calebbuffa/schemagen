//! Building a [`Config`] in Rust.
//!
//! A JSON config is the right shape for the `schemagen` command line, but a
//! consumer that already owns a generator program gains from writing the same
//! decisions as code: they are type-checked, can carry comments explaining
//! why each exists, and need no file to be found at run time.
//!
//! Every setter takes the arguments the JSON form takes, and
//! [`ConfigBuilder::build`] runs the same validation, so the two forms accept
//! and reject exactly the same configurations. Mistakes made along the way
//! (an unparsable type, a key set twice) are collected and reported together
//! by `build` rather than aborting the chain at the first one.
//!
//! ```
//! use schemagen::Config;
//!
//! let config = Config::builder()
//!     .extensible(true)
//!     .field_type("*.bitstream", "Option<usize>")
//!     .schema("Tile", |schema| {
//!         schema
//!             .override_name("Tile")
//!             .property_type("refine", "Option<Refine>")
//!             .property_default("transform", [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1])
//!     })
//!     .custom_enum("Refine", |custom| custom.variants(["ADD", "REPLACE"]))
//!     .build()
//!     .unwrap();
//! assert!(config.extensible);
//! ```

use std::collections::BTreeMap;

use super::{Config, CustomKind, CustomTypeConfig, ExtraFieldConfig, SchemaConfig, TypeNaming};
use crate::error::{Error, Result};
use crate::model::RustType;

/// Something that names a Rust type: source text such as `"Option<u8>"`, or
/// an already built [`RustType`].
pub trait IntoRustType {
    /// Parses or passes through the type.
    fn into_rust_type(self) -> Result<RustType>;
}

impl IntoRustType for RustType {
    fn into_rust_type(self) -> Result<RustType> {
        Ok(self)
    }
}

impl IntoRustType for &str {
    fn into_rust_type(self) -> Result<RustType> {
        self.parse()
    }
}

impl IntoRustType for String {
    fn into_rust_type(self) -> Result<RustType> {
        self.parse()
    }
}

impl IntoRustType for &String {
    fn into_rust_type(self) -> Result<RustType> {
        self.parse()
    }
}

/// Problems found while building, reported together by `build`.
#[derive(Debug, Default)]
struct Problems(Vec<String>);

impl Problems {
    fn add(&mut self, message: impl Into<String>) {
        self.0.push(message.into());
    }

    fn absorb(&mut self, other: Problems) {
        self.0.extend(other.0);
    }

    fn rust_type(&mut self, context: &str, ty: impl IntoRustType) -> Option<RustType> {
        match ty.into_rust_type() {
            Ok(ty) => Some(ty),
            Err(error) => {
                self.add(format!("{context}: {error}"));
                None
            }
        }
    }

    /// Inserts `value`, recording a problem when `key` was already set: a
    /// second entry would silently discard the first.
    fn insert<V>(&mut self, map: &mut BTreeMap<String, V>, section: &str, key: String, value: V) {
        use std::collections::btree_map::Entry;
        match map.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(value);
            }
            Entry::Occupied(slot) => {
                self.add(format!("{section}: `{}` is set more than once", slot.key()));
            }
        }
    }

    fn finish(self) -> Result<()> {
        match self.0.len() {
            0 => Ok(()),
            1 => Err(Error::config(self.0.into_iter().next().unwrap_or_default())),
            _ => Err(Error::config(format!("\n  - {}", self.0.join("\n  - ")))),
        }
    }
}

impl Config {
    /// Starts building a configuration in code.
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }
}

/// Builds a [`Config`]; see the [module documentation](self).
#[derive(Debug, Default)]
pub struct ConfigBuilder {
    config: Config,
    problems: Problems,
}

impl ConfigBuilder {
    /// How type names are derived when nothing overrides them.
    pub fn type_naming(mut self, naming: TypeNaming) -> Self {
        self.config.type_naming = naming;
        self
    }

    /// Gives every object struct `extensions` and `extras` members.
    pub fn extensible(mut self, extensible: bool) -> Self {
        self.config.extensible = extensible;
        self
    }

    /// A path to `use` at the top of the module, as it follows the keyword:
    /// `"crate::VertexAttribute"`, `"super::cmn::*"`.
    pub fn import(mut self, path: impl Into<String>) -> Self {
        self.config.imports.push(path.into());
        self
    }

    /// An additional root schema file, relative to the schema directory.
    pub fn additional_schema(mut self, path: impl Into<String>) -> Self {
        self.config.additional_schemas.push(path.into());
        self
    }

    /// The Rust type of a property, keyed `"OwnerType.property"` or
    /// `"*.property"`.
    pub fn field_type(mut self, key: impl Into<String>, ty: impl IntoRustType) -> Self {
        let key = key.into();
        if let Some(ty) = self.problems.rust_type(&format!("fieldTypes.{key}"), ty) {
            self.problems
                .insert(&mut self.config.field_types, "fieldTypes", key, ty);
        }
        self
    }

    /// Pins the name of the enum generated from `"OwnerType.property"`.
    pub fn enum_name(mut self, key: impl Into<String>, name: impl Into<String>) -> Self {
        self.problems.insert(
            &mut self.config.enum_names,
            "enumNames",
            key.into(),
            name.into(),
        );
        self
    }

    /// Extra accepted spellings for one value of the enum behind
    /// `"OwnerType.property"` (or `"*.property"`). Written output keeps the
    /// schema's own value.
    pub fn enum_alias<I, S>(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        spellings: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let (key, value) = (key.into(), value.into());
        let section = format!("enumAliases.{key}");
        let spellings: Vec<String> = spellings.into_iter().map(Into::into).collect();
        let values = self.config.enum_aliases.entry(key).or_default();
        self.problems.insert(values, &section, value, spellings);
        self
    }

    /// Overrides for the schema titled `title`.
    pub fn schema(
        mut self,
        title: impl Into<String>,
        configure: impl FnOnce(SchemaBuilder) -> SchemaBuilder,
    ) -> Self {
        let title = title.into();
        let built = configure(SchemaBuilder::new(&title));
        self.problems.absorb(built.problems);
        self.problems
            .insert(&mut self.config.schemas, "schemas", title, built.schema);
        self
    }

    /// An enum no schema describes.
    pub fn custom_enum(
        mut self,
        name: impl Into<String>,
        configure: impl FnOnce(CustomEnumBuilder) -> CustomEnumBuilder,
    ) -> Self {
        let name = name.into();
        let built = configure(CustomEnumBuilder::default());
        self.problems.insert(
            &mut self.config.custom_types,
            "customTypes",
            name,
            built.custom,
        );
        self
    }

    /// A `type` alias no schema describes.
    pub fn custom_alias(
        mut self,
        name: impl Into<String>,
        target: impl IntoRustType,
        doc: impl Into<String>,
    ) -> Self {
        let name = name.into();
        if let Some(target) = self
            .problems
            .rust_type(&format!("customTypes.{name}"), target)
        {
            let custom = CustomTypeConfig {
                kind: CustomKind::Alias,
                variants: Vec::new(),
                target: Some(target),
                doc: Some(doc.into()),
                numeric: false,
                numeric_values: BTreeMap::new(),
                default: None,
                origin: None,
            };
            self.problems
                .insert(&mut self.config.custom_types, "customTypes", name, custom);
        }
        self
    }

    /// Finishes the configuration, validating it exactly as a JSON config is.
    pub fn build(self) -> Result<Config> {
        self.problems.finish()?;
        self.config.validate()?;
        Ok(self.config)
    }
}

/// Overrides for one schema; see [`ConfigBuilder::schema`].
#[derive(Debug)]
pub struct SchemaBuilder {
    title: String,
    schema: SchemaConfig,
    problems: Problems,
}

impl SchemaBuilder {
    fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            schema: SchemaConfig::default(),
            problems: Problems::default(),
        }
    }

    fn context(&self, section: &str, key: &str) -> String {
        format!("schemas.{}.{section}.{key}", self.title)
    }

    /// The Rust type name to use instead of the derived one.
    pub fn override_name(mut self, name: impl Into<String>) -> Self {
        self.schema.override_name = Some(name.into());
        self
    }

    /// Generates no type for this schema. References to it become
    /// `serde_json::Value` unless [`replace_with`](Self::replace_with) says
    /// otherwise.
    pub fn skip(mut self) -> Self {
        self.schema.skip = true;
        self
    }

    /// Generates no type, and resolves references to this schema to `ty`.
    pub fn replace_with(mut self, ty: impl IntoRustType) -> Self {
        let context = format!("schemas.{}.replaceWith", self.title);
        self.schema.replace_with = self.problems.rust_type(&context, ty);
        self
    }

    /// The Rust type of one property.
    pub fn property_type(mut self, property: impl Into<String>, ty: impl IntoRustType) -> Self {
        let property = property.into();
        let context = self.context("propertyTypes", &property);
        if let Some(ty) = self.problems.rust_type(&context, ty) {
            let section = format!("schemas.{}.propertyTypes", self.title);
            self.problems
                .insert(&mut self.schema.property_types, &section, property, ty);
        }
        self
    }

    /// The JSON default of one property, used when the schema has none.
    pub fn property_default(
        mut self,
        property: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        let section = format!("schemas.{}.propertyDefaults", self.title);
        self.problems.insert(
            &mut self.schema.property_defaults,
            &section,
            property.into(),
            value.into(),
        );
        self
    }

    /// A `skip_serializing_if` function path for one property.
    pub fn property_skip_if(
        mut self,
        property: impl Into<String>,
        predicate: impl Into<String>,
    ) -> Self {
        let section = format!("schemas.{}.propertySkipIf", self.title);
        self.problems.insert(
            &mut self.schema.property_skip_if,
            &section,
            property.into(),
            predicate.into(),
        );
        self
    }

    /// Whether one property is required when deserializing, replacing the
    /// schema's `required` list.
    pub fn property_required(mut self, property: impl Into<String>, required: bool) -> Self {
        let section = format!("schemas.{}.propertyRequired", self.title);
        self.problems.insert(
            &mut self.schema.property_required,
            &section,
            property.into(),
            required,
        );
        self
    }

    /// Leaves one property out of the struct.
    pub fn property_skip(mut self, property: impl Into<String>) -> Self {
        self.schema.property_skip.push(property.into());
        self
    }

    /// Adds a member the schema does not describe. `ty` must implement
    /// `Default`.
    pub fn extra_field(
        mut self,
        name: impl Into<String>,
        ty: impl IntoRustType,
        configure: impl FnOnce(ExtraFieldBuilder) -> ExtraFieldBuilder,
    ) -> Self {
        let name = name.into();
        let context = self.context("extraFields", &name);
        if let Some(ty) = self.problems.rust_type(&context, ty) {
            let built = configure(ExtraFieldBuilder {
                field: ExtraFieldConfig {
                    name,
                    ty,
                    doc: None,
                    skip_serde: false,
                },
            });
            self.schema.extra_fields.push(built.field);
        }
        self
    }
}

/// Options of an extra field; see [`SchemaBuilder::extra_field`].
#[derive(Debug)]
pub struct ExtraFieldBuilder {
    field: ExtraFieldConfig,
}

impl ExtraFieldBuilder {
    /// Documentation for the field.
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.field.doc = Some(doc.into());
        self
    }

    /// Excludes the field from (de)serialization.
    pub fn skip_serde(mut self) -> Self {
        self.field.skip_serde = true;
        self
    }
}

/// A custom enum; see [`ConfigBuilder::custom_enum`].
#[derive(Debug)]
pub struct CustomEnumBuilder {
    custom: CustomTypeConfig,
}

impl Default for CustomEnumBuilder {
    fn default() -> Self {
        Self {
            custom: CustomTypeConfig {
                kind: CustomKind::Enum,
                variants: Vec::new(),
                target: None,
                doc: None,
                numeric: false,
                numeric_values: BTreeMap::new(),
                default: None,
                origin: None,
            },
        }
    }
}

impl CustomEnumBuilder {
    /// The variants, as they appear on the wire for a string enum.
    pub fn variants<I, S>(mut self, variants: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.custom.variants = variants.into_iter().map(Into::into).collect();
        self
    }

    /// (De)serializes as integers. Variants take their position unless given
    /// a value by [`value`](Self::value).
    pub fn numeric(mut self) -> Self {
        self.custom.numeric = true;
        self
    }

    /// The integer value of one variant of a numeric enum.
    pub fn value(mut self, variant: impl Into<String>, value: i64) -> Self {
        self.custom.numeric = true;
        self.custom.numeric_values.insert(variant.into(), value);
        self
    }

    /// The variant `Default` returns; the first one when unset.
    pub fn default_variant(mut self, variant: impl Into<String>) -> Self {
        self.custom.default = Some(variant.into());
        self
    }

    /// Documentation for the type.
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.custom.doc = Some(doc.into());
        self
    }

    /// Why the type exists, recorded in the generation manifest.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.custom.origin = Some(origin.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_config_equals_its_json_form() {
        let built = Config::builder()
            .extensible(true)
            .import("super::cmn::*")
            .additional_schema("Subtree/subtree.schema.json")
            .field_type("*.bitstream", "Option<usize>")
            .enum_name("HeaderValue.valueType", "HeaderValueType")
            .enum_alias("*.resourcePattern", "featureData", ["FeatureData"])
            .schema("Tile", |schema| {
                schema
                    .override_name("Tile")
                    .property_type("refine", "Option<Refine>")
                    .property_default("count", 3)
                    .property_skip_if("name", "String::is_empty")
                    .property_required("color", false)
                    .property_skip("legacy")
                    .extra_field("data", "bytes::Bytes", |f| f.doc("Payload.").skip_serde())
            })
            .schema("Id", |schema| schema.replace_with("u32"))
            .custom_enum("Refine", |e| e.variants(["ADD", "REPLACE"]))
            .custom_enum("Kind", |e| {
                e.variants(["A", "B"]).value("A", 512).default_variant("B")
            })
            .custom_alias("Value", "serde_json::Value", "Any JSON.")
            .build()
            .unwrap();
        let json = Config::from_json_str(
            r#"{
                "extensible": true,
                "imports": ["super::cmn::*"],
                "additionalSchemas": ["Subtree/subtree.schema.json"],
                "fieldTypes": {"*.bitstream": "Option<usize>"},
                "enumNames": {"HeaderValue.valueType": "HeaderValueType"},
                "enumAliases": {"*.resourcePattern": {"featureData": ["FeatureData"]}},
                "schemas": {
                    "Tile": {
                        "overrideName": "Tile",
                        "propertyTypes": {"refine": "Option<Refine>"},
                        "propertyDefaults": {"count": 3},
                        "propertySkipIf": {"name": "String::is_empty"},
                        "propertyRequired": {"color": false},
                        "propertySkip": ["legacy"],
                        "extraFields": [{"name": "data", "type": "bytes::Bytes",
                            "doc": "Payload.", "skipSerde": true}]
                    },
                    "Id": {"replaceWith": "u32"}
                },
                "customTypes": {
                    "Refine": {"kind": "enum", "variants": ["ADD", "REPLACE"]},
                    "Kind": {"kind": "enum", "variants": ["A", "B"], "numeric": true,
                        "numericValues": {"A": 512}, "default": "B"},
                    "Value": {"kind": "alias", "target": "serde_json::Value", "doc": "Any JSON."}
                }
            }"#,
        )
        .unwrap();
        assert_eq!(format!("{built:?}"), format!("{json:?}"));
    }

    #[test]
    fn building_runs_the_same_validation_as_json() {
        let error = Config::builder()
            .custom_enum("Empty", |e| e)
            .build()
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least one variant"), "{error}");
    }

    #[test]
    fn problems_are_collected_not_first_wins() {
        let error = Config::builder()
            .field_type("*.a", "&str")
            .field_type("*.b", "u8")
            .field_type("*.b", "u16")
            .schema("Tile", |schema| schema.property_type("x", "Vec<>"))
            .build()
            .unwrap_err()
            .to_string();
        assert!(error.contains("fieldTypes.*.a"), "{error}");
        assert!(error.contains("`*.b` is set more than once"), "{error}");
        assert!(error.contains("schemas.Tile.propertyTypes.x"), "{error}");
    }

    #[test]
    fn a_schema_given_twice_is_an_error() {
        let error = Config::builder()
            .schema("Tile", |c| c.skip())
            .schema("Tile", |c| c.skip())
            .build()
            .unwrap_err()
            .to_string();
        assert!(error.contains("`Tile` is set more than once"), "{error}");
    }

    #[test]
    fn rust_types_may_be_passed_already_built() {
        let config = Config::builder()
            .field_type("*.id", RustType::U32)
            .build()
            .unwrap();
        assert_eq!(config.field_types["*.id"], RustType::U32);
    }
}
