//! Schema IR to Rust type resolution.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::path::PathBuf;

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::config::Config;
use crate::diagnostics::{Level, Sink};
use crate::graph::{Graph, SchemaId};
use crate::ir::SchemaNode;
use crate::policy::GenerationPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustType {
    Bool,
    String,
    I64,
    U64,
    Usize,
    F64,
    Value,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    Array(Box<RustType>, usize),
    Map(Box<RustType>, Box<RustType>),
    Named(String),
}

/// A generated enum discovered from a finite schema value set.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub description: Option<String>,
    pub variants: Vec<EnumVariantDef>,
}

#[derive(Debug, Clone)]
pub struct EnumVariantDef {
    pub name: String,
    pub value: serde_json::Value,
}

impl fmt::Display for RustType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("bool"),
            Self::String => f.write_str("String"),
            Self::I64 => f.write_str("i64"),
            Self::U64 => f.write_str("u64"),
            Self::Usize => f.write_str("usize"),
            Self::F64 => f.write_str("f64"),
            Self::Value => f.write_str("serde_json::Value"),
            Self::Option(inner) => write!(f, "Option<{inner}>"),
            Self::Vec(inner) => write!(f, "Vec<{inner}>"),
            Self::Array(inner, len) => write!(f, "[{inner}; {len}]"),
            Self::Map(key, value) => write!(f, "std::collections::HashMap<{key}, {value}>"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

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

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub is_object: bool,
    pub alias: Option<RustType>,
    pub fields: Vec<FieldDef>,
    pub extra_fields: Vec<ExtraFieldDef>,
    pub extensions: bool,
    pub enums: Vec<EnumDef>,
    pub unions: Vec<UnionDef>,
}

