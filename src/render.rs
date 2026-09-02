//! Rust source rendering using `syn`, `quote`, and `prettyplease`.

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse_str;

use crate::config::Config;
use crate::policy::GenerationPolicy;
use crate::types::{RustType, StructDef};

pub fn render_module<P: GenerationPolicy>(
    module_doc: &str,
    structs: &[StructDef],
    config: &Config,
    policy: &P,
) -> Result<String, String> {
    let definitions: Vec<TokenStream> = structs
        .iter()
        .filter(|definition| definition.is_object || definition.alias.is_some())
        .map(render_definition)
        .collect::<Result<_, _>>()?;
    let doc = syn::LitStr::new(module_doc, proc_macro2::Span::call_site());
    let mut custom_types = config
        .custom_types
        .iter()
        .map(|(name, definition)| render_custom_type(name, definition))
        .collect::<Result<Vec<_>, _>>()?;
    custom_types.sort_by_key(|definition| definition.to_string());
    let policy_definitions = policy.additional_definitions();
    let mut enum_definitions = structs
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .map(render_schema_enum)
        .collect::<Result<Vec<_>, _>>()?;
    enum_definitions.sort_by_key(|definition| definition.to_string());
    let mut union_definitions = structs
        .iter()
        .flat_map(|definition| definition.unions.iter())
        .map(render_union)
        .collect::<Result<Vec<_>, _>>()?;
    union_definitions.sort_by_key(|definition| definition.to_string());
    let tokens = quote! {
        #![doc = #doc]
        #![allow(missing_docs)]
        use serde::{Deserialize, Serialize};
        #(#custom_types)*
        #(#policy_definitions)*
        #(#enum_definitions)*
        #(#union_definitions)*
        #(#definitions)*
    };
    let token_text = tokens.to_string();
    let file: syn::File = syn::parse2(tokens).map_err(|error| {
        format!(
            "generated Rust is invalid: {error}; output starts: {}",
            &token_text[..token_text.len().min(1000)]
        )
    })?;
    Ok(prettyplease::unparse(&file))
}

fn render_union(definition: &crate::types::UnionDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let variant_name = parse_str::<syn::Ident>(&variant.name)
                .map_err(|e| format!("union {} variant {}: {e}", definition.name, variant.name))?;
            let ty = parse_str::<syn::Type>(&variant.ty.to_string()).map_err(|e| e.to_string())?;
            Ok::<_, String>(quote!(#variant_name(#ty)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let default_variant = definition
        .variants
        .first()
        .ok_or_else(|| format!("union {} has no variants", definition.name))?;
    let default_name = parse_str::<syn::Ident>(&default_variant.name).map_err(|e| e.to_string())?;
    Ok(quote!(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(untagged)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default_name(Default::default()) }
        }
    ))
}

fn render_definition(definition: &StructDef) -> Result<TokenStream, String> {
    if definition.is_object {
        return render_struct(definition);
    }
    let name = parse_str::<syn::Ident>(&definition.name)
        .map_err(|e| format!("enum {}: {e}", definition.name))?;
    let ty = parse_str::<syn::Type>(&definition.alias.as_ref().unwrap().to_string())
        .map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|doc| {
            let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #literal])
        })
        .unwrap_or_default();
    Ok(quote!(#docs pub type #name = #ty;))
}

fn render_schema_enum(definition: &crate::types::EnumDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|doc| {
            let lit = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #lit])
        })
        .unwrap_or_default();
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let name = parse_str::<syn::Ident>(&variant.name)
                .map_err(|e| format!("enum {} variant {}: {e}", definition.name, variant.name))?;
            let value = match &variant.value {
                serde_json::Value::String(value) => {
                    syn::LitStr::new(value, proc_macro2::Span::call_site())
                }
                _ => return Err("schema enum value must be a string".to_string()),
            };
            Ok::<_, String>(quote!(#[serde(rename = #value)] #name))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let default_variant = definition
        .variants
        .first()
        .ok_or_else(|| format!("enum {} has no variants", definition.name))?;
    let default_name = parse_str::<syn::Ident>(&default_variant.name).map_err(|e| e.to_string())?;
    Ok(quote!(
        #docs
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default_name }
        }
    ))
}

