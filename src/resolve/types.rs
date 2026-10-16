//! Mapping schema nodes onto Rust types.

use super::Resolver;
use crate::graph::SchemaId;
use crate::ir::SchemaNode;
use crate::model::{RustType, UnionDef, UnionVariantDef};
use crate::policy::GenerationPolicy;
use crate::settings::TypeSettings;

/// The longest fixed-size array rendered as `[T; N]`; beyond it a `Vec` is
/// kinder to the stack.
const MAX_FIXED_ARRAY: u32 = 32;

impl<P: GenerationPolicy + ?Sized> Resolver<'_, P> {
    pub(super) fn type_for(&mut self, source: &SchemaId, node: &SchemaNode) -> RustType {
        let settings = self.settings;
        if node.object.is_none()
            && node.reference.is_none()
            && node.combinators.all_of.len() == 1
            && node.types.only().is_none()
        {
            return self.type_for(source, &node.combinators.all_of[0]);
        }
        if node.bool_true {
            return RustType::Value;
        }
        if node.bool_false {
            self.sink().error(
                node.location.clone(),
                "false schema cannot be represented as a Rust field",
            );
            return RustType::Value;
        }
        if let Some(reference) = &node.reference {
            return match self.graph.resolve_node(source, reference) {
                Some((target, resolved)) => self.reference_type(&target, &resolved),
                None => {
                    self.sink()
                        .error(node.location.clone(), "unable to resolve $ref");
                    RustType::Value
                }
            };
        }
        if node.types.null && node.types.only().is_none() {
            let non_null = node.types.string
                || node.types.integer
                || node.types.number
                || node.types.boolean
                || node.types.object
                || node.types.array;
            return if non_null {
                RustType::Option(Box::new(RustType::Value))
            } else {
                RustType::Value
            };
        }
        let union = if !node.combinators.any_of.is_empty() {
            &node.combinators.any_of
        } else {
            &node.combinators.one_of
        };
        if !union.is_empty() {
            if node.metadata.title.is_some() {
                let name = self.type_name(node);
                self.discover(node);
                return RustType::Named(name);
            }
            let non_null: Vec<_> = union
                .iter()
                .filter(|branch| !branch.types.null && !branch.bool_true)
                .collect();
            if union.len() == 2 && non_null.len() == 1 {
                let inner = self.type_for(source, non_null[0]);
                return RustType::Option(Box::new(inner));
            }
            if let Some(primitive) = open_primitive_union(union, settings) {
                return primitive;
            }
            self.sink().warn(
                node.location.clone(),
                "general union schema is represented as serde_json::Value; \
                 give it a title to generate an enum",
            );
            return RustType::Value;
        }
        if node.enum_values.is_some() || node.const_value.is_some() {
            return primitive_type_for_enum(node, settings);
        }
        let base = match node.types.only() {
            Some("boolean") => RustType::Bool,
            Some("string") => settings.string.rust_type(),
            Some("integer") => integer_type(node, settings),
            Some("number") => settings.number.rust_type(),
            Some("array") => self.array_type(source, node),
            Some("object") => self.object_type(source, node),
            Some("null") => RustType::Value,
            _ if node
                .object
                .as_ref()
                .is_some_and(|object| object.additional_properties.is_some()) =>
            {
                self.map_type(source, node)
            }
            _ if node.object.is_some() => self.object_type(source, node),
            _ => RustType::Value,
        };
        if node.types.null {
            RustType::Option(Box::new(base))
        } else {
            base
        }
    }

    /// The type a reference to `resolved` becomes: a configured replacement,
    /// `Value` when the schema is skipped, the generated type otherwise, or
    /// the schema's own structure when it is an untitled non-object.
    pub(super) fn reference_type(&mut self, target: &SchemaId, resolved: &SchemaNode) -> RustType {
        let label = self.label(resolved);
        if let Some(schema) = self.schema_config(&label) {
            if let Some(replacement) = &schema.replace_with {
                return replacement.clone();
            }
            if schema.skip {
                return RustType::Value;
            }
        }
        if resolved.metadata.title.is_none() && resolved.object.is_none() {
            return self.type_for(target, resolved);
        }
        let name = self.type_name(resolved);
        self.discover(resolved);
        RustType::Named(name)
    }

    fn array_type(&mut self, source: &SchemaId, node: &SchemaNode) -> RustType {
        let items = node.array.as_ref().and_then(|array| array.items.as_deref());
        let inner = match items {
            Some(item) => self.type_for(source, item),
            None => RustType::Value,
        };
        collection_of(node, inner)
    }

    fn map_type(&mut self, source: &SchemaId, node: &SchemaNode) -> RustType {
        let settings = self.settings;
        let value = match node
            .object
            .as_ref()
            .and_then(|object| object.additional_properties.as_deref())
        {
            Some(schema) if !schema.bool_true => self.type_for(source, schema),
            _ => RustType::Value,
        };
        settings.map.rust_type(RustType::String, value)
    }

    fn object_type(&mut self, source: &SchemaId, node: &SchemaNode) -> RustType {
        let has_properties = node
            .object
            .as_ref()
            .is_some_and(|object| !object.properties.is_empty());
        let free_form_map = node
            .object
            .as_ref()
            .and_then(|object| object.additional_properties.as_deref())
            .is_some_and(|schema| !schema.bool_false);
        if !has_properties && free_form_map {
            return self.map_type(source, node);
        }
        if node.metadata.title.is_some() {
            let name = self.type_name(node);
            self.discover(node);
            return RustType::Named(name);
        }
        RustType::Value
    }

    /// Builds an untagged enum for a general `oneOf`/`anyOf`, returning the
    /// type that refers to it.
    pub(super) fn union_type(
        &mut self,
        source: &SchemaId,
        field: &SchemaNode,
        name: &str,
    ) -> Option<(RustType, UnionDef)> {
        let branches = if !field.combinators.one_of.is_empty() {
            &field.combinators.one_of
        } else if !field.combinators.any_of.is_empty() {
            &field.combinators.any_of
        } else {
            return None;
        };
        if branches.len() < 2
            || (branches.len() == 2
                && branches
                    .iter()
                    .any(|branch| branch.types.only() == Some("null")))
            || open_primitive_union(branches, self.settings).is_some()
        {
            return None;
        }
        let mut variants: Vec<UnionVariantDef> = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let ty = self.type_for(source, branch);
            if variants.iter().any(|variant| variant.ty == ty) {
                continue;
            }
            let base_name = match &ty {
                RustType::Named(name) => name.clone(),
                RustType::Array(_, length) => format!("Array{length}"),
                RustType::Vec(_) => "Array".to_string(),
                RustType::String | RustType::BoxedStr => "String".to_string(),
                RustType::Bool => "Boolean".to_string(),
                RustType::I8
                | RustType::I16
                | RustType::I32
                | RustType::I64
                | RustType::U8
                | RustType::U16
                | RustType::U32
                | RustType::U64
                | RustType::Usize => "Integer".to_string(),
                RustType::F32 | RustType::F64 => "Number".to_string(),
                _ => format!("Variant{}", index + 1),
            };
            let mut variant_name = base_name.clone();
            let mut suffix = 2;
            while variants.iter().any(|variant| variant.name == variant_name) {
                variant_name = format!("{base_name}{suffix}");
                suffix += 1;
            }
            variants.push(UnionVariantDef {
                name: variant_name,
                ty,
            });
        }
        let definition = UnionDef {
            name: name.to_string(),
            description: field.metadata.description.clone(),
            variants,
        };
        Some((RustType::Named(name.to_string()), definition))
    }
}

