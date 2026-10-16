//! The intermediate representation between schema resolution and rendering.
//!
//! Resolution turns JSON Schema into these definitions; rendering turns them
//! into Rust source. Keeping the two apart means a [`GenerationPolicy`] sees
//! and edits a typed model rather than text, and a type is never printed only
//! to be parsed again.
//!
//! [`GenerationPolicy`]: crate::policy::GenerationPolicy

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};

use crate::error::Error;

/// A Rust type as the generator models it.
///
/// Every width and wrapper is its own variant so that nothing is coerced:
/// asking for `u8` yields `u8`, not the nearest type the generator happens to
/// know. Text is accepted through [`FromStr`] (or `serde`, so configuration
/// can name types directly) and rendered through [`ToTokens`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustType {
    Bool,
    String,
    /// An immutable `Box<str>`: exactly sized on the heap and eight bytes
    /// narrower than a `String` once wrapped in an `Option`.
    BoxedStr,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    /// `serde_json::Value`.
    Value,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    /// A heap-indirected `Box<T>`, used to keep rarely populated or large
    /// payloads out of the inline struct layout.
    Boxed(Box<RustType>),
    /// A `Box<[T]>`: a fixed-length heap slice with no spare-capacity field.
    BoxedSlice(Box<RustType>),
    /// A small vector storing up to `N` elements inline, spilling to the heap
    /// beyond that.
    ///
    /// Schema arrays are frequently short and fixed-shape (a three-component
    /// vector, a four-component colour). Storing those inline removes a heap
    /// allocation per value, which matters more than the extra inline bytes
    /// when the array count scales with document size.
    InlineVec(Box<RustType>, usize),
    Array(Box<RustType>, usize),
    /// A `std::collections::HashMap`.
    Map(Box<RustType>, Box<RustType>),
    /// A sorted `Box<[(K, V)]>` with map-like accessors.
    ///
    /// Schema objects with homogeneous values are usually small and read-only
    /// once parsed, where a hash map costs far more inline and heap space than
    /// a contiguous sorted slice.
    SortedMap(Box<RustType>, Box<RustType>),
    /// Any other type, by path: a type generated from a schema (`Tile`) or an
    /// external one (`bytes::Bytes`). Never generic.
    Named(String),
}

/// The largest inline capacity an `InlineVec` supports: its length is a `u8`,
/// and a capacity near that bound has stopped being an optimisation anyway.
pub const MAX_INLINE_CAPACITY: usize = u8::MAX as usize;

impl RustType {
    /// Parses `text` as a Rust type.
    pub fn parse(text: &str) -> Result<Self, Error> {
        text.parse()
    }

    /// Whether the type represents absence as emptiness, so that wrapping it
    /// in an `Option` would add a second way to say the same thing.
    pub fn is_collection(&self) -> bool {
        matches!(
            self,
            RustType::Vec(_)
                | RustType::BoxedSlice(_)
                | RustType::InlineVec(_, _)
                | RustType::Array(_, _)
                | RustType::Map(_, _)
                | RustType::SortedMap(_, _)
        )
    }

    /// Whether an empty value should be omitted from serialized output.
    /// Fixed-length arrays are never empty in that sense.
    pub fn omits_when_empty(&self) -> bool {
        self.is_collection() && !matches!(self, RustType::Array(_, _))
    }

    /// Visits this type and every type nested inside it.
    pub fn walk(&self, visit: &mut dyn FnMut(&RustType)) {
        visit(self);
        match self {
            RustType::Option(inner)
            | RustType::Vec(inner)
            | RustType::Boxed(inner)
            | RustType::BoxedSlice(inner)
            | RustType::InlineVec(inner, _)
            | RustType::Array(inner, _) => inner.walk(visit),
            RustType::Map(key, value) | RustType::SortedMap(key, value) => {
                key.walk(visit);
                value.walk(visit);
            }
            _ => {}
        }
    }

    /// Rewrites `Named` types according to `renames`, recursing through
    /// wrappers so nested occurrences are updated too.
    pub fn rename_named(&mut self, renames: &HashMap<String, String>) {
        match self {
            RustType::Named(name) => {
                if let Some(replacement) = renames.get(name.as_str()) {
                    name.clone_from(replacement);
                }
            }
            RustType::Option(inner)
            | RustType::Vec(inner)
            | RustType::Boxed(inner)
            | RustType::BoxedSlice(inner)
            | RustType::InlineVec(inner, _)
            | RustType::Array(inner, _) => inner.rename_named(renames),
            RustType::Map(key, value) | RustType::SortedMap(key, value) => {
                key.rename_named(renames);
                value.rename_named(renames);
            }
            _ => {}
        }
    }

