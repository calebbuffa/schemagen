//! Enumerations discovered from finite value sets.

use std::collections::{HashMap, HashSet};

use heck::ToUpperCamelCase;
use serde_json::Value;

use super::Resolver;
use super::names::{check_ident, upper_camel};
use crate::diagnostics::Location;
use crate::ir::SchemaNode;
use crate::model::{EnumDef, EnumVariantDef, StructDef};
use crate::policy::GenerationPolicy;

/// A closed set of literal values discovered on a schema node, suitable for
/// generating a Rust enum.
pub(crate) struct FiniteValues {
    pub variants: Vec<EnumVariantDef>,
    /// True when the values are numeric, which requires explicit discriminants
    /// and integer-based (rather than string-based) serde handling.
    pub numeric: bool,
    /// True when the schema also permitted an open primitive fallback, meaning
    /// values outside the listed set are legal and must not be rejected.
    pub open: bool,
}

/// Discovers a closed set of literal values on `node`.
///
/// Recognises both the `enum` keyword and the `anyOf`/`oneOf`-of-`const` idiom.
/// Numeric and string values are both supported.
///
/// Schemas frequently pair the `const` branches with one open fallback branch
/// (for example a bare `{"type": "integer"}`) to keep the property extensible.
/// That fallback is only honoured when `allow_open_fallback` is set; the
/// resulting enum is then open, so values outside the set round-trip.
pub(crate) fn finite_values(node: &SchemaNode, allow_open_fallback: bool) -> Option<FiniteValues> {
    if let Some(values) = &node.enum_values
        && let Some(finite) = from_values(values)
    {
        return (!finite.variants.is_empty()).then_some(finite);
    }

    let branches = if !node.combinators.any_of.is_empty() {
        &node.combinators.any_of
    } else if !node.combinators.one_of.is_empty() {
        &node.combinators.one_of
    } else {
        return None;
    };

    let mut variants: Vec<EnumVariantDef> = Vec::new();
    let mut saw_number = false;
    let mut saw_string = false;
    let mut open = false;
    for branch in branches {
        let Some(value) = &branch.const_value else {
            // An open fallback branch. Even when tolerated it must be a plain
            // primitive: anything richer means this is a real union, not an
            // enum, and must not collapse.
            if !allow_open_fallback
                || branch.enum_values.is_some()
                || !branch.combinators.any_of.is_empty()
                || !branch.combinators.one_of.is_empty()
                || branch.reference.is_some()
            {
                return None;
            }
            open = true;
            continue;
        };
        match value {
            Value::Number(_) => saw_number = true,
            Value::String(_) => saw_string = true,
            _ => return None,
        }
        let variant = variant_for(value, branch.metadata.description.as_deref())?;
        if !variants
            .iter()
            .any(|existing| existing.value == variant.value)
        {
            variants.push(variant);
        }
    }
    if variants.is_empty() || (saw_number && saw_string) {
        return None;
    }
    Some(FiniteValues {
        variants,
        numeric: saw_number,
        open,
    })
}

fn from_values(values: &[Value]) -> Option<FiniteValues> {
    let numeric = values.iter().all(Value::is_number);
    if !numeric && !values.iter().all(Value::is_string) {
        return None;
    }
    let mut variants: Vec<EnumVariantDef> = Vec::new();
    for value in values {
        let variant = variant_for(value, None)?;
        if !variants
            .iter()
            .any(|existing| existing.value == variant.value)
        {
            variants.push(variant);
        }
    }
    Some(FiniteValues {
        variants,
        numeric,
        open: false,
    })
}

fn variant_for(value: &Value, description: Option<&str>) -> Option<EnumVariantDef> {
    // Prefer the branch's description as the variant name. Schemas that use
    // numeric consts have no other human-readable handle, and a description
    // like "UNSIGNED_BYTE" yields a far better name than "Value5121".
    let name = match description.map(str::trim).filter(|d| is_identifier_like(d)) {
        Some(description) => upper_camel(description),
        None => match value {
            Value::String(value) => upper_camel(value),
            Value::Number(number) => format!("Value{}", number.to_string().replace('-', "Neg")),
            _ => return None,
        },
    };
    Some(EnumVariantDef {
        name,
        value: value.clone(),
        aliases: Vec::new(),
    })
}

/// True when `text` looks like a symbolic constant name (such as
/// `UNSIGNED_BYTE`) rather than prose, and is therefore a good source for a
/// variant name.
fn is_identifier_like(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && text.starts_with(|c: char| c.is_ascii_alphabetic())
}

/// Detects an enumeration declared on an array's `items`, returning the values
/// alongside the item schema they came from.
///
/// Only plain arrays qualify: if the array itself carries an enum, or has no
/// item schema, the regular property-level handling applies instead.
pub(crate) fn array_item_enum(
    schema: &SchemaNode,
    infer_open_enums: bool,
) -> Option<(FiniteValues, &SchemaNode)> {
    if schema.types.only() != Some("array") || schema.enum_values.is_some() {
        return None;
    }
    let items = schema.array.as_ref()?.items.as_deref()?;
    let finite = finite_values(items, infer_open_enums)?;
    Some((finite, items))
}