/// Wraps `inner` in the collection type `schema` describes, preserving the
/// fixed-length array form when the schema pins both bounds to the same value.
pub(super) fn collection_of(schema: &SchemaNode, inner: RustType) -> RustType {
    if let Some(array) = &schema.array
        && let (Some(min), Some(max)) = (array.min_items, array.max_items)
        && min == max
        && min <= MAX_FIXED_ARRAY
    {
        return RustType::Array(Box::new(inner), min as usize);
    }
    RustType::Vec(Box::new(inner))
}

/// A union of scalar types declared on a schema node (`type: [string,
/// integer]`), as an untagged enum.
pub(super) fn scalar_union(
    name: &str,
    node: &SchemaNode,
    settings: TypeSettings,
) -> Option<UnionDef> {
    let variants = [
        (node.types.boolean, "Boolean", RustType::Bool),
        (node.types.integer, "Integer", integer_type(node, settings)),
        (node.types.number, "Number", settings.number.rust_type()),
        (node.types.string, "String", settings.string.rust_type()),
    ]
    .into_iter()
    .filter_map(|(present, name, ty)| {
        present.then(|| UnionVariantDef {
            name: name.to_string(),
            ty,
        })
    })
    .collect::<Vec<_>>();
    (variants.len() > 1).then(|| UnionDef {
        name: name.to_string(),
        description: node.metadata.description.clone(),
        variants,
    })
}

