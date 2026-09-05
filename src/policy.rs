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
//!   whose title must be spelled differently.
//!
//! A knob whose legal values are a fixed set belongs here, not in config: as
//! an enum the compiler rejects a mistake, whereas in JSON a typo deserialises
//! to the default and silently does nothing.
//!
//! Every method has a default, so the minimum implementation is an empty
//! `impl GenerationPolicy for MyPolicy {}`.
//!
//! [`Config`]: crate::config::Config

use crate::ir::SchemaNode;
use crate::settings::TypeSettings;
use crate::types::{RustType, StructDef};
use proc_macro2::TokenStream;

pub trait GenerationPolicy {
    /// How schema primitives map onto Rust types.
    ///
    /// Defaults to [`TypeSettings::default`], which mirrors JSON's own value
    /// model. Override to trade JSON fidelity for a more compact or more
    /// precisely typed representation.
    fn settings(&self) -> TypeSettings {
        TypeSettings::default()
    }

    fn allow_lossy(&self) -> bool {
        false
    }

    fn field_type(
        &self,
        _owner: &SchemaNode,
        _field: &str,
        _schema: &SchemaNode,
    ) -> Option<RustType> {
        None
    }

    fn skip_field(&self, _owner: &SchemaNode, _field: &str, _schema: &SchemaNode) -> bool {
        false
    }

    fn reference_type(&self, _title: &str, _schema: &SchemaNode) -> Option<RustType> {
        None
    }

    fn skip_serializing_if(
        &self,
        _owner: &SchemaNode,
        _field: &str,
        _schema: &SchemaNode,
    ) -> Option<String> {
        None
    }

    fn should_generate(&self, _title: &str, _schema: &SchemaNode) -> bool {
        true
    }

    fn type_name(&self, _title: &str, _schema: &SchemaNode) -> Option<String> {
        None
    }

    fn augment_struct(&self, _definition: &mut StructDef) {}

    /// Extra items to emit alongside a generated struct, such as trait impls
    /// keyed off the struct's schema source.
    fn struct_items(&self, _definition: &StructDef) -> Vec<TokenStream> {
        Vec::new()
    }

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
/// almost every consumer needs, and often the only one. This spares them
/// writing a unit struct and a one-method impl to express it.
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