fn render_struct(definition: &StructDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
    let docs = docs
        .map(|value| quote!(#[doc = #value]))
        .unwrap_or_default();
    let mut default_functions = Vec::new();
    let fields: Vec<TokenStream> = definition
        .fields
        .iter()
        .map(|field| {
            let field_name =
                parse_str::<syn::Ident>(&field.rust_name).map_err(|e| e.to_string())?;
            let type_source = field.ty.to_string();
            let ty = parse_str::<syn::Type>(&type_source)
                .map_err(|e| format!("{} ({type_source}): {e}", field.json_name))?;
            let json_name = syn::LitStr::new(&field.json_name, proc_macro2::Span::call_site());
            let field_doc = field
                .description
                .as_deref()
                .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
            let rename = if field.rust_name != field.json_name {
                quote!(rename = #json_name,)
            } else {
                quote!()
            };
            let default = if field.default.is_some() {
                let function_name = syn::Ident::new(
                    &format!(
                        "default_{}_{}",
                        definition.name.to_snake_case(),
                        field.rust_name.trim_start_matches("r#").to_snake_case()
                    ),
                    proc_macro2::Span::call_site(),
                );
                let expression = default_expression(field.default.as_ref().unwrap(), &field.ty)?;
                let return_type = ty.clone();
                default_functions.push(quote!(fn #function_name() -> #return_type { #expression }));
                let function_name_literal =
                    syn::LitStr::new(&function_name.to_string(), proc_macro2::Span::call_site());
                quote!(default = #function_name_literal,)
            } else if field.required {
                quote!()
            } else if field.ty.to_string().starts_with("Option<") {
                quote!(default, skip_serializing_if = "Option::is_none",)
            } else {
                quote!(default,)
            };
            let skip = field
                .skip_serializing_if
                .as_deref()
                .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
            let skip = skip
                .map(|value| quote!(skip_serializing_if = #value,))
                .unwrap_or_default();
            let flatten = field.flatten.then(|| quote!(flatten,)).unwrap_or_default();
            let serde = if field.flatten {
                quote!(#[serde(default, #flatten)])
            } else if field.required
                && field.default.is_none()
                && field.rust_name == field.json_name
            {
                quote!()
            } else {
                quote!(#[serde(#rename #default #skip #flatten)])
            };
            let docs = field_doc
                .map(|value| quote!(#[doc = #value]))
                .unwrap_or_default();
            Ok(quote! {
                #docs
                #serde
                pub #field_name: #ty,
            })
        })
        .collect::<Result<_, String>>()?;
    let extra_fields = definition
        .extra_fields
        .iter()
        .map(|field| {
            let name = parse_str::<syn::Ident>(&field.name).map_err(|e| e.to_string())?;
            let ty = parse_str::<syn::Type>(&field.rust_type).map_err(|e| e.to_string())?;
            let docs = field
                .description
                .as_deref()
                .map(|doc| {
                    let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
                    quote!(#[doc = #literal])
                })
                .unwrap_or_default();
            let serde = if field.skip_serde {
                quote!(#[serde(skip)])
            } else {
                quote!(#[serde(default)])
            };
            Ok::<TokenStream, String>(quote!(#docs #serde pub #name: #ty,))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let extension_fields = if definition.extensions {
        quote!(#[doc = "Extension-specific data."] #[serde(default)] pub extensions: std::collections::HashMap<String, serde_json::Value>, #[doc = "Application-specific data."] pub extras: Option<serde_json::Value>,)
    } else {
        quote!()
    };
    Ok(quote! {
        #docs
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct #name { #(#fields)* #(#extra_fields)* #extension_fields }
        #(#default_functions)*
    })
}

fn default_expression(value: &serde_json::Value, ty: &RustType) -> Result<TokenStream, String> {
    if let RustType::Option(inner) = ty {
        let expression = default_expression(value, inner)?;
        return Ok(quote!(Some(#expression)));
    }
    if let RustType::Array(inner, _) | RustType::Vec(inner) = ty {
        let expressions = value
            .as_array()
            .ok_or_else(|| "array default must be a JSON array".to_string())?
            .iter()
            .map(|value| default_expression(value, inner))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote!([#(#expressions),*]));
    }
    if matches!(ty, RustType::Named(_)) {
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        let literal = syn::LitStr::new(&json, proc_macro2::Span::call_site());
        return Ok(quote!(serde_json::from_str(#literal).expect("valid generated JSON default")));
    }
    if matches!(ty, RustType::Value) {
        if let Some(string) = value.as_str() {
            let literal = syn::LitStr::new(string, proc_macro2::Span::call_site());
            return Ok(quote!(serde_json::Value::String(#literal.to_owned())));
        }
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        let literal = syn::LitStr::new(&json, proc_macro2::Span::call_site());
        return Ok(quote!(serde_json::from_str(#literal).expect("valid generated JSON default")));
    }
    if let Some(value) = value.as_str() {
        let literal = syn::LitStr::new(value, proc_macro2::Span::call_site());
        if matches!(ty, RustType::String) {
            return Ok(quote!(#literal.to_string()));
        }
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_bool() {
        return Ok(quote!(#value));
    }
    if let Some(value) = value.as_i64() {
        if matches!(ty, RustType::F64) {
            let literal = syn::LitFloat::new(&format!("{value}.0"), proc_macro2::Span::call_site());
            return Ok(quote!(#literal));
        }
        let literal = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_u64() {
        if matches!(ty, RustType::F64) {
            let literal = syn::LitFloat::new(&format!("{value}.0"), proc_macro2::Span::call_site());
            return Ok(quote!(#literal));
        }
        let literal = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_f64() {
        let literal = syn::LitFloat::new(&format!("{value:?}"), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(values) = value.as_array() {
        let expressions = values
            .iter()
            .map(|value| default_expression(value, ty))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote!([#(#expressions),*]));
    }
    Ok(quote!(Default::default()))
}

fn render_custom_type(
    type_name: &str,
    config: &crate::config::CustomTypeConfig,
) -> Result<TokenStream, String> {
    if config.kind != "enum" {
        return Err(format!("unsupported custom type kind: {}", config.kind));
    }
    let name =
        parse_str::<syn::Ident>(&type_name.to_upper_camel_case()).map_err(|e| e.to_string())?;
    let variants = config
        .variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let variant_name = variant.to_upper_camel_case();
            let variant_ident =
                parse_str::<syn::Ident>(&variant_name).map_err(|e| e.to_string())?;
            if config.numeric {
                let value = config
                    .numeric_values
                    .get(variant)
                    .copied()
                    .unwrap_or(index as u32);
                let value = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
                Ok(quote!(#variant_ident = #value))
            } else {
                let value = syn::LitStr::new(variant, proc_macro2::Span::call_site());
                Ok(quote!(#[serde(rename = #value)] #variant_ident))
            }
        })
        .collect::<Result<Vec<TokenStream>, String>>()?;
    let default_variant = config
        .variants
        .first()
        .map(|variant| parse_str::<syn::Ident>(&variant.to_upper_camel_case()))
        .transpose()
        .map_err(|error| error.to_string())?;
    let default_impl = default_variant
        .map(|variant| quote!(impl Default for #name { fn default() -> Self { Self::#variant } }))
        .unwrap_or_default();
    let docs = config
        .doc
        .as_deref()
        .map(|doc| {
            let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #literal])
        })
        .unwrap_or_default();
    let derives = if config.numeric {
        quote!(#[repr(u32)] #[derive(Debug, Clone, Copy, PartialEq, Eq, serde_repr::Serialize_repr, serde_repr::Deserialize_repr)])
    } else {
        quote!(#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)])
    };
    Ok(quote!(#docs #derives pub enum #name { #(#variants),* } #default_impl))
}