pub(super) fn integer_type(node: &SchemaNode, settings: TypeSettings) -> RustType {
    let constraints = node.number.as_ref();
    let non_negative = constraints.is_some_and(|constraints| {
        constraints.minimum.is_some_and(|value| value >= 0.0)
            || constraints
                .exclusive_minimum
                .is_some_and(|value| value >= 0.0)
    });
    // A schema that provably needs more than 32 bits keeps 64, regardless of
    // the configured default.
    let needs_64 = constraints.is_some_and(|constraints| {
        constraints
            .maximum
            .is_some_and(|value| value > u32::MAX as f64)
            || constraints
                .minimum
                .is_some_and(|value| value < i32::MIN as f64)
    });
    // Conversely, a schema that states a bound within 32 bits is narrowed to
    // 32 bits even when the consumer's default is wider: the schema is the
    // more specific statement about the value, so it wins.
    let fits_32 = !needs_64
        && constraints.is_some_and(|constraints| {
            constraints
                .maximum
                .is_some_and(|value| value <= u32::MAX as f64)
        });

    if non_negative {
        if needs_64 {
            RustType::U64
        } else if fits_32 {
            RustType::U32
        } else {
            settings.integer.unsigned()
        }
    } else if needs_64 {
        RustType::I64
    } else if fits_32
        && constraints.is_some_and(|c| c.maximum.is_some_and(|v| v <= i32::MAX as f64))
    {
        RustType::I32
    } else {
        settings.integer.signed()
    }
}

/// The primitive a single `const`/`enum` node holds, for when it is not worth
/// an enum of its own.
fn primitive_type_for_enum(node: &SchemaNode, settings: TypeSettings) -> RustType {
    let value = node
        .const_value
        .as_ref()
        .or_else(|| node.enum_values.as_ref().and_then(|values| values.first()));
    match value {
        Some(serde_json::Value::String(_)) => settings.string.rust_type(),
        Some(serde_json::Value::Bool(_)) => RustType::Bool,
        Some(serde_json::Value::Number(number)) if number.is_u64() => settings.integer.unsigned(),
        Some(serde_json::Value::Number(number)) if number.is_i64() => settings.integer.signed(),
        Some(serde_json::Value::Number(_)) => settings.number.rust_type(),
        _ => RustType::Value,
    }
}

/// The primitive a `oneOf`/`anyOf` of named constants plus one open primitive
/// branch collapses to when open enums are not being inferred.
pub(super) fn open_union_primitive(node: &SchemaNode, settings: TypeSettings) -> Option<RustType> {
    let branches = if !node.combinators.any_of.is_empty() {
        &node.combinators.any_of
    } else {
        &node.combinators.one_of
    };
    (!branches.is_empty())
        .then(|| open_primitive_union(branches, settings))
        .flatten()
}

pub(super) fn open_primitive_union(
    branches: &[SchemaNode],
    settings: TypeSettings,
) -> Option<RustType> {
    let mut primitive = None;
    for branch in branches {
        if branch.const_value.is_some() || branch.enum_values.is_some() {
            continue;
        }
        let candidate = match branch.types.only()? {
            "string" => settings.string.rust_type(),
            "integer" => integer_type(branch, settings),
            "number" => settings.number.rust_type(),
            "boolean" => RustType::Bool,
            _ => return None,
        };
        if primitive.is_some() {
            return None;
        }
        primitive = Some(candidate);
    }
    primitive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Location;
    use crate::ir::{NumberConstraints, TypeSet};
    use crate::settings::IntegerWidth;

    fn integer(constraints: NumberConstraints) -> SchemaNode {
        let mut node = SchemaNode::any(Location::new("memory.json"));
        node.types = TypeSet::single("integer");
        node.number = Some(constraints);
        node
    }

    #[test]
    fn non_negative_exclusive_integer_uses_unsigned_type() {
        let node = integer(NumberConstraints {
            exclusive_minimum: Some(0.0),
            ..Default::default()
        });
        assert_eq!(integer_type(&node, TypeSettings::default()), RustType::U64);
        let narrow = TypeSettings::default().with_integer(IntegerWidth::Bits32);
        assert_eq!(integer_type(&node, narrow), RustType::U32);
    }

    #[test]
    fn schema_bounds_beat_the_configured_width() {
        let narrow = TypeSettings::default();
        let bounded = integer(NumberConstraints {
            minimum: Some(0.0),
            maximum: Some(255.0),
            ..Default::default()
        });
        assert_eq!(integer_type(&bounded, narrow), RustType::U32);
        let wide = integer(NumberConstraints {
            minimum: Some(0.0),
            maximum: Some(1e12),
            ..Default::default()
        });
        let bits32 = narrow.with_integer(IntegerWidth::Bits32);
        assert_eq!(integer_type(&wide, bits32), RustType::U64);
    }

    #[test]
    fn open_primitive_branches_follow_the_settings() {
        let mut fallback = SchemaNode::any(Location::new("m.json"));
        fallback.types = TypeSet::single("string");
        let mut constant = SchemaNode::any(Location::new("m.json"));
        constant.const_value = Some("a".into());
        let branches = [constant, fallback];
        assert_eq!(
            open_primitive_union(&branches, TypeSettings::compact()),
            Some(RustType::BoxedStr)
        );
        assert_eq!(
            open_primitive_union(&branches, TypeSettings::default()),
            Some(RustType::String)
        );
    }
}
