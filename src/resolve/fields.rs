//! Struct fields: which properties a schema contributes and how each is typed.

use std::collections::BTreeMap;

use heck::ToUpperCamelCase;

use super::Resolver;
use super::enums::{array_item_enum, finite_values};
use super::names::field_name;
use super::types::collection_of;
use crate::config::SchemaConfig;
use crate::graph::SchemaId;
use crate::ir::SchemaNode;
use crate::model::{EnumDef, FieldDef, RustType, UnionDef};
use crate::policy::GenerationPolicy;

type Properties = BTreeMap<String, SchemaNode>;

impl<P: GenerationPolicy + ?Sized> Resolver<'_, P> {
    pub(super) fn fields_for(
        &mut self,
        source: &SchemaId,
        node: &SchemaNode,
        owner: &str,
        enums: &mut Vec<EnumDef>,
        unions: &mut Vec<UnionDef>,
    ) -> Vec<FieldDef> {
        let Some(object) = node.object.as_ref() else {
            return Vec::new();
        };
        let policy = self.policy;
        let settings = self.settings;
        let mut properties = Properties::new();
        let mut required = Vec::new();
        if let Some(reference) = &node.reference {
            match self.graph.resolve_node(source, reference) {
                Some((target, resolved)) => {
                    self.merge_branch(&target, &resolved, &mut properties, &mut required);
                }
                None => self
                    .sink()
                    .error(node.location.clone(), "unable to resolve object $ref"),
            }
        }
        // A schema's own properties refine the base it references.
        properties.extend(object.properties.clone());
        for name in &object.required {
            if !required.contains(name) {
                required.push(name.clone());
            }
        }
        for branch in &node.combinators.all_of {
            self.merge_branch(source, branch, &mut properties, &mut required);
        }

        let label = self.label(node);
        let overrides = self.schema_config(&label).unwrap_or_default();
        self.check_configured_properties(node, &label, &overrides, &properties);

        let extensible = self.config.extensible;
        properties.retain(|name, schema| {
            let reserved = extensible && matches!(name.as_str(), "extensions" | "extras");
            !(reserved
                || overrides.property_skip.contains(name)
                || policy.skip_field(node, name, schema))
        });

        let mut fields: Vec<FieldDef> = Vec::new();
        for (json_name, schema) in &properties {
            let is_required = overrides
                .property_required
                .get(json_name)
                .copied()
                .or_else(|| policy.field_required(node, json_name, schema))
                .unwrap_or_else(|| required.contains(json_name));
            let property = json_name.to_upper_camel_case();
            let default = schema
                .metadata
                .default
                .clone()
                .or_else(|| overrides.property_defaults.get(json_name).cloned());

            let ty = if let Some(chosen) =
                self.field_type_override(owner, node, json_name, schema, &overrides)
            {
                chosen
            } else if let Some(finite) = finite_values(schema, settings.infer_open_enums) {
                RustType::Named(self.push_enum(enums, owner, json_name, schema, finite))
            } else if let Some((finite, items)) = array_item_enum(schema, settings.infer_open_enums)
            {
                // An enumeration declared on `items` constrains each element,
                // so the named enum becomes the element type.
                let name = self.push_enum(enums, owner, json_name, items, finite);
                collection_of(schema, RustType::Named(name))
            } else if let Some((ty, union)) =
                self.union_type(source, schema, &format!("{owner}{property}"))
            {
                unions.push(union);
                ty
            } else {
                self.type_for(source, schema)
            };

            // Collections represent absence as emptiness, so wrapping them in
            // an `Option` would add a second way to say the same thing.
            let ty = if !is_required
                && default.is_none()
                && !ty.is_collection()
                && !matches!(ty, RustType::Option(_))
            {
                RustType::Option(Box::new(ty))
            } else {
                ty
            };
            let rust_name = match field_name(json_name) {
                Ok(name) => name,
                Err(error) => {
                    self.sink()
                        .error(schema.location.clone(), error.to_string());
                    continue;
                }
            };
            fields.push(FieldDef {
                json_name: json_name.clone(),
                rust_name,
                ty,
                required: is_required,
                description: schema.metadata.description.clone(),
                default,
                skip_serializing_if: policy
                    .skip_serializing_if(node, json_name, schema)
                    .or_else(|| overrides.property_skip_if.get(json_name).cloned()),
                flatten: false,
            });
        }

        if properties.is_empty()
            && let Some(value_schema) = object.additional_properties.as_deref()
            && !value_schema.bool_false
        {
            let value = if value_schema.bool_true {
                RustType::Value
            } else {
                self.type_for(source, value_schema)
            };
            // `#[serde(flatten)]` needs a real map deserializer, so this is a
            // `HashMap` whatever representation the settings choose.
            fields.push(FieldDef {
                json_name: "additionalProperties".to_string(),
                rust_name: "additional_properties".to_string(),
                ty: RustType::Map(Box::new(RustType::String), Box::new(value)),
                required: false,
                description: None,
                default: None,
                skip_serializing_if: None,
                flatten: true,
            });
        }
        fields.sort_by(|a, b| a.json_name.cmp(&b.json_name));
        self.check_field_collisions(node, owner, &fields);
        fields
    }

    /// The type chosen for a property by the policy or the configuration, in
    /// that order of precedence, if either has an opinion.
    fn field_type_override(
        &mut self,
        owner: &str,
        node: &SchemaNode,
        json_name: &str,
        schema: &SchemaNode,
        overrides: &SchemaConfig,
    ) -> Option<RustType> {
        if let Some(chosen) = self.policy.field_type(node, json_name, schema) {
            return Some(chosen);
        }
        if let Some(chosen) = overrides.property_types.get(json_name) {
            return Some(chosen.clone());
        }
        for key in [format!("{owner}.{json_name}"), format!("*.{json_name}")] {
            if let Some(chosen) = self.config.field_types.get(&key) {
                self.used.field_types.insert(key);
                return Some(chosen.clone());
            }
        }
        None
    }

    /// Reports configured properties the schema does not have, which is
    /// always a typo or a leftover.
    fn check_configured_properties(
        &mut self,
        node: &SchemaNode,
        label: &str,
        overrides: &SchemaConfig,
        properties: &Properties,
    ) {
        let configured = overrides
            .property_types
            .keys()
            .chain(overrides.property_defaults.keys())
            .chain(overrides.property_skip_if.keys())
            .chain(overrides.property_required.keys())
            .chain(overrides.property_skip.iter());
        let mut stale: Vec<&String> = configured
            .filter(|name| !properties.contains_key(*name))
            .collect();
        stale.sort();
        stale.dedup();
        for name in stale {
            self.sink().warn(
                node.location.clone(),
                format!(
                    "config for `{label}` names property `{name}`, which the schema does not have"
                ),
            );
        }
    }

    fn check_field_collisions(&mut self, node: &SchemaNode, owner: &str, fields: &[FieldDef]) {
        for (index, field) in fields.iter().enumerate() {
            if let Some(earlier) = fields[..index]
                .iter()
                .find(|other| other.rust_name == field.rust_name)
            {
                self.sink().error(
                    node.location.clone(),
                    format!(
                        "{owner}: properties `{}` and `{}` both become the field `{}`",
                        earlier.json_name, field.json_name, field.rust_name
                    ),
                );
            }
        }
    }

    /// Folds an `allOf` branch (or referenced base schema) into the property
    /// set being assembled.
    fn merge_branch(
        &mut self,
        source: &SchemaId,
        branch: &SchemaNode,
        properties: &mut Properties,
        required: &mut Vec<String>,
    ) {
        if let Some(reference) = &branch.reference {
            match self.graph.resolve_node(source, reference) {
                Some((target, resolved)) => {
                    self.merge_branch(&target, &resolved, properties, required);
                }
                None => self
                    .sink()
                    .error(branch.location.clone(), "unable to resolve allOf reference"),
            }
            return;
        }
        if let Some(object) = &branch.object {
            for (name, schema) in &object.properties {
                if properties.contains_key(name) {
                    self.sink().warn(
                        branch.location.clone(),
                        format!("allOf property {name} overrides an inherited property"),
                    );
                }
                properties.insert(name.clone(), schema.clone());
            }
            for name in &object.required {
                if !required.contains(name) {
                    required.push(name.clone());
                }
            }
        }
        for nested in &branch.combinators.all_of {
            self.merge_branch(source, nested, properties, required);
        }
    }
}