    /// Confirms every `Named` path inside is syntactically a path, so that
    /// rendering cannot fail on it later.
    pub fn validate(&self) -> Result<(), Error> {
        let mut failure = None;
        self.walk(&mut |ty| match ty {
            RustType::Named(name) if syn::parse_str::<syn::Path>(name).is_err() => {
                failure.get_or_insert_with(|| invalid_type(name, "not a valid type path"));
            }
            RustType::InlineVec(_, capacity)
                if *capacity == 0 || *capacity > MAX_INLINE_CAPACITY =>
            {
                failure.get_or_insert_with(|| {
                    invalid_type(
                        &ty.to_string(),
                        format!("inline capacity must be between 1 and {MAX_INLINE_CAPACITY}"),
                    )
                });
            }
            _ => {}
        });
        failure.map_or(Ok(()), Err)
    }
}

fn invalid_type(input: &str, reason: impl Into<String>) -> Error {
    Error::InvalidType {
        input: input.to_string(),
        reason: reason.into(),
    }
}

impl fmt::Display for RustType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("bool"),
            Self::String => f.write_str("String"),
            Self::BoxedStr => f.write_str("Box<str>"),
            Self::I8 => f.write_str("i8"),
            Self::I16 => f.write_str("i16"),
            Self::I32 => f.write_str("i32"),
            Self::I64 => f.write_str("i64"),
            Self::U8 => f.write_str("u8"),
            Self::U16 => f.write_str("u16"),
            Self::U32 => f.write_str("u32"),
            Self::U64 => f.write_str("u64"),
            Self::Usize => f.write_str("usize"),
            Self::F32 => f.write_str("f32"),
            Self::F64 => f.write_str("f64"),
            Self::Value => f.write_str("serde_json::Value"),
            Self::Option(inner) => write!(f, "Option<{inner}>"),
            Self::Vec(inner) => write!(f, "Vec<{inner}>"),
            Self::Boxed(inner) => write!(f, "Box<{inner}>"),
            Self::BoxedSlice(inner) => write!(f, "Box<[{inner}]>"),
            Self::InlineVec(inner, capacity) => write!(f, "InlineVec<{inner}, {capacity}>"),
            Self::Array(inner, len) => write!(f, "[{inner}; {len}]"),
            Self::Map(key, value) => write!(f, "std::collections::HashMap<{key}, {value}>"),
            Self::SortedMap(key, value) => write!(f, "SortedMap<{key}, {value}>"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

impl ToTokens for RustType {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let rendered = match self {
            Self::Bool => quote!(bool),
            Self::String => quote!(String),
            Self::BoxedStr => quote!(Box<str>),
            Self::I8 => quote!(i8),
            Self::I16 => quote!(i16),
            Self::I32 => quote!(i32),
            Self::I64 => quote!(i64),
            Self::U8 => quote!(u8),
            Self::U16 => quote!(u16),
            Self::U32 => quote!(u32),
            Self::U64 => quote!(u64),
            Self::Usize => quote!(usize),
            Self::F32 => quote!(f32),
            Self::F64 => quote!(f64),
            Self::Value => quote!(serde_json::Value),
            Self::Option(inner) => quote!(Option<#inner>),
            Self::Vec(inner) => quote!(Vec<#inner>),
            Self::Boxed(inner) => quote!(Box<#inner>),
            Self::BoxedSlice(inner) => quote!(Box<[#inner]>),
            Self::InlineVec(inner, capacity) => {
                let capacity = proc_macro2::Literal::usize_unsuffixed(*capacity);
                quote!(InlineVec<#inner, #capacity>)
            }
            Self::Array(inner, len) => {
                let len = proc_macro2::Literal::usize_unsuffixed(*len);
                quote!([#inner; #len])
            }
            Self::Map(key, value) => quote!(std::collections::HashMap<#key, #value>),
            Self::SortedMap(key, value) => quote!(SortedMap<#key, #value>),
            Self::Named(name) => match syn::parse_str::<syn::Path>(name) {
                Ok(path) => path.into_token_stream(),
                // `validate` rejects this before rendering; surfacing it as a
                // compile error keeps `to_tokens` total.
                Err(error) => error.to_compile_error(),
            },
        };
        tokens.extend(rendered);
    }
}

impl FromStr for RustType {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let parsed = syn::parse_str::<syn::Type>(text)
            .map_err(|error| invalid_type(text, error.to_string()))?;
        let ty = from_syn(&parsed, text)?;
        ty.validate()?;
        Ok(ty)
    }
}

impl<'de> serde::Deserialize<'de> for RustType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

fn from_syn(ty: &syn::Type, input: &str) -> Result<RustType, Error> {
    match ty {
        syn::Type::Paren(paren) => from_syn(&paren.elem, input),
        syn::Type::Group(group) => from_syn(&group.elem, input),
        syn::Type::Array(array) => {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(len),
                ..
            }) = &array.len
            else {
                return Err(invalid_type(
                    input,
                    "array length must be an integer literal",
                ));
            };
            let len = len
                .base10_parse::<usize>()
                .map_err(|error| invalid_type(input, error.to_string()))?;
            Ok(RustType::Array(
                Box::new(from_syn(&array.elem, input)?),
                len,
            ))
        }
        syn::Type::Path(path) if path.qself.is_none() => from_path(&path.path, input),
        other => Err(invalid_type(
            input,
            format!(
                "unsupported type form `{}`",
                other.to_token_stream().to_string().replace(' ', "")
            ),
        )),
    }
}

fn from_path(path: &syn::Path, input: &str) -> Result<RustType, Error> {
    let last = path
        .segments
        .last()
        .ok_or_else(|| invalid_type(input, "empty path"))?;
    let name = last.ident.to_string();
    let prefix: Vec<String> = path
        .segments
        .iter()
        .take(path.segments.len() - 1)
        .map(|segment| segment.ident.to_string())
        .collect();
    let in_std = |allowed: &[&str]| {
        prefix.is_empty()
            || (matches!(prefix[0].as_str(), "std" | "alloc" | "core")
                && prefix[1..]
                    .iter()
                    .map(String::as_str)
                    .eq(allowed.iter().copied()))
    };
    let types = |arguments: &syn::PathArguments| -> Result<Vec<RustType>, Error> {
        let syn::PathArguments::AngleBracketed(arguments) = arguments else {
            return Err(invalid_type(
                input,
                format!("`{name}` needs type arguments"),
            ));
        };
        arguments
            .args
            .iter()
            .map(|argument| match argument {
                syn::GenericArgument::Type(ty) => from_syn(ty, input),
                _ => Err(invalid_type(input, "unsupported generic argument")),
            })
            .collect()
    };
    let arity = |found: &[RustType], expected: usize| {
        if found.len() == expected {
            Ok(())
        } else {
            Err(invalid_type(
                input,
                format!("`{name}` takes {expected} type argument(s)"),
            ))
        }
    };
    let has_arguments = !matches!(last.arguments, syn::PathArguments::None);

    if !has_arguments && prefix.is_empty() {
        let primitive = match name.as_str() {
            "bool" => Some(RustType::Bool),
            "String" => Some(RustType::String),
            "i8" => Some(RustType::I8),
            "i16" => Some(RustType::I16),
            "i32" => Some(RustType::I32),
            "i64" => Some(RustType::I64),
            "u8" => Some(RustType::U8),
            "u16" => Some(RustType::U16),
            "u32" => Some(RustType::U32),
            "u64" => Some(RustType::U64),
            "usize" => Some(RustType::Usize),
            "f32" => Some(RustType::F32),
            "f64" => Some(RustType::F64),
            _ => None,
        };
        if let Some(primitive) = primitive {
            return Ok(primitive);
        }
    }
    if !has_arguments {
        if name == "Value" && prefix == ["serde_json"] {
            return Ok(RustType::Value);
        }
        if name == "String" && in_std(&["string"]) {
            return Ok(RustType::String);
        }
        return Ok(RustType::Named(path_text(path)));
    }

    let args = types(&last.arguments);
    let boxed = |inner: RustType| Box::new(inner);
    match name.as_str() {
        "Option" if in_std(&["option"]) => {
            let args = args?;
            arity(&args, 1)?;
            Ok(RustType::Option(boxed(args.into_iter().next().unwrap())))
        }
        "Vec" if in_std(&["vec"]) => {
            let args = args?;
            arity(&args, 1)?;
            Ok(RustType::Vec(boxed(args.into_iter().next().unwrap())))
        }
        "Box" if in_std(&["boxed"]) => {
            let syn::PathArguments::AngleBracketed(arguments) = &last.arguments else {
                return Err(invalid_type(input, "`Box` needs a type argument"));
            };
            let [syn::GenericArgument::Type(inner)] =
                arguments.args.iter().collect::<Vec<_>>().as_slice()
            else {
                return Err(invalid_type(input, "`Box` takes one type argument"));
            };
            match inner {
                syn::Type::Slice(slice) => {
                    Ok(RustType::BoxedSlice(boxed(from_syn(&slice.elem, input)?)))
                }
                syn::Type::Path(path) if path.path.is_ident("str") => Ok(RustType::BoxedStr),
                inner => Ok(RustType::Boxed(boxed(from_syn(inner, input)?))),
            }
        }
        "HashMap"
            if in_std(&["collections"])
                || in_std(&["collections", "hash_map"])
                || prefix == ["collections"] =>
        {
            let args = args?;
            arity(&args, 2)?;
            let mut args = args.into_iter();
            Ok(RustType::Map(
                boxed(args.next().unwrap()),
                boxed(args.next().unwrap()),
            ))
        }
        "SortedMap" if prefix.is_empty() => {
            let args = args?;
            arity(&args, 2)?;
            let mut args = args.into_iter();
            Ok(RustType::SortedMap(
                boxed(args.next().unwrap()),
                boxed(args.next().unwrap()),
            ))
        }
        "InlineVec" if prefix.is_empty() => {
            let syn::PathArguments::AngleBracketed(arguments) = &last.arguments else {
                return Err(invalid_type(input, "`InlineVec` needs arguments"));
            };
            let collected = arguments.args.iter().collect::<Vec<_>>();
            let [syn::GenericArgument::Type(element), capacity] = collected.as_slice() else {
                return Err(invalid_type(
                    input,
                    "`InlineVec` takes an element type and a capacity",
                ));
            };
            let capacity = match capacity {
                syn::GenericArgument::Const(syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(value),
                    ..
                })) => value.base10_parse::<usize>(),
                // A literal capacity always parses as a const argument.
                _ => {
                    return Err(invalid_type(
                        input,
                        "inline capacity must be an integer literal",
                    ));
                }
            }
            .map_err(|error| invalid_type(input, error.to_string()))?;
            Ok(RustType::InlineVec(
                boxed(from_syn(element, input)?),
                capacity,
            ))
        }
        _ => Err(invalid_type(
            input,
            format!("generic type `{name}` is not supported; define a non-generic alias"),
        )),
    }
}

fn path_text(path: &syn::Path) -> String {
    let mut text = String::new();
    if path.leading_colon.is_some() {
        text.push_str("::");
    }
    text.push_str(
        &path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
    );
    text
}

/// A struct, or an alias for another type, generated from one schema.
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub title: String,
    /// File name of the schema this definition came from, without extension.
    ///
    /// Conventions that encode meaning in the file name (such as glTF's
    /// `mesh.primitive.KHR_foo.schema.json`) are otherwise unrecoverable from
    /// the schema body alone.
    pub source_file: String,
    pub description: Option<String>,
    pub is_object: bool,
    pub alias: Option<RustType>,
    pub fields: Vec<FieldDef>,
    pub extra_fields: Vec<ExtraFieldDef>,
    /// Adds `extensions` and `extras` members.
    pub extensions: bool,
    pub enums: Vec<EnumDef>,
    pub unions: Vec<UnionDef>,
}

/// One property of a generated struct.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub json_name: String,
    pub rust_name: String,
    pub ty: RustType,
    pub required: bool,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
    pub skip_serializing_if: Option<String>,
    pub flatten: bool,
}

/// A member the schema does not describe, added from configuration or by a
/// policy. Typically runtime state such as a decoded payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraFieldDef {
    pub name: String,
    pub ty: RustType,
    /// Excluded from (de)serialization entirely.
    pub skip_serde: bool,
    pub description: Option<String>,
}

/// A generated enum discovered from a finite schema value set.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub description: Option<String>,
    pub variants: Vec<EnumVariantDef>,
    /// When true the variants carry explicit numeric discriminants and are
    /// (de)serialized as numbers rather than strings.
    pub numeric: bool,
    /// When true the schema permits values outside the listed set, so the type
    /// is rendered as an open form. Unknown values round-trip losslessly
    /// instead of erroring.
    pub open: bool,
    /// The JSON property this enum was derived from, used to shorten the
    /// generated name when it is unambiguous across the whole output.
    pub property: String,
    /// The variant `Default` returns; the first one when `None`.
    pub default: Option<String>,
}

/// One value of an [`EnumDef`].
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariantDef {
    pub name: String,
    pub value: serde_json::Value,
    /// Additional string spellings accepted on input.
    pub aliases: Vec<String>,
}

/// An untagged enum generated from a general `oneOf`/`anyOf`.
#[derive(Debug, Clone)]
pub struct UnionDef {
    pub name: String,
    pub description: Option<String>,
    pub variants: Vec<UnionVariantDef>,
}

/// One branch of a [`UnionDef`].
#[derive(Debug, Clone)]
pub struct UnionVariantDef {
    pub name: String,
    pub ty: RustType,
}

/// A `type` alias that no schema describes, declared in configuration.
#[derive(Debug, Clone)]
pub struct AliasDef {
    pub name: String,
    pub target: RustType,
    pub description: Option<String>,
}

/// Everything one generation run produces, ready to render.
#[derive(Debug, Clone, Default)]
pub struct Generated {
    /// One definition per generated schema, sorted by name.
    pub definitions: Vec<StructDef>,
    /// Enums declared under `customTypes`.
    pub custom_enums: Vec<EnumDef>,
    /// Aliases declared under `customTypes`.
    pub aliases: Vec<AliasDef>,
    /// `(type name, reason)` for each custom type, for the manifest.
    pub custom_origins: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> RustType {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn integer_widths_are_never_coerced() {
        for (text, expected) in [
            ("u8", RustType::U8),
            ("u16", RustType::U16),
            ("u32", RustType::U32),
            ("u64", RustType::U64),
            ("usize", RustType::Usize),
            ("i8", RustType::I8),
            ("i16", RustType::I16),
            ("i32", RustType::I32),
            ("i64", RustType::I64),
            ("f32", RustType::F32),
            ("f64", RustType::F64),
        ] {
            assert_eq!(parse(text), expected);
            assert_eq!(expected.to_string(), text);
        }
    }

    #[test]
    fn wrappers_nest() {
        assert_eq!(
            parse("Option<Box<[f64]>>"),
            RustType::Option(Box::new(RustType::BoxedSlice(Box::new(RustType::F64))))
        );
        assert_eq!(parse("Box<str>"), RustType::BoxedStr);
        assert_eq!(
            parse("[f64; 16]"),
            RustType::Array(Box::new(RustType::F64), 16)
        );
        assert_eq!(
            parse("InlineVec<u32, 4>"),
            RustType::InlineVec(Box::new(RustType::U32), 4)
        );
        assert_eq!(
            parse("SortedMap<VertexAttribute, u32>"),
            RustType::SortedMap(
                Box::new(RustType::Named("VertexAttribute".into())),
                Box::new(RustType::U32)
            )
        );
    }

    #[test]
    fn std_paths_and_whitespace_are_accepted() {
        assert_eq!(
            parse("std::collections::HashMap< String , serde_json::Value >"),
            RustType::Map(Box::new(RustType::String), Box::new(RustType::Value))
        );
        assert_eq!(
            parse("std::option::Option<std::string::String>"),
            RustType::Option(Box::new(RustType::String))
        );
    }

    #[test]
    fn external_paths_are_kept_whole() {
        assert_eq!(
            parse("bytes::Bytes"),
            RustType::Named("bytes::Bytes".into())
        );
        assert_eq!(
            parse("crate::VertexAttribute"),
            RustType::Named("crate::VertexAttribute".into())
        );
    }

    #[test]
    fn unsupported_forms_are_errors_not_guesses() {
        for text in [
            "&str",
            "(u8, u8)",
            "Cow<str>",
            "Vec<>",
            "InlineVec<u8, 0>",
            "InlineVec<u8, 300>",
            "[u8; N]",
            "fn()",
            "not a type",
        ] {
            assert!(text.parse::<RustType>().is_err(), "{text} must be rejected");
        }
    }

    #[test]
    fn tokens_round_trip_through_text() {
        for text in [
            "Option<Box<[f64]>>",
            "[f64; 12]",
            "SortedMap<String, Vec<u32>>",
            "InlineVec<f32, 3>",
            "std::collections::HashMap<String, serde_json::Value>",
            "bytes::Bytes",
        ] {
            let ty = parse(text);
            let reparsed: syn::Type = syn::parse2(ty.to_token_stream()).unwrap();
            assert_eq!(
                from_syn(&reparsed, text).unwrap(),
                ty,
                "{text} must survive rendering"
            );
        }
    }

    #[test]
    fn collections_are_recognised() {
        assert!(parse("Vec<u8>").is_collection());
        assert!(parse("[u8; 3]").is_collection());
        assert!(!parse("[u8; 3]").omits_when_empty());
        assert!(parse("Box<[u8]>").omits_when_empty());
        assert!(!parse("Option<Vec<u8>>").is_collection());
    }

    #[test]
    fn deserializes_from_a_string() {
        let ty: RustType = serde_json::from_str("\"Option<u16>\"").unwrap();
        assert_eq!(ty, RustType::Option(Box::new(RustType::U16)));
        assert!(serde_json::from_str::<RustType>("\"&str\"").is_err());
    }
}
