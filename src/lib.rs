//! # schemagen
//!
//! Generates Rust types from JSON Schema (Draft-07) documents.
//!
//! Generation is a pipeline, one module per stage:
//!
//! 1. [`loader`] and [`graph`] read schema documents into a normalised
//!    [`ir`] and resolve `$ref`s across files.
//! 2. [`resolve`] turns that into a typed [`model`] of structs, enums and
//!    unions, consulting the [`Config`] (data) and a [`GenerationPolicy`]
//!    (rules) for every decision a schema alone cannot make.
//! 3. [`render`] turns the model into Rust source.
//! 4. [`driver`] (the `cli` feature) wraps all of it as a command line.
//!
//! Every stage reports through [`diagnostics`] rather than failing on the
//! first problem, and everything that can fail returns [`Error`].

pub mod config;
pub mod diagnostics;
#[cfg(feature = "cli")]
pub mod driver;
pub mod error;
pub mod graph;
pub mod ir;
pub mod loader;
pub mod model;
pub mod pointer;
pub mod policy;
pub mod render;
pub mod resolve;
pub mod settings;

pub use config::Config;
pub use error::{Error, Result};
pub use graph::{Graph, SchemaId};
pub use model::{
    EnumDef, EnumVariantDef, Generated, RustType, StructDef, UnionDef, UnionVariantDef,
};
pub use policy::{DefaultPolicy, GenerationPolicy, SettingsPolicy};
pub use render::{Module, format_tokens, render_module, render_modules};
pub use resolve::{ConfigUsage, generate};
pub use settings::{FloatWidth, IntegerWidth, MapRepr, StringRepr, TypeSettings};
