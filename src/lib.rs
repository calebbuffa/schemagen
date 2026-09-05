//! # schemagen
//!
//! Generate Rust types from JSON Schema files with configurable
//! customization. The crate is being redesigned as a general-purpose
//! Draft-07 generator; see the module docs for the layered architecture.
//!
//! Layers:
//! - [`pointer`]: RFC 6901 JSON Pointer
//! - [`diagnostics`]: structured error/warning reporting
//! - [`ir`]: normalized `SchemaNode` IR (preserves all Draft-07 fields)
//! - [`loader`]: JSON -> `SchemaNode` conversion
//! - [`graph`]: schema document discovery and `$ref` resolution across files
//! - [`types`]: schema -> Rust type resolution (M3)
//! - [`settings`]: how schema primitives map onto Rust types
//! - [`config`]: data-driven per-schema exceptions (M4)
//! - [`policy`]: consumer-defined generation rules (M4)
//! - [`render`]: syn/quote/prettyplease rendering (M5)
//!
pub mod config;
pub mod diagnostics;
pub mod graph;
pub mod ir;
pub mod loader;
pub mod pointer;
pub mod policy;
pub mod render;
pub mod settings;
pub mod types;

pub use config::Config;
pub use graph::{Graph, SchemaId};
pub use policy::{DefaultPolicy, GenerationPolicy, SettingsPolicy};
pub use render::{render_module, render_modules};
pub use settings::{FloatWidth, IntegerWidth, MapRepr, StringRepr, TypeSettings};
pub use types::{
    EnumDef, EnumVariantDef, RustType, StructDef, UnionDef, UnionVariantDef, generate_types,
    generate_types_from_roots,
};
