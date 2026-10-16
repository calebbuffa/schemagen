//! Consumer-defined generation policy.
//!
//! JSON Schema interpretation belongs in `schemagen`; decisions that depend on
//! a consumer's runtime model belong behind this trait.
//!
//! # Policy or config?
//!
//! Two mechanisms customise generation, and the split is deliberate:
//!
//! - **This trait** carries every decision that is a *rule*: how a schema
//!   title becomes a Rust name, which schemas belong in this module, how
//!   primitives map onto Rust types. Rules are code, so they are type-checked,
//!   unit-testable, and can consult the [`SchemaNode`] they are deciding about.
//! - [`Config`] carries the residue that is genuinely *data*: a table of
//!   per-schema exceptions that no rule predicts, such as a single schema
//!   whose title must be spelled differently or a property whose type the
//!   schema understates.
//!
//! A knob whose legal values are a fixed set belongs here, not in config: as
//! an enum the compiler rejects a mistake.
//!
//! Every method has a default, so the minimum implementation is an empty
//! `impl GenerationPolicy for MyPolicy {}`. The trait is object safe.
//!
//! Hooks run after the config has been consulted for the same decision only
//! where noted; for types, a policy hook always has the final say over
//! the config, because code is the more specific statement of intent.
//!
//! [`Config`]: crate::config::Config

use proc_macro2::TokenStream;

use crate::ir::SchemaNode;
use crate::model::{RustType, StructDef};
use crate::settings::TypeSettings;

pub trait GenerationPolicy {
    /// How schema primitives map onto Rust types.
    ///
    /// Defaults to [`TypeSettings::default`], which mirrors JSON's own value
    /// model. Override to trade JSON fidelity for a more compact or more
    /// precisely typed representation.
    fn settings(&self) -> TypeSettings {
        TypeSettings::default()
    }

    /// Overrides the Rust type of one property.
    fn field_type(
        &self,
        _owner: &SchemaNode,
        _field: &str,
        _schema: &SchemaNode,
    ) -> Option<RustType> {
        None
    }

    /// Whether to leave one property out of its struct.
    fn skip_field(&self, _owner: &SchemaNode, _field: &str, _schema: &SchemaNode) -> bool {
        false
    }

    /// Overrides whether a property is required. `None` defers to the
    /// config, then to the schema's `required` list.
    fn field_required(
        &self,
        _owner: &SchemaNode,
        _field: &str,
        _schema: &SchemaNode,
    ) -> Option<bool> {
        None
    }

    /// A `skip_serializing_if` function path for one property.
    fn skip_serializing_if(
        &self,
        _owner: &SchemaNode,
        _field: &str,
        _schema: &SchemaNode,
    ) -> Option<String> {
        None
    }

    /// Whether a titled schema generates a type in this module.
    fn should_generate(&self, _title: &str, _schema: &SchemaNode) -> bool {
        true
    }

    /// Names a type, taking precedence over `overrideName` and the configured
    /// naming scheme.
    fn type_name(&self, _title: &str, _schema: &SchemaNode) -> Option<String> {
        None
    }

    /// Extra spellings accepted for the values of an enum property, by
    /// schema value. Merged with the config's `enumAliases`.
    fn variant_aliases(&self, _owner: &str, _property: &str, _value: &str) -> Vec<String> {
        Vec::new()
    }

    /// Adjusts a struct before it is rendered.
    fn augment_struct(&self, _definition: &mut StructDef) {}

    /// Extra items to emit alongside a generated struct, such as trait impls
    /// keyed off the struct's schema source.
    fn struct_items(&self, _definition: &StructDef) -> Vec<TokenStream> {
        Vec::new()
    }

    /// Items to emit once per module.
    fn additional_definitions(&self) -> Vec<TokenStream> {
        Vec::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicy;

impl GenerationPolicy for DefaultPolicy {}

/// A policy that supplies [`TypeSettings`] and nothing else.
///
/// Choosing how primitives map onto Rust types is the one customisation
/// almost every consumer needs, and often the only one.
///
/// ```
/// use schemagen::{SettingsPolicy, TypeSettings};
///
/// let policy = SettingsPolicy::new(TypeSettings::compact());
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct SettingsPolicy {
    settings: TypeSettings,
}

impl SettingsPolicy {
    pub fn new(settings: TypeSettings) -> Self {
        Self { settings }
    }
}

impl GenerationPolicy for SettingsPolicy {
    fn settings(&self) -> TypeSettings {
        self.settings
    }
}
