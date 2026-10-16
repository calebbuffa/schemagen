//! Rendering enums: closed and open, string and numeric.

use heck::ToShoutySnakeCase;
use proc_macro2::{Literal, TokenStream};
use quote::quote;
use serde_json::Value;

use super::{doc, ident, string_literal};
use crate::error::{Error, Result};
use crate::model::{EnumDef, EnumVariantDef, UnionDef};

pub(super) fn render_enum(definition: &EnumDef) -> Result<TokenStream> {
    if definition.variants.is_empty() {
        return Err(Error::render(format!(
            "enum {} has no variants",
            definition.name
        )));
    }
    let name = ident(&definition.name, "an enum name")?;
    let docs = doc(definition.description.as_deref());
    let default = default_variant(definition)?;
    match (definition.open, definition.numeric) {
        (true, true) => render_open_numeric(definition, &name, &docs, default),
        (true, false) => render_open_string(definition, &name, &docs, default),
        (false, true) => render_closed_numeric(definition, &name, &docs, default),
        (false, false) => render_closed_string(definition, &name, &docs, default),
    }
}

fn default_variant(definition: &EnumDef) -> Result<&EnumVariantDef> {
    match &definition.default {
        None => Ok(&definition.variants[0]),
        Some(wanted) => definition
            .variants
            .iter()
            .find(|variant| &variant.name == wanted)
            .ok_or_else(|| {
                Error::render(format!(
                    "enum {} defaults to `{wanted}`, which is not one of its variants",
                    definition.name
                ))
            }),
    }
}

fn integer(definition: &EnumDef, variant: &EnumVariantDef) -> Result<i64> {
    variant.value.as_i64().ok_or_else(|| {
        Error::render(format!(
            "enum {} variant {} has the non-integer value {}",
            definition.name, variant.name, variant.value
        ))
    })
}

fn text<'a>(definition: &EnumDef, variant: &'a EnumVariantDef) -> Result<&'a str> {
    match &variant.value {
        Value::String(text) => Ok(text),
        other => Err(Error::render(format!(
            "enum {} variant {} has the non-string value {other}",
            definition.name, variant.name
        ))),
    }
}

/// A string-valued enum whose values are exactly the schema's.
fn render_closed_string(
    definition: &EnumDef,
    name: &syn::Ident,
    docs: &TokenStream,
    default: &EnumVariantDef,
) -> Result<TokenStream> {
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let ident = ident(&variant.name, "a variant name")?;
            let value = string_literal(text(definition, variant)?);
            let aliases = variant.aliases.iter().map(|alias| {
                let alias = string_literal(alias);
                quote!(alias = #alias,)
            });
            Ok(quote!(#[serde(rename = #value, #(#aliases)*)] #ident))
        })
        .collect::<Result<Vec<_>>>()?;
    let default = ident(&default.name, "a variant name")?;
    Ok(quote!(
        #docs
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default }
        }
    ))
}

