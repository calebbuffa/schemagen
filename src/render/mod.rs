//! Turns the generated [`model`](crate::model) into Rust source.
//!
//! Rendering is deterministic: items are ordered by kind and name, so the same
//! input always produces the same bytes and a `--check` can compare outputs.

mod defaults;
mod enums;
mod structs;
pub(crate) mod support;

use std::collections::BTreeSet;

use proc_macro2::{Span, TokenStream};
use quote::quote;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{AliasDef, Generated, RustType};
use crate::policy::GenerationPolicy;
use support::Helper;

/// One generated module and the rules it was generated under.
pub struct Module<'a> {
    /// The module's name, for multi-module output.
    pub name: &'a str,
    pub generated: &'a Generated,
    pub config: &'a Config,
    pub policy: &'a dyn GenerationPolicy,
}

pub(crate) fn ident(name: &str, what: &str) -> Result<syn::Ident> {
    syn::parse_str::<syn::Ident>(name)
        .map_err(|error| Error::render(format!("{what} `{name}` is not an identifier: {error}")))
}

pub(crate) fn string_literal(value: &str) -> syn::LitStr {
    syn::LitStr::new(value, Span::call_site())
}

pub(crate) fn doc(text: Option<&str>) -> TokenStream {
    text.map(|text| {
        let text = string_literal(text);
        quote!(#[doc = #text])
    })
    .unwrap_or_default()
}

/// Lints that generated code trips by construction, with the reason for each.
fn module_attributes() -> TokenStream {
    quote!(
        // Documentation comes from the schema, where it is often absent.
        #![allow(missing_docs)]
        // Not every generated type is used by every consumer.
        #![allow(dead_code)]
        // Defaults are written out explicitly so they read like the schema.
        #![allow(clippy::derivable_impls)]
        // Schema defaults may be spelled `3.14`.
        #![allow(clippy::approx_constant)]
        // Schema numbers keep the precision they were written with.
        #![allow(clippy::excessive_precision)]
        // Schema descriptions may be empty.
        #![allow(clippy::empty_docs)]
    )
}

fn imports(config: &Config) -> Result<TokenStream> {
    let mut tokens = TokenStream::new();
    for path in &config.imports {
        let tree = syn::parse_str::<syn::UseTree>(path)
            .map_err(|error| Error::config(format!("import `{path}`: {error}")))?;
        tokens.extend(quote!(use #tree;));
    }
    Ok(tokens)
}

struct Body {
    tokens: TokenStream,
    helpers: BTreeSet<Helper>,
}

fn alias_helpers(ty: &RustType, helpers: &mut BTreeSet<Helper>) {
    ty.walk(&mut |ty| match ty {
        RustType::SortedMap(..) => {
            helpers.insert(Helper::SortedMap);
        }
        RustType::InlineVec(..) => {
            helpers.insert(Helper::InlineVec);
        }
        _ => {}
    });
}

fn render_alias(alias: &AliasDef) -> Result<TokenStream> {
    let name = ident(&alias.name, "an alias name")?;
    let docs = doc(alias.description.as_deref());
    let target = &alias.target;
    Ok(quote!(#docs pub type #name = #target;))
}

/// Renders every item of one module, in a stable order.
fn render_body(module: &Module<'_>) -> Result<Body> {
    let generated = module.generated;
    let mut helpers = BTreeSet::new();
    let sorted = |mut items: Vec<(String, TokenStream)>| {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
            .into_iter()
            .map(|(_, tokens)| tokens)
            .collect::<Vec<_>>()
    };

    let mut custom = Vec::new();
    for definition in &generated.custom_enums {
        custom.push((definition.name.clone(), enums::render_enum(definition)?));
    }
    for alias in &generated.aliases {
        alias_helpers(&alias.target, &mut helpers);
        custom.push((alias.name.clone(), render_alias(alias)?));
    }

    let mut enum_items = Vec::new();
    let mut union_items = Vec::new();
    for definition in &generated.definitions {
        for item in &definition.enums {
            enum_items.push((item.name.clone(), enums::render_enum(item)?));
        }
        for item in &definition.unions {
            for variant in &item.variants {
                alias_helpers(&variant.ty, &mut helpers);
            }
            union_items.push((item.name.clone(), enums::render_union(item)?));
        }
    }

    let mut definitions = Vec::new();
    for definition in &generated.definitions {
        if let Some(target) = &definition.alias {
            alias_helpers(target, &mut helpers);
            let alias = AliasDef {
                name: definition.name.clone(),
                target: target.clone(),
                description: definition.description.clone(),
            };
            definitions.push((definition.name.clone(), render_alias(&alias)?));
        } else if definition.is_object {
            let rendered = structs::render_struct(definition)?;
            helpers.extend(rendered.helpers);
            let rendered = rendered.tokens;
            let items = module.policy.struct_items(definition);
            definitions.push((definition.name.clone(), quote!(#rendered #(#items)*)));
        }
    }

    let additional = module.policy.additional_definitions();
    let mut tokens = TokenStream::new();
    tokens.extend(imports(module.config)?);
    tokens.extend(sorted(custom));
    tokens.extend(additional);
    tokens.extend(sorted(enum_items));
    tokens.extend(sorted(union_items));
    tokens.extend(sorted(definitions));
    Ok(Body { tokens, helpers })
}

fn assemble(file_doc: &str, helpers: &BTreeSet<Helper>, body: TokenStream) -> Result<String> {
    let attributes = module_attributes();
    let file_doc = string_literal(file_doc);
    let helpers = support::render_helpers(&support::with_dependencies(helpers.iter().copied()))?;
    let tokens = quote!(
        #![doc = #file_doc]
        #attributes
        use serde::{Deserialize, Serialize};
        #helpers
        #body
    );
    format_tokens(tokens)
}

/// Parses `tokens` as a file and formats it, so that bad output is reported
/// here rather than as a confusing error in the consumer's build.
pub fn format_tokens(tokens: TokenStream) -> Result<String> {
    let file = syn::parse2::<syn::File>(tokens)?;
    Ok(prettyplease::unparse(&file))
}

/// Renders one module's types as a standalone source file.
pub fn render_module(
    file_doc: &str,
    generated: &Generated,
    config: &Config,
    policy: &dyn GenerationPolicy,
) -> Result<String> {
    let module = Module {
        name: "",
        generated,
        config,
        policy,
    };
    let body = render_body(&module)?;
    assemble(file_doc, &body.helpers, body.tokens)
}

/// Renders several modules into one file, as `pub mod name { .. }` blocks that
/// share a single copy of the support code.
pub fn render_modules(file_doc: &str, modules: &[Module<'_>]) -> Result<String> {
    let mut helpers = BTreeSet::new();
    let mut blocks = TokenStream::new();
    let mut seen = BTreeSet::new();
    for module in modules {
        if !seen.insert(module.name) {
            return Err(Error::render(format!("duplicate module `{}`", module.name)));
        }
        let name = ident(module.name, "a module name")?;
        let body = render_body(module)?;
        helpers.extend(body.helpers);
        let tokens = body.tokens;
        blocks.extend(quote!(
            pub mod #name {
                use super::*;
                #tokens
            }
        ));
    }
    assemble(file_doc, &helpers, blocks)
}
