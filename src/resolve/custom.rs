//! Types declared in configuration rather than described by a schema.

use serde_json::Value;

use super::Resolver;
use super::names::{check_ident, upper_camel};
use crate::config::CustomKind;
use crate::diagnostics::Location;
use crate::model::{AliasDef, EnumDef, EnumVariantDef, Generated};
use crate::policy::GenerationPolicy;

impl<P: GenerationPolicy + ?Sized> Resolver<'_, P> {
    /// Builds the enums and aliases under `customTypes`.
    pub(super) fn custom_types(&mut self) -> Generated {
        let mut generated = Generated::default();
        for (name, custom) in &self.config.custom_types {
            let location = Location::new("config").with_pointer(format!("/customTypes/{name}"));
            if let Err(error) = check_ident(name, "a custom type name", name) {
                self.graph.sink_mut().error(location, error.to_string());
                continue;
            }
            if let Some(origin) = &custom.origin {
                generated
                    .custom_origins
                    .push((name.clone(), origin.clone()));
            }
            match custom.kind {
                CustomKind::Alias => {
                    let Some(target) = custom.target.clone() else {
                        continue;
                    };
                    generated.aliases.push(AliasDef {
                        name: name.clone(),
                        target,
                        description: custom.doc.clone(),
                    });
                }
                CustomKind::Enum => {
                    let variants = custom
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(index, variant)| {
                            let value = if custom.numeric {
                                let position = i64::try_from(index).unwrap_or(i64::MAX);
                                Value::from(
                                    custom
                                        .numeric_values
                                        .get(variant)
                                        .copied()
                                        .unwrap_or(position),
                                )
                            } else {
                                Value::String(variant.clone())
                            };
                            let variant_name = upper_camel(variant);
                            if let Err(error) =
                                check_ident(&variant_name, "a variant name", variant)
                            {
                                self.graph
                                    .sink_mut()
                                    .error(location.clone(), error.to_string());
                            }
                            EnumVariantDef {
                                name: variant_name,
                                value,
                                aliases: Vec::new(),
                            }
                        })
                        .collect();
                    generated.custom_enums.push(EnumDef {
                        name: name.clone(),
                        description: custom.doc.clone(),
                        variants,
                        numeric: custom.numeric,
                        open: false,
                        property: name.clone(),
                        default: custom.default.as_deref().map(upper_camel),
                    });
                }
            }
        }
        generated
    }
}
