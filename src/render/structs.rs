//! Rendering structs and their serde attributes.

use std::collections::BTreeSet;

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::quote;

use super::defaults::default_expression;
use super::support::Helper;
use super::{doc, ident, string_literal};
use crate::error::{Error, Result};
use crate::model::{FieldDef, RustType, StructDef};

pub(super) struct RenderedStruct {
    pub tokens: TokenStream,
    pub helpers: BTreeSet<Helper>,
}

/// How one field is declared to serde.
///
/// Planning is separate from emitting so the same decisions drive both the
/// attributes and the set of support helpers the module must carry.
struct FieldPlan {
    rename: Option<String>,
    default: Default_,
    skip_serializing_if: Option<String>,
    deserialize_with: Option<&'static str>,
    flatten: bool,
    helpers: BTreeSet<Helper>,
}

enum Default_ {
    None,
    Trait,
    Function(String),
}

/// What serde's `rename_all = "camelCase"` makes of a Rust field name.
fn serde_camel_case(rust_name: &str) -> String {
    let mut out = String::new();
    let mut capitalize = false;
    for ch in rust_name.trim_start_matches("r#").chars() {
        if ch == '_' {
            capitalize = !out.is_empty();
        } else if capitalize {
            out.extend(ch.to_uppercase());
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn default_function(owner: &str, field: &FieldDef) -> String {
    format!(
        "default_{}_{}",
        owner.to_snake_case(),
        field.rust_name.trim_start_matches("r#").to_snake_case()
    )
}

fn has_default_function(field: &FieldDef) -> bool {
    field.default.is_some() && !field.flatten
}

fn plan(owner: &str, field: &FieldDef) -> FieldPlan {
    let mut helpers = BTreeSet::new();
    let rename =
        (serde_camel_case(&field.rust_name) != field.json_name).then(|| field.json_name.clone());

    field.ty.walk(&mut |ty| match ty {
        RustType::SortedMap(..) => {
            helpers.insert(Helper::SortedMap);
        }
        RustType::InlineVec(..) => {
            helpers.insert(Helper::InlineVec);
        }
        _ => {}
    });

    if field.flatten {
        return FieldPlan {
            rename,
            default: Default_::Trait,
            skip_serializing_if: None,
            deserialize_with: None,
            flatten: true,
            helpers,
        };
    }

    let default = match (&field.default, field.required) {
        (Some(_), _) => Default_::Function(default_function(owner, field)),
        (None, false) => Default_::Trait,
        (None, true) => Default_::None,
    };
    let optional_without_default = !field.required && field.default.is_none();

    // An absent property deserializes to an empty collection, so writing the
    // empty value back would not round-trip.
    let skip_serializing_if = field.skip_serializing_if.clone().or_else(|| {
        if !optional_without_default {
            None
        } else if matches!(field.ty, RustType::Option(_)) {
            Some("Option::is_none".to_owned())
        } else if field.ty.omits_when_empty() {
            helpers.insert(Helper::EmptyCollection);
            Some("is_empty_collection".to_owned())
        } else {
            None
        }
    });

    // `Box<[T]>` derived through `Vec` allocates twice; the helper sizes its
    // allocation exactly. Other optional collections need the `null`-tolerant
    // deserializer for the reason given beside `deserialize_null_default`.
    let deserialize_with = if matches!(field.ty, RustType::BoxedSlice(_)) {
        helpers.insert(Helper::BoxedSlice);
        Some("deserialize_boxed_slice")
    } else if optional_without_default && field.ty.omits_when_empty() {
        helpers.insert(Helper::EmptyCollection);
        Some("deserialize_null_default")
    } else {
        None
    };

    FieldPlan {
        rename,
        default,
        skip_serializing_if,
        deserialize_with,
        flatten: false,
        helpers,
    }
}

impl FieldPlan {
    fn attribute(&self) -> Result<TokenStream> {
        let mut parts = Vec::new();
        if let Some(rename) = &self.rename {
            let rename = string_literal(rename);
            parts.push(quote!(rename = #rename));
        }
        match &self.default {
            Default_::None => {}
            Default_::Trait => parts.push(quote!(default)),
            Default_::Function(name) => {
                let name = string_literal(name);
                parts.push(quote!(default = #name));
            }
        }
        if let Some(predicate) = &self.skip_serializing_if {
            let predicate = string_literal(predicate);
            parts.push(quote!(skip_serializing_if = #predicate));
        }
        if self.flatten {
            parts.push(quote!(flatten));
        }
        if let Some(function) = self.deserialize_with {
            let function = string_literal(function);
            parts.push(quote!(deserialize_with = #function));
        }
        Ok(if parts.is_empty() {
            quote!()
        } else {
            quote!(#[serde(#(#parts),*)])
        })
    }
}

pub(super) fn render_struct(definition: &StructDef) -> Result<RenderedStruct> {
    let name = ident(&definition.name, "a struct name")?;
    let docs = doc(definition.description.as_deref());
    let mut helpers = BTreeSet::new();
    let mut default_functions = Vec::new();
    let mut fields = Vec::new();

    for field in &definition.fields {
        let plan = plan(&definition.name, field);
        let field_name = ident(&field.rust_name, "a field name")?;
        let ty = &field.ty;
        let attribute = plan.attribute()?;
        let field_doc = doc(field.description.as_deref());
        if let (Default_::Function(function), Some(value)) = (&plan.default, &field.default) {
            let function = ident(function, "a default function name")?;
            let expression = default_expression(value, ty).map_err(|error| {
                Error::render(format!("{}.{}: {error}", definition.name, field.json_name))
            })?;
            default_functions.push(quote!(fn #function() -> #ty { #expression }));
        }
        helpers.extend(plan.helpers);
        fields.push(quote!(#field_doc #attribute pub #field_name: #ty,));
    }

    let mut extras = Vec::new();
    for field in &definition.extra_fields {
        let field_name = ident(&field.name, "an extra field name")?;
        let ty = &field.ty;
        ty.walk(&mut |ty| match ty {
            RustType::SortedMap(..) => {
                helpers.insert(Helper::SortedMap);
            }
            RustType::InlineVec(..) => {
                helpers.insert(Helper::InlineVec);
            }
            _ => {}
        });
        let field_doc = doc(field.description.as_deref());
        let attribute = if field.skip_serde {
            quote!(#[serde(skip)])
        } else {
            quote!(#[serde(default)])
        };
        extras.push(quote!(#field_doc #attribute pub #field_name: #ty,));
    }

    let extension_fields = if definition.extensions {
        quote!(
            #[doc = "Extension-specific data."]
            #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
            pub extensions: std::collections::HashMap<String, serde_json::Value>,
            #[doc = "Application-specific data."]
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub extras: Option<serde_json::Value>,
        )
    } else {
        quote!()
    };

    // `Default` must agree with deserializing `{}`, so it cannot be derived
    // once any field carries a schema default.
    let has_schema_default = definition.fields.iter().any(has_default_function);
    let (derive, default_impl) = if has_schema_default {
        let initializers = definition.fields.iter().map(|field| {
            let field_name = ident(&field.rust_name, "a field name")?;
            Ok(if has_default_function(field) {
                let function = ident(&default_function(&definition.name, field), "a name")?;
                quote!(#field_name: #function(),)
            } else {
                quote!(#field_name: Default::default(),)
            })
        });
        let initializers = initializers.collect::<Result<Vec<_>>>()?;
        let extra_names = definition
            .extra_fields
            .iter()
            .map(|f| ident(&f.name, "an extra field name"))
            .collect::<Result<Vec<_>>>()?;
        let extension_defaults = if definition.extensions {
            quote!(extensions: Default::default(), extras: Default::default(),)
        } else {
            quote!()
        };
        (
            quote!(#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]),
            quote!(
                impl Default for #name {
                    fn default() -> Self {
                        Self {
                            #(#initializers)*
                            #(#extra_names: Default::default(),)*
                            #extension_defaults
                        }
                    }
                }
            ),
        )
    } else {
        (
            quote!(#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]),
            quote!(),
        )
    };

    Ok(RenderedStruct {
        tokens: quote!(
            #docs
            #derive
            #[serde(rename_all = "camelCase")]
            pub struct #name { #(#fields)* #(#extras)* #extension_fields }
            #default_impl
            #(#default_functions)*
        ),
        helpers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(json: &str, rust: &str, ty: RustType, required: bool) -> FieldDef {
        FieldDef {
            json_name: json.into(),
            rust_name: rust.into(),
            ty,
            required,
            description: None,
            default: None,
            skip_serializing_if: None,
            flatten: false,
        }
    }

    fn attribute(field: &FieldDef) -> String {
        plan("Thing", field).attribute().unwrap().to_string()
    }

    #[test]
    fn camel_case_matches_serde() {
        assert_eq!(serde_camel_case("foo_bar_baz"), "fooBarBaz");
        assert_eq!(serde_camel_case("r#type"), "type");
        assert_eq!(serde_camel_case("uv0"), "uv0");
        assert_eq!(serde_camel_case("_x"), "x");
    }

    #[test]
    fn names_serde_would_derive_need_no_rename() {
        let plain = field("fooBar", "foo_bar", RustType::U32, true);
        assert_eq!(attribute(&plain), "");
        let keyword = field("type", "r#type", RustType::U32, true);
        assert_eq!(attribute(&keyword), "");
    }

    #[test]
    fn names_serde_would_not_derive_are_renamed() {
        let odd = field("FooBar", "foo_bar", RustType::U32, true);
        assert_eq!(attribute(&odd), "# [serde (rename = \"FooBar\")]");
    }

    #[test]
    fn optional_collections_tolerate_null_and_skip_when_empty() {
        let list = field(
            "items",
            "items",
            RustType::Vec(Box::new(RustType::U8)),
            false,
        );
        let planned = plan("Thing", &list);
        assert!(planned.helpers.contains(&Helper::EmptyCollection));
        let rendered = planned.attribute().unwrap().to_string();
        assert!(rendered.contains("is_empty_collection"), "{rendered}");
        assert!(rendered.contains("deserialize_null_default"), "{rendered}");
    }

    #[test]
    fn an_explicit_skip_predicate_is_not_duplicated() {
        let mut item = field("x", "x", RustType::Option(Box::new(RustType::U8)), false);
        item.skip_serializing_if = Some("custom".into());
        let rendered = attribute(&item);
        assert_eq!(
            rendered.matches("skip_serializing_if").count(),
            1,
            "{rendered}"
        );
        assert!(rendered.contains("custom"));
    }

    #[test]
    fn defaults_become_functions_named_for_the_field() {
        let mut item = field("count", "count", RustType::U32, false);
        item.default = Some(3.into());
        let rendered = render_struct(&StructDef {
            name: "Thing".into(),
            title: "Thing".into(),
            source_file: String::new(),
            description: None,
            is_object: true,
            alias: None,
            fields: vec![item],
            extra_fields: vec![],
            extensions: false,
            enums: vec![],
            unions: vec![],
        })
        .unwrap()
        .tokens
        .to_string();
        assert!(
            rendered.contains("default = \"default_thing_count\""),
            "{rendered}"
        );
        assert!(
            rendered.contains("fn default_thing_count () -> u32 { 3 }"),
            "{rendered}"
        );
    }

    #[test]
    fn a_default_that_does_not_fit_is_an_error_naming_the_field() {
        let mut item = field("count", "count", RustType::U8, false);
        item.default = Some(300.into());
        let error = render_struct(&StructDef {
            name: "Thing".into(),
            title: "Thing".into(),
            source_file: String::new(),
            description: None,
            is_object: true,
            alias: None,
            fields: vec![item],
            extra_fields: vec![],
            extensions: false,
            enums: vec![],
            unions: vec![],
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("Thing.count"), "{error}");
    }
}