#[derive(Debug, Clone)]
pub struct ExtraFieldDef {
    pub name: String,
    pub rust_type: String,
    pub skip_serde: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UnionDef {
    pub name: String,
    pub description: Option<String>,
    pub variants: Vec<UnionVariantDef>,
}

#[derive(Debug, Clone)]
pub struct UnionVariantDef {
    pub name: String,
    pub ty: RustType,
}

pub fn generate_types<P: GenerationPolicy>(
    graph: &mut Graph,
    root: SchemaId,
    config: &Config,
    policy: &P,
) -> Result<Vec<StructDef>, String> {
    generate_types_from_roots(graph, vec![root], config, policy)
}

pub fn generate_types_from_roots<P: GenerationPolicy>(
    graph: &mut Graph,
    roots: Vec<SchemaId>,
    config: &Config,
    policy: &P,
) -> Result<Vec<StructDef>, String> {
    let mut queue = VecDeque::new();
    for root in graph.reachable_documents(&roots) {
        let root_node = graph
            .root(&root)
            .ok_or_else(|| "root schema is not loaded".to_string())?;
        queue.push_back((root, root_node));
    }
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    let mut sink = Sink::new();
    while let Some((source, node)) = queue.pop_front() {
        let title = node
            .metadata
            .title
            .clone()
            .unwrap_or_else(|| "GeneratedType".into());
        let name = policy
            .type_name(&title, &node)
            .unwrap_or_else(|| class_name(graph, config, &node, &title));
        if !seen.insert(name.clone()) {
            continue;
        }
        if config
            .classes
            .iter()
            .any(|(key, class)| key.eq_ignore_ascii_case(&title) && class.skip)
        {
            continue;
        }
        let mut discovered = Vec::new();
        if !policy.should_generate(&title, &node) {
            continue;
        }
        let mut enums = Vec::new();
        let mut unions = Vec::new();
        let is_object = node.object.is_some() || node.reference.is_some();
        let root_primitive = (!is_object).then(|| open_union_primitive(&node)).flatten();
        let root_union = (!is_object && root_primitive.is_none())
            .then(|| {
                union_type(
                    graph,
                    &source,
                    &node,
                    &name,
                    config,
                    policy,
                    &mut discovered,
                    &mut sink,
                )
            })
            .flatten()
            .map(|(_, definition)| definition);
        let fields = fields_for(
            graph,
            &source,
            &node,
            config,
            policy,
            &mut discovered,
            &mut sink,
            &mut enums,
            &mut unions,
        );
        let alias = (!is_object).then(|| {
            root_primitive.clone().unwrap_or_else(|| {
                type_for(
                    graph,
                    &source,
                    &node,
                    config,
                    policy,
                    &mut discovered,
                    &mut sink,
                )
            })
        });
        let mut definition = StructDef {
            name: name.clone(),
            title: title.clone(),
            description: node
                .metadata
                .description
                .clone()
                .or_else(|| Some(title.clone())),
            is_object,
            alias,
            fields,
            extra_fields: Vec::new(),
            extensions: false,
            enums,
            unions,
        };
        if !is_object && let Some(union) = root_union.or_else(|| scalar_union(&name, &node)) {
            definition.alias = None;
            definition.unions.push(union);
        }
        if definition.is_object {
            policy.augment_struct(&mut definition);
        }
        output.push(definition);
        queue.extend(discovered);
    }
    if sink.has_errors() {
        return Err(sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Error)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"));
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(output)
}

fn fields_for(
    graph: &mut Graph,
    source: &SchemaId,
    node: &SchemaNode,
    config: &Config,
    policy: &impl GenerationPolicy,
    discovered: &mut Vec<(SchemaId, SchemaNode)>,
    sink: &mut Sink,
    enums: &mut Vec<EnumDef>,
    unions: &mut Vec<UnionDef>,
) -> Vec<FieldDef> {
    let Some(object) = node.object.as_ref() else {
        return Vec::new();
    };
    let mut properties = object.properties.clone();
    let mut required = object.required.clone();
    if let Some(reference) = &node.reference {
        if let Some(resolved) = graph.resolve(source, reference) {
            merge_object_branch(
                graph,
                source,
                &resolved,
                &mut properties,
                &mut required,
                sink,
            );
        } else {
            sink.error(node.location.clone(), "unable to resolve object $ref");
        }
    }
    for branch in &node.combinators.all_of {
        merge_object_branch(graph, source, branch, &mut properties, &mut required, sink);
    }
    properties.retain(|name, schema| !policy.skip_field(node, name, schema));
    let class = node.metadata.title.as_deref().and_then(|title| {
        config.classes.get(title).or_else(|| {
            config
                .classes
                .iter()
                .find_map(|(key, value)| key.eq_ignore_ascii_case(title).then_some(value))
        })
    });
    let mut fields = Vec::new();
    for (json_name, schema) in &properties {
        if policy.skip_field(node, json_name, schema) {
            continue;
        }
        let required = required.iter().any(|name| name == json_name);
        let ty = if let Some(policy_type) = policy.field_type(node, json_name, schema) {
            policy_type
        } else if let Some(override_type) = class.and_then(|c| c.property_overrides.get(json_name))
        {
            parse_type(override_type).unwrap_or_else(|| {
                sink.warn(
                    schema.location.clone(),
                    format!("invalid Rust type override for {json_name}"),
                );
                RustType::Value
            })
        } else if let Some(values) = finite_string_values(schema) {
            let enum_name = format!(
                "{}{}",
                owner_name(node, policy),
                json_name.to_upper_camel_case()
            );
            enums.push(EnumDef {
                name: enum_name.clone(),
                description: schema.metadata.description.clone(),
                variants: values
                    .iter()
                    .map(|value| EnumVariantDef {
                        name: schema
                            .metadata
                            .extensions
                            .get("x-rust-variant-names")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|names| names.get(value))
                            .and_then(serde_json::Value::as_str)
                            .map_or_else(|| value.to_upper_camel_case(), str::to_owned),
                        value: serde_json::Value::String(value.clone()),
                    })
                    .collect(),
            });
            RustType::Named(enum_name)
        } else if let Some(union_type) = union_type(
            graph,
            source,
            schema,
            &format!(
                "{}{}",
                owner_name(node, policy),
                json_name.to_upper_camel_case()
            ),
            config,
            policy,
            discovered,
            sink,
        ) {
            unions.push(union_type.1);
            union_type.0
        } else {
            type_for(graph, source, schema, config, policy, discovered, sink)
        };
        let is_collection = matches!(
            ty,
            RustType::Vec(_) | RustType::Array(_, _) | RustType::Map(_, _)
        );
        let has_default = schema.metadata.default.is_some()
            || class.is_some_and(|class| class.property_defaults.contains_key(json_name));
        let ty =
            if !required && !has_default && !is_collection && !matches!(ty, RustType::Option(_)) {
                RustType::Option(Box::new(ty))
            } else {
                ty
            };
        fields.push(FieldDef {
            json_name: json_name.clone(),
            rust_name: safe_name(json_name),
            ty,
            required,
            description: schema.metadata.description.clone(),
            default: schema.metadata.default.clone().or_else(|| {
                class.and_then(|class| class.property_defaults.get(json_name).cloned())
            }),
            skip_serializing_if: policy.skip_serializing_if(node, json_name, schema),
            flatten: false,
        });
    }
    if properties.is_empty()
        && let Some(value_schema) = object.additional_properties.as_deref()
        && !value_schema.bool_false
    {
        let value = type_for(
            graph,
            source,
            value_schema,
            config,
            policy,
            discovered,
            sink,
        );
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
    fields
}

fn owner_name(node: &SchemaNode, policy: &impl GenerationPolicy) -> String {
    policy
        .type_name(node.metadata.title.as_deref().unwrap_or("Generated"), node)
        .unwrap_or_else(|| {
            node.metadata
                .title
                .as_deref()
                .unwrap_or("Generated")
                .to_upper_camel_case()
        })
}

fn type_for(
    graph: &mut Graph,
    source: &SchemaId,
    node: &SchemaNode,
    config: &Config,
    policy: &impl GenerationPolicy,
    discovered: &mut Vec<(SchemaId, SchemaNode)>,
    sink: &mut Sink,
) -> RustType {
    if node.object.is_none()
        && node.reference.is_none()
        && node.combinators.all_of.len() == 1
        && node.types.only().is_none()
    {
        return type_for(
            graph,
            source,
            &node.combinators.all_of[0],
            config,
            policy,
            discovered,
            sink,
        );
    }
    if node.bool_true {
        return RustType::Value;
    }
    if node.bool_false {
        sink.error(
            node.location.clone(),
            "false schema cannot be represented as a Rust field",
        );
        return RustType::Value;
    }
    if let Some(reference) = &node.reference {
        if let Some(resolved) = graph.resolve(source, reference) {
            let title = resolved
                .metadata
                .title
                .clone()
                .unwrap_or_else(|| "GeneratedType".into());
            if let Some(policy_type) = policy.reference_type(&title, &resolved) {
                return policy_type;
            }
            if config.classes.get(&title).is_some_and(|class| class.skip) {
                return RustType::Value;
            }
            if let Some(name) = policy.type_name(&title, &resolved) {
                discovered.push((
                    SchemaId::new(PathBuf::from(resolved.location.file.clone())),
                    resolved.clone(),
                ));
                return RustType::Named(name);
            }
            if let Some(override_name) = config
                .classes
                .get(&title)
                .and_then(|c| c.override_name.clone())
            {
                discovered.push((
                    SchemaId::new(PathBuf::from(resolved.location.file.clone())),
                    resolved.clone(),
                ));
                return RustType::Named(override_name);
            }
            let name = policy
                .type_name(&title, &resolved)
                .unwrap_or_else(|| class_name(graph, config, &resolved, &title));
            discovered.push((
                SchemaId::new(PathBuf::from(resolved.location.file.clone())),
                resolved,
            ));
            return RustType::Named(name);
        }
        sink.error(node.location.clone(), "unable to resolve $ref");
        return RustType::Value;
    }
    if node.types.null && node.types.only().is_none() {
        let non_null = node.types.string
            || node.types.integer
            || node.types.number
            || node.types.boolean
            || node.types.object
            || node.types.array;
        if non_null {
            return RustType::Option(Box::new(RustType::Value));
        }
        return RustType::Value;
    }
    let union = if !node.combinators.any_of.is_empty() {
        &node.combinators.any_of
    } else {
        &node.combinators.one_of
    };
    if !union.is_empty() {
        if let Some(title) = &node.metadata.title {
            let name = policy
                .type_name(title, node)
                .unwrap_or_else(|| class_name(graph, config, node, title));
            discovered.push((
                SchemaId::new(PathBuf::from(node.location.file.clone())),
                node.clone(),
            ));
            return RustType::Named(name);
        }
        let non_null: Vec<_> = union
            .iter()
            .filter(|branch| !branch.types.null && !branch.bool_true)
            .collect();
        if union.len() == 2 && non_null.len() == 1 {
            let inner = type_for(graph, source, non_null[0], config, policy, discovered, sink);
            return RustType::Option(Box::new(inner));
        }
        if let Some(primitive) = open_primitive_union(union) {
            return primitive;
        }
        let message = "general union schema is represented as serde_json::Value";
        if policy.allow_lossy() {
            sink.warn(node.location.clone(), message);
        } else {
            sink.error(
                node.location.clone(),
                format!("{message}; enable lossy generation in the consumer policy to continue"),
            );
        }
        return RustType::Value;
    }
    if node.enum_values.is_some() || node.const_value.is_some() {
        return primitive_type_for_enum(node);
    }
    let base = match node.types.only() {
        Some("boolean") => RustType::Bool,
        Some("string") => RustType::String,
        Some("integer") => integer_type(node),
        Some("number") => RustType::F64,
        Some("array") => {
            let inner = node
                .array
                .as_ref()
                .and_then(|a| a.items.as_deref())
                .map(|item| type_for(graph, source, item, config, policy, discovered, sink))
                .unwrap_or(RustType::Value);
            if let (Some(min), Some(max)) = (
                node.array.as_ref().and_then(|a| a.min_items),
                node.array.as_ref().and_then(|a| a.max_items),
            ) {
                if min == max && min <= 32 {
                    RustType::Array(Box::new(inner), min as usize)
                } else {
                    RustType::Vec(Box::new(inner))
                }
            } else {
                RustType::Vec(Box::new(inner))
            }
        }
        Some("object") => {
            if let Some(object) = &node.object {
                if let Some(value_schema) = object
                    .additional_properties
                    .as_deref()
                    .filter(|schema| !schema.bool_false)
                {
                    let value = type_for(
                        graph,
                        source,
                        value_schema,
                        config,
                        policy,
                        discovered,
                        sink,
                    );
                    RustType::Map(Box::new(RustType::String), Box::new(value))
                } else {
                    object_type(node)
                }
            } else {
                object_type(node)
            }
        }
        Some("null") => RustType::Value,
        _ if node
            .object
            .as_ref()
            .and_then(|object| object.additional_properties.as_ref())
            .is_some() =>
        {
            let object = node.object.as_ref().unwrap();
            let value_schema = object.additional_properties.as_deref().unwrap();
            if value_schema.bool_true {
                RustType::Map(Box::new(RustType::String), Box::new(RustType::Value))
            } else {
                let value = type_for(
                    graph,
                    source,
                    value_schema,
                    config,
                    policy,
                    discovered,
                    sink,
                );
                RustType::Map(Box::new(RustType::String), Box::new(value))
            }
        }
        _ if node.object.is_some() => object_type(node),
        _ => RustType::Value,
    };
    if node.types.null {
        RustType::Option(Box::new(base))
    } else {
        base
    }
}

fn merge_object_branch(
    graph: &mut Graph,
    source: &SchemaId,
    branch: &SchemaNode,
    properties: &mut std::collections::BTreeMap<String, SchemaNode>,
    required: &mut Vec<String>,
    sink: &mut Sink,
) {
    if let Some(reference) = &branch.reference {
        if let Some(resolved) = graph.resolve(source, reference) {
            merge_object_branch(graph, source, &resolved, properties, required, sink);
            return;
        }
        sink.error(branch.location.clone(), "unable to resolve allOf reference");
        return;
    }
    if let Some(object) = &branch.object {
        for (name, schema) in &object.properties {
            if properties.contains_key(name) {
                sink.warn(
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
        merge_object_branch(graph, source, nested, properties, required, sink);
    }
}

fn object_type(node: &SchemaNode) -> RustType {
    node.metadata
        .title
        .as_ref()
        .map(|t| RustType::Named(t.to_upper_camel_case()))
        .unwrap_or(RustType::Value)
}

fn scalar_union(name: &str, node: &SchemaNode) -> Option<UnionDef> {
    let variants = [
        (node.types.boolean, "Boolean", RustType::Bool),
        (node.types.integer, "Integer", integer_type(node)),
        (node.types.number, "Number", RustType::F64),
        (node.types.string, "String", RustType::String),
    ]
    .into_iter()
    .filter_map(|(present, name, ty)| {
        present.then_some(UnionVariantDef {
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
fn integer_type(node: &SchemaNode) -> RustType {
    let non_negative = node.number.as_ref().is_some_and(|constraints| {
        constraints.minimum.is_some_and(|value| value >= 0.0)
            || constraints
                .exclusive_minimum
                .is_some_and(|value| value >= 0.0)
    });
    if non_negative {
        RustType::U64
    } else {
        RustType::I64
    }
}

fn primitive_type_for_enum(node: &SchemaNode) -> RustType {
    let value = node
        .const_value
        .as_ref()
        .or_else(|| node.enum_values.as_ref().and_then(|values| values.first()));
    match value {
        Some(serde_json::Value::String(_)) => RustType::String,
        Some(serde_json::Value::Bool(_)) => RustType::Bool,
        Some(serde_json::Value::Number(number)) if number.is_u64() => RustType::U64,
        Some(serde_json::Value::Number(_)) => RustType::I64,
        _ => RustType::Value,
    }
}

fn finite_string_values(node: &SchemaNode) -> Option<Vec<String>> {
    if let Some(values) = &node.enum_values {
        if values.iter().all(|value| value.is_string()) {
            return Some(
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect(),
            );
        }
    }
    let branches = if !node.combinators.any_of.is_empty() {
        &node.combinators.any_of
    } else if !node.combinators.one_of.is_empty() {
        &node.combinators.one_of
    } else {
        return None;
    };
    let values: Option<Vec<String>> = branches
        .iter()
        .map(|branch| branch.const_value.as_ref()?.as_str().map(str::to_string))
        .collect();
    values.filter(|values| !values.is_empty())
}

fn union_type<P: GenerationPolicy>(
    graph: &mut Graph,
    source: &SchemaId,
    field: &SchemaNode,
    name: &str,
    config: &Config,
    policy: &P,
    discovered: &mut Vec<(SchemaId, SchemaNode)>,
    sink: &mut Sink,
) -> Option<(RustType, UnionDef)> {
    let branches = if !field.combinators.one_of.is_empty() {
        &field.combinators.one_of
    } else if !field.combinators.any_of.is_empty() {
        &field.combinators.any_of
    } else {
        return None;
    };
    if branches.len() == 2
        && branches
            .iter()
            .any(|branch| branch.types.only() == Some("null"))
    {
        return None;
    }
    if open_primitive_union(branches).is_some() {
        return None;
    }
    if branches.len() < 2 {
        return None;
    }
    let union_name = name.to_upper_camel_case();
    let mut variants = Vec::new();
    for (index, branch) in branches.iter().enumerate() {
        let ty = type_for(graph, source, branch, config, policy, discovered, sink);
        if variants
            .iter()
            .any(|variant: &UnionVariantDef| variant.ty == ty)
        {
            continue;
        }
        let base_name = match &ty {
            RustType::Named(name) => name.clone(),
            RustType::Array(_, length) => format!("Array{length}"),
            RustType::Vec(_) => "Array".to_string(),
            RustType::String => "String".to_string(),
            RustType::Bool => "Boolean".to_string(),
            RustType::I64 | RustType::U64 | RustType::Usize => "Integer".to_string(),
            RustType::F64 => "Number".to_string(),
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
        name: union_name.clone(),
        description: field.metadata.description.clone(),
        variants,
    };
    Some((RustType::Named(union_name), definition))
}

fn open_union_primitive(node: &SchemaNode) -> Option<RustType> {
    let branches = if !node.combinators.any_of.is_empty() {
        &node.combinators.any_of
    } else {
        &node.combinators.one_of
    };
    (!branches.is_empty())
        .then(|| open_primitive_union(branches))
        .flatten()
}

fn open_primitive_union(branches: &[SchemaNode]) -> Option<RustType> {
    let mut primitive = None;
    for branch in branches {
        if branch.const_value.is_some() || branch.enum_values.is_some() {
            continue;
        }
        let candidate = match branch.types.only()? {
            "string" => RustType::String,
            "integer" => RustType::I64,
            "number" => RustType::F64,
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
fn class_name(graph: &Graph, config: &Config, node: &SchemaNode, title: &str) -> String {
    let configured = config
        .classes
        .get(title)
        .and_then(|c| c.override_name.clone());
    if let Some(name) = configured {
        return name;
    }
    let title_name = title.to_upper_camel_case();
    if graph.title_count(title) <= 1 {
        return (!title_name.is_empty())
            .then_some(title_name)
            .unwrap_or_else(|| "GeneratedType".into());
    }
    let file_name = node
        .location
        .file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Schema")
        .strip_suffix(".schema.json")
        .unwrap_or("Schema")
        .to_upper_camel_case();
    format!("{file_name}{title_name}")
}
fn safe_name(name: &str) -> String {
    let name = name.to_snake_case();
    if [
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
        "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final",
        "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ]
    .contains(&name.as_str())
    {
        return format!("r#{name}");
    } else {
        return name;
    }
}

pub fn parse_type(value: &str) -> Option<RustType> {
    match value {
        "bool" => Some(RustType::Bool),
        "String" => Some(RustType::String),
        "i64" | "i32" => Some(RustType::I64),
        "u8" | "u16" | "u32" | "u64" | "usize" => Some(RustType::U64),
        "f32" | "f64" => Some(RustType::F64),
        "serde_json::Value" => Some(RustType::Value),
        value if value.starts_with("Option<") && value.ends_with('>') => Some(RustType::Option(
            Box::new(parse_type(&value[7..value.len() - 1])?),
        )),
        value if value.starts_with("Vec<") && value.ends_with('>') => Some(RustType::Vec(
            Box::new(parse_type(&value[4..value.len() - 1])?),
        )),
        value if value.starts_with('[') && value.ends_with(']') => {
            let (inner, length) = value[1..value.len() - 1].rsplit_once(';')?;
            Some(RustType::Array(
                Box::new(parse_type(inner.trim())?),
                length.trim().parse().ok()?,
            ))
        }
        value if value.starts_with('[') && value.ends_with(']') => {
            let (inner, length) = value[1..value.len() - 1].rsplit_once(';')?;
            Some(RustType::Array(
                Box::new(parse_type(inner.trim())?),
                length.trim().parse().ok()?,
            ))
        }
        value if value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            Some(RustType::Named(value.into()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Location;
    use crate::ir::{NumberConstraints, TypeSet};

    #[test]
    fn non_negative_exclusive_integer_uses_unsigned_type() {
        let mut types = TypeSet::default();
        types.set("integer");
        let node = SchemaNode {
            location: Location::new("memory.json"),
            metadata: Default::default(),
            reference: None,
            types,
            object: None,
            array: None,
            number: Some(NumberConstraints {
                exclusive_minimum: Some(0.0),
                ..Default::default()
            }),
            string: None,
            combinators: Default::default(),
            enum_values: None,
            const_value: None,
            bool_true: false,
            bool_false: false,
        };
        assert_eq!(integer_type(&node), RustType::U64);
    }
}