/// A numeric enum with explicit discriminants and an integer serde bridge;
/// deriving `Serialize`/`Deserialize` directly would write variant *names*.
fn render_closed_numeric(
    definition: &EnumDef,
    name: &syn::Ident,
    docs: &TokenStream,
    default: &EnumVariantDef,
) -> Result<TokenStream> {
    let mut declarations = Vec::new();
    let mut to_value = Vec::new();
    let mut from_value = Vec::new();
    for variant in &definition.variants {
        let ident = ident(&variant.name, "a variant name")?;
        let value = Literal::i64_unsuffixed(integer(definition, variant)?);
        declarations.push(quote!(#ident = #value));
        to_value.push(quote!(Self::#ident => #value));
        from_value.push(quote!(#value => Ok(Self::#ident)));
    }
    let default = ident(&default.name, "a variant name")?;
    let expected = format!("a valid {} value", definition.name);
    Ok(quote!(
        #docs
        #[repr(i64)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum #name { #(#declarations),* }
        impl #name {
            /// Returns the underlying schema value.
            pub fn value(self) -> i64 {
                match self { #(#to_value),* }
            }
            /// Converts a raw schema value into this enum.
            pub fn from_value(value: i64) -> Result<Self, i64> {
                match value { #(#from_value,)* other => Err(other) }
            }
        }
        impl Default for #name {
            fn default() -> Self { Self::#default }
        }
        impl Serialize for #name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_i64(self.value())
            }
        }
        impl<'de> Deserialize<'de> for #name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = i64::deserialize(d)?;
                Self::from_value(value).map_err(|value| {
                    serde::de::Error::invalid_value(
                        serde::de::Unexpected::Signed(value),
                        &#expected,
                    )
                })
            }
        }
    ))
}

const OPEN_NOTE: &str = "\n\nThe schema permits values beyond those listed, so unrecognised \
                         values are preserved rather than rejected.";

fn open_doc(definition: &EnumDef) -> TokenStream {
    let base = definition.description.as_deref().unwrap_or_default();
    doc(Some(&format!("{base}{OPEN_NOTE}")))
}

/// An open numeric set: a transparent newtype over `i64` with associated
/// constants. It is the size of the raw primitive and makes serde a pure
/// passthrough with no match table.
fn render_open_numeric(
    definition: &EnumDef,
    name: &syn::Ident,
    _docs: &TokenStream,
    default: &EnumVariantDef,
) -> Result<TokenStream> {
    let docs = open_doc(definition);
    let mut constants = Vec::new();
    let mut names = Vec::new();
    for variant in &definition.variants {
        let constant = ident(&variant.name.to_shouty_snake_case(), "a constant name")?;
        let value = Literal::i64_unsuffixed(integer(definition, variant)?);
        let note = doc(Some(&format!("Schema value `{}`.", variant.value)));
        constants.push(quote!(#note pub const #constant: Self = Self(#value);));
        names.push(constant);
    }
    let default = ident(&default.name.to_shouty_snake_case(), "a constant name")?;
    Ok(quote!(
        #docs
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct #name(pub i64);

        impl #name {
            #(#constants)*

            /// Returns true when the value is one of the schema's named constants.
            pub fn is_known(&self) -> bool {
                [#(Self::#names),*].iter().any(|known| known == self)
            }
        }

        impl Default for #name {
            fn default() -> Self { Self::#default }
        }
    ))
}

/// An open string set: an enum whose known variants are fieldless and whose
/// `Other` variant boxes the unrecognised string. Known values cost no
/// allocation and the type stays 16 bytes, where a `Cow`-based newtype would
/// be 24 and allocate for every value parsed.
fn render_open_string(
    definition: &EnumDef,
    name: &syn::Ident,
    _docs: &TokenStream,
    default: &EnumVariantDef,
) -> Result<TokenStream> {
    let docs = open_doc(definition);
    let mut idents = Vec::new();
    let mut values = Vec::new();
    let mut spellings = Vec::new();
    for variant in &definition.variants {
        let ident = ident(&variant.name, "a variant name")?;
        let value = string_literal(text(definition, variant)?);
        let accepted = std::iter::once(value.clone())
            .chain(variant.aliases.iter().map(|alias| string_literal(alias)))
            .collect::<Vec<_>>();
        idents.push(ident);
        values.push(value);
        spellings.push(accepted);
    }
    let default = ident(&default.name, "a variant name")?;
    let from_arms = idents
        .iter()
        .zip(&spellings)
        .map(|(ident, accepted)| quote!(#(#accepted)|* => Self::#ident,));
    let visit_arms = idents
        .iter()
        .zip(&spellings)
        .map(|(ident, accepted)| quote!(#(#accepted)|* => #name::#ident,));
    Ok(quote!(
        #docs
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum #name {
            #(
                #[doc = #values]
                #idents,
            )*
            /// A value not listed in the schema, preserved verbatim.
            ///
            /// The payload is boxed so the enum stays pointer-sized rather
            /// than paying the full `String` width in every value.
            Other(Box<String>),
        }

        impl #name {
            /// Returns the value's schema representation.
            pub fn as_str(&self) -> &str {
                match self {
                    #(Self::#idents => #values,)*
                    Self::Other(value) => value,
                }
            }

            /// Returns true when the value is one of the schema's named variants.
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Other(_))
            }
        }

        impl From<&str> for #name {
            fn from(value: &str) -> Self {
                match value {
                    #(#from_arms)*
                    other => Self::Other(Box::new(other.to_owned())),
                }
            }
        }

        impl Default for #name {
            fn default() -> Self { Self::#default }
        }

        impl Serialize for #name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for #name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl serde::de::Visitor<'_> for Visitor {
                    type Value = #name;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a string")
                    }
                    // Borrowed input avoids allocating for known values.
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<#name, E> {
                        Ok(#name::from(value))
                    }
                    // Owned input reuses the allocation for unknown values.
                    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<#name, E> {
                        Ok(match value.as_str() {
                            #(#visit_arms)*
                            _ => #name::Other(Box::new(value)),
                        })
                    }
                }
                d.deserialize_str(Visitor)
            }
        }
    ))
}