impl<P: GenerationPolicy + ?Sized> Resolver<'_, P> {
    /// Records a named enum for `schema` on `owner`'s property `json_name` and
    /// returns its name.
    ///
    /// Variant spellings come from the `x-rust-variant-names` extension when
    /// the schema carries one, otherwise from the values themselves.
    pub(super) fn push_enum(
        &mut self,
        enums: &mut Vec<EnumDef>,
        owner: &str,
        json_name: &str,
        schema: &SchemaNode,
        finite: FiniteValues,
    ) -> String {
        let property = json_name.to_upper_camel_case();
        let name = self
            .pinned_enum_name(owner, &property)
            .unwrap_or_else(|| format!("{owner}{property}"));
        let renames = schema
            .metadata
            .extensions
            .get("x-rust-variant-names")
            .and_then(Value::as_object);
        let mut variants = finite.variants;
        for variant in &mut variants {
            if let Some(chosen) = renames
                .and_then(|names| {
                    names
                        .get(&variant.value.to_string())
                        .or_else(|| variant.value.as_str().and_then(|text| names.get(text)))
                })
                .and_then(Value::as_str)
            {
                variant.name = chosen.to_owned();
            }
            if let Some(text) = variant.value.as_str() {
                variant.aliases = self.aliases_for(owner, json_name, text);
            }
        }
        self.check_variants(&name, &variants, finite.numeric, &schema.location);
        enums.push(EnumDef {
            name: name.clone(),
            description: schema.metadata.description.clone(),
            variants,
            numeric: finite.numeric,
            open: finite.open,
            property,
            default: None,
        });
        name
    }

    fn pinned_enum_name(&mut self, owner: &str, property: &str) -> Option<String> {
        let (key, chosen) = self.config.enum_names.iter().find(|(key, _)| {
            key.split_once('.')
                .is_some_and(|(key_owner, key_property)| {
                    key_owner == owner && key_property.to_upper_camel_case() == property
                })
        })?;
        self.used.enum_names.insert(key.clone());
        self.pinned.insert(chosen.clone());
        Some(chosen.clone())
    }

    /// The accepted alternative spellings of one enum value: the config's
    /// entries for the owner, then its wildcard entries, then the policy's.
    fn aliases_for(&mut self, owner: &str, property: &str, value: &str) -> Vec<String> {
        let mut aliases = Vec::new();
        for key in [format!("{owner}.{property}"), format!("*.{property}")] {
            if let Some(spellings) = self
                .config
                .enum_aliases
                .get(&key)
                .and_then(|by_value| by_value.get(value))
            {
                self.used.enum_aliases.insert(key);
                aliases.extend(spellings.iter().cloned());
            }
        }
        aliases.extend(self.policy.variant_aliases(owner, property, value));
        let mut unique = Vec::new();
        for alias in aliases {
            if alias != value && !unique.contains(&alias) {
                unique.push(alias);
            }
        }
        unique
    }

    fn check_variants(
        &mut self,
        enum_name: &str,
        variants: &[EnumVariantDef],
        numeric: bool,
        location: &Location,
    ) {
        let mut names: HashMap<&str, &Value> = HashMap::new();
        let mut spellings: HashSet<&str> = HashSet::new();
        for variant in variants {
            if let Err(error) =
                check_ident(&variant.name, "a variant name", &variant.value.to_string())
            {
                self.sink()
                    .error(location.clone(), format!("{enum_name}: {error}"));
            }
            if let Some(previous) = names.insert(&variant.name, &variant.value) {
                self.sink().error(
                    location.clone(),
                    format!(
                        "{enum_name}: values {previous} and {} both produce the variant `{}`; \
                         disambiguate with `x-rust-variant-names`",
                        variant.value, variant.name
                    ),
                );
            }
            if numeric && !variant.aliases.is_empty() {
                self.sink().warn(
                    location.clone(),
                    format!("{enum_name}: aliases only apply to string enums and are ignored"),
                );
            }
            if let Some(text) = variant.value.as_str() {
                spellings.insert(text);
            }
        }
        for variant in variants {
            for alias in &variant.aliases {
                if spellings.contains(alias.as_str()) {
                    self.sink().error(
                        location.clone(),
                        format!("{enum_name}: alias `{alias}` is also a value of another variant"),
                    );
                }
            }
        }
    }
}

