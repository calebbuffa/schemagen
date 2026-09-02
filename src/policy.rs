//! Consumer-defined generation policy.
//!
//! JSON Schema interpretation belongs in `schemagen`; decisions that depend on
//! a consumer's runtime model belong behind this trait.

use crate::ir::SchemaNode;
use crate::types::{RustType, StructDef};
use proc_macro2::TokenStream;

pub trait GenerationPolicy {
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

    fn additional_definitions(&self) -> Vec<TokenStream> {
        Vec::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicy;

impl GenerationPolicy for DefaultPolicy {}