/// An untagged enum for a general `oneOf`/`anyOf`.
pub(super) fn render_union(definition: &UnionDef) -> Result<TokenStream> {
    let name = ident(&definition.name, "a union name")?;
    let first = definition
        .variants
        .first()
        .ok_or_else(|| Error::render(format!("union {} has no variants", definition.name)))?;
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let ident = ident(&variant.name, "a union variant name")?;
            let ty = &variant.ty;
            Ok(quote!(#ident(#ty)))
        })
        .collect::<Result<Vec<_>>>()?;
    let default = ident(&first.name, "a union variant name")?;
    let docs = doc(definition.description.as_deref());
    Ok(quote!(
        #docs
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(untagged)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default(Default::default()) }
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(name: &str, value: Value, aliases: &[&str]) -> EnumVariantDef {
        EnumVariantDef {
            name: name.into(),
            value,
            aliases: aliases.iter().map(ToString::to_string).collect(),
        }
    }

    fn definition(open: bool, numeric: bool, variants: Vec<EnumVariantDef>) -> EnumDef {
        EnumDef {
            name: "Mode".into(),
            description: Some("A mode.".into()),
            variants,
            numeric,
            open,
            property: "Mode".into(),
            default: None,
        }
    }

    #[test]
    fn closed_string_enums_declare_aliases_for_serde() {
        let rendered = render_enum(&definition(
            false,
            false,
            vec![variant("Feature", "featureData".into(), &["FeatureData"])],
        ))
        .unwrap()
        .to_string();
        assert!(rendered.contains("alias = \"FeatureData\""), "{rendered}");
        assert!(rendered.contains("rename = \"featureData\""), "{rendered}");
    }

    #[test]
    fn open_string_enums_accept_aliases_on_both_input_paths() {
        let rendered = render_enum(&definition(
            true,
            false,
            vec![variant("Feature", "featureData".into(), &["FeatureData"])],
        ))
        .unwrap()
        .to_string();
        assert_eq!(rendered.matches("\"FeatureData\"").count(), 2, "{rendered}");
    }

    #[test]
    fn the_default_variant_is_configurable() {
        let mut mode = definition(
            false,
            false,
            vec![variant("A", "a".into(), &[]), variant("B", "b".into(), &[])],
        );
        mode.default = Some("B".into());
        assert!(
            render_enum(&mode)
                .unwrap()
                .to_string()
                .contains("Self :: B")
        );
        mode.default = Some("C".into());
        assert!(render_enum(&mode).is_err());
    }

    #[test]
    fn values_of_the_wrong_kind_are_errors_not_panics() {
        assert!(
            render_enum(&definition(
                false,
                true,
                vec![variant("A", "a".into(), &[])]
            ))
            .is_err()
        );
        assert!(render_enum(&definition(false, false, vec![variant("A", 1.into(), &[])])).is_err());
        assert!(render_enum(&definition(false, true, vec![])).is_err());
    }
}