/// Unifies open enums where one schema's value set extends another's.
///
/// An open value set already accepts values outside the listed constants, so a
/// schema that lists a superset of another's values is widening that same type
/// rather than describing a new one. Merging those keeps the union of the named
/// constants, so values that would otherwise land in the unknown case stay
/// first-class.
///
/// Merging requires a subset relationship, not just a shared property name:
/// `Camera.type` and `Accessor.type` share a name but have disjoint values, so
/// they are unrelated types that must stay separate. Closed sets never merge,
/// since there the listed values are a validation boundary, and neither do
/// enums pinned by name in the config.
pub(crate) fn merge_open_enums(output: &mut [StructDef], pinned: &HashSet<String>) {
    let values = |generated: &EnumDef| -> Vec<Value> {
        generated.variants.iter().map(|v| v.value.clone()).collect()
    };
    let mergeable = |generated: &EnumDef| generated.open && !pinned.contains(&generated.name);
    let mut groups: HashMap<String, Vec<EnumDef>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for definition in output.iter() {
        for generated in definition.enums.iter().filter(|e| mergeable(e)) {
            let bucket = groups.entry(generated.property.clone()).or_insert_with(|| {
                order.push(generated.property.clone());
                Vec::new()
            });
            let mine = values(generated);
            let related = bucket.iter_mut().find(|existing| {
                existing.numeric == generated.numeric && {
                    let theirs = values(existing);
                    mine.iter().all(|v| theirs.contains(v))
                        || theirs.iter().all(|v| mine.contains(v))
                }
            });
            match related {
                Some(existing) => {
                    for variant in &generated.variants {
                        match existing
                            .variants
                            .iter_mut()
                            .find(|v| v.value == variant.value)
                        {
                            Some(known) => {
                                for alias in &variant.aliases {
                                    if !known.aliases.contains(alias) {
                                        known.aliases.push(alias.clone());
                                    }
                                }
                            }
                            None => existing.variants.push(variant.clone()),
                        }
                    }
                }
                None => bucket.push(generated.clone()),
            }
        }
    }

    // Only properties that collapsed to a single definition were actually
    // merged; the rest keep their per-owner types untouched.
    let unified: HashMap<String, EnumDef> = order
        .iter()
        .filter_map(|property| {
            let [single] = groups.get(property)?.as_slice() else {
                return None;
            };
            Some((property.clone(), single.clone()))
        })
        .collect();

    let renames: HashMap<String, String> = output
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .filter(|generated| mergeable(generated))
        .filter_map(|generated| {
            let target = unified.get(&generated.property)?;
            (target.name != generated.name).then(|| (generated.name.clone(), target.name.clone()))
        })
        .collect();

    // Drop every per-owner copy of a merged property, then re-add the single
    // unified definition so it is emitted exactly once.
    for definition in output.iter_mut() {
        definition.enums.retain(|generated| {
            !(mergeable(generated) && unified.contains_key(&generated.property))
        });
    }
    let relocated: Vec<EnumDef> = order
        .iter()
        .filter_map(|property| unified.get(property).cloned())
        .collect();
    if let Some(host) = output.first_mut() {
        host.enums.extend(relocated);
    }
    apply_renames(output, &renames);
}

/// Renames generated enums from `OwnerProperty` to just `Property` where that
/// is unambiguous across the entire output.
///
/// Enum names are built by qualifying the property with its owning type so that
/// two schemas can both define a `type` property without colliding. In practice
/// most properties are unique, and the qualified name is redundant noise
/// (`AccessorComponentType` when `ComponentType` would do). This keeps the
/// qualified form only where it is actually load-bearing.
///
/// A short name is only used if exactly one enum wants it and no struct, union,
/// other enum or pinned name occupies it.
pub(crate) fn shorten_enum_names(
    output: &mut [StructDef],
    pinned: &HashSet<String>,
    reserved: &HashSet<String>,
) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for definition in output.iter() {
        for generated in &definition.enums {
            *counts.entry(generated.property.as_str()).or_default() += 1;
        }
    }
    let taken: HashSet<&str> = output
        .iter()
        .flat_map(|definition| {
            std::iter::once(definition.name.as_str())
                .chain(definition.unions.iter().map(|u| u.name.as_str()))
                .chain(definition.enums.iter().map(|e| e.name.as_str()))
        })
        .chain(pinned.iter().map(String::as_str))
        .chain(reserved.iter().map(String::as_str))
        .collect();
    let renames: HashMap<String, String> = output
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .filter(|generated| {
            counts.get(generated.property.as_str()) == Some(&1)
                && !taken.contains(generated.property.as_str())
                && !pinned.contains(&generated.name)
        })
        .map(|generated| (generated.name.clone(), generated.property.clone()))
        .collect();
    apply_renames(output, &renames);
}

fn apply_renames(output: &mut [StructDef], renames: &HashMap<String, String>) {
    if renames.is_empty() {
        return;
    }
    for definition in output.iter_mut() {
        for generated in &mut definition.enums {
            if let Some(short) = renames.get(&generated.name) {
                generated.name = short.clone();
            }
        }
        for field in &mut definition.fields {
            field.ty.rename_named(renames);
        }
        for extra in &mut definition.extra_fields {
            extra.ty.rename_named(renames);
        }
        if let Some(alias) = &mut definition.alias {
            alias.rename_named(renames);
        }
        for union in &mut definition.unions {
            for variant in &mut union.variants {
                variant.ty.rename_named(renames);
            }
        }
    }
}
