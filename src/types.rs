//! Schema IR to Rust type resolution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::config::Config;
use crate::diagnostics::{Level, Sink};
use crate::graph::{Graph, SchemaId};
use crate::ir::SchemaNode;
use crate::policy::GenerationPolicy;
use crate::settings::TypeSettings;

/// The state every type-resolution step needs.
///
/// Resolving a schema is a mutual recursion across a handful of functions,
/// each of which needs the same six things: the schema graph to resolve `$ref`
/// against, the consumer's config and policy, the primitive-mapping settings,
/// somewhere to record schemas discovered along the way, and somewhere to
/// report diagnostics. Threading those individually gave every function eight
/// or nine parameters, which made call sites unreadable and made adding a
/// seventh piece of state a change to every signature.
struct Context<'a, P: GenerationPolicy> {
    graph: &'a mut Graph,
    config: &'a Config,
    policy: &'a P,
    settings: TypeSettings,
    /// Schemas referenced by the one being resolved, to be generated in turn.
    discovered: Vec<(SchemaId, SchemaNode)>,
    sink: Sink,
}

impl<P: GenerationPolicy> Context<'_, P> {
    fn discover(&mut self, node: &SchemaNode) {
        self.discovered
            .push((SchemaId::new(node.location.file.clone()), node.clone()));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustType {
    Bool,
    String,
    /// An immutable `Box<str>`: exactly-sized on the heap and eight bytes
    /// narrower than a `String` once wrapped in an `Option`.
    BoxedStr,
    I64,
    U64,
    I32,
    U32,
    Usize,
    F64,
    F32,
    Value,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    /// A heap-indirected `Box<T>`, used to keep rarely-populated or large
    /// payloads out of the inline struct layout.
    Boxed(Box<RustType>),
    /// A `Box<[T]>`, a fixed-length heap slice with no spare-capacity field.
    BoxedSlice(Box<RustType>),
    /// A small-vector storing up to `N` elements inline, spilling to the heap
    /// beyond that.
    ///
    /// Schema arrays are frequently short and fixed-shape (a three-component
    /// vector, a four-component colour). Storing those inline removes a heap
    /// allocation per value entirely, which matters more than the extra inline
    /// bytes when the array count scales with document size.
    InlineVec(Box<RustType>, usize),
    Array(Box<RustType>, usize),
    Map(Box<RustType>, Box<RustType>),
    /// A sorted `Box<[(K, V)]>` with map-like accessors.
    ///
    /// Schema objects with homogeneous values are usually small and read-only
    /// once parsed, where a hash map costs far more inline and heap space than
    /// a contiguous sorted slice.
    SortedMap(Box<RustType>, Box<RustType>),
    Named(String),
}

impl RustType {
    /// Rewrites `Named` types according to `renames`, recursing through
    /// wrappers so nested occurrences are updated too.
    pub(crate) fn rename_named(&mut self, renames: &HashMap<String, String>) {
        match self {
            RustType::Named(name) => {
                if let Some(replacement) = renames.get(name.as_str()) {
                    *name = replacement.clone();
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
            Self::BoxedStr => f.write_str("Box<str>"),
            Self::I64 => f.write_str("i64"),
            Self::U64 => f.write_str("u64"),
            Self::I32 => f.write_str("i32"),
            Self::U32 => f.write_str("u32"),
            Self::Usize => f.write_str("usize"),
            Self::F64 => f.write_str("f64"),
            Self::F32 => f.write_str("f32"),
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
    config.validate()?;
    let mut queue = VecDeque::new();
    for root in graph.reachable_documents(&roots) {
        let root_node = graph
            .root(&root)
            .ok_or_else(|| "root schema is not loaded".to_string())?;
        queue.push_back((root, root_node));
    }
    let settings = policy.settings();
    let mut context = Context {
        graph,
        config,
        policy,
        settings,
        discovered: Vec::new(),
        sink: Sink::new(),
    };
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    while let Some((source, node)) = queue.pop_front() {
        let title = node
            .metadata
            .title
            .clone()
            .unwrap_or_else(|| "GeneratedType".into());
        let name = policy
            .type_name(&title, &node)
            .unwrap_or_else(|| class_name(context.graph, config, &node, &title));
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
        if !policy.should_generate(&title, &node) {
            continue;
        }
        context.discovered.clear();
        let mut enums = Vec::new();
        let mut unions = Vec::new();
        let is_object = node.object.is_some() || node.reference.is_some();
        let root_primitive = (!is_object).then(|| open_union_primitive(&node)).flatten();
        let root_union = (!is_object && root_primitive.is_none())
            .then(|| union_type(&mut context, &source, &node, &name))
            .flatten()
            .map(|(_, definition)| definition);
        let fields = fields_for(&mut context, &source, &node, &mut enums, &mut unions);
        let alias = (!is_object).then(|| {
            root_primitive
                .clone()
                .unwrap_or_else(|| type_for(&mut context, &source, &node))
        });
        let mut definition = StructDef {
            name: name.clone(),
            title: title.clone(),
            source_file: source
                .file
                .file_name()
                .map(|name| {
                    let name = name.to_string_lossy();
                    name.split_once(".schema.json")
                        .map(|(stem, _)| stem.to_string())
                        .unwrap_or_else(|| name.to_string())
                })
                .unwrap_or_default(),
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
        if !is_object
            && let Some(union) = root_union.or_else(|| scalar_union(&name, &node, settings))
        {
            definition.alias = None;
            definition.unions.push(union);
        }
        if definition.is_object {
            policy.augment_struct(&mut definition);
        }
        output.push(definition);
        queue.extend(context.discovered.drain(..));
    }
    if context.sink.has_errors() {
        return Err(context
            .sink
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Error)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"));
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    if settings.infer_open_enums {
        merge_open_enums(&mut output);
    }
    apply_enum_names(&mut output, config);
    shorten_enum_names(&mut output, config);
    Ok(output)
}

/// Applies explicit `enumNames` entries, keyed by `"Owner.property"`.
///
/// The property half is matched case-insensitively so that a config may spell
/// it as the schema does (`valueType`) rather than in the PascalCase form the
/// generator derives names from.
fn apply_enum_names(output: &mut [StructDef], config: &Config) {
    if config.enum_names.is_empty() {
        return;
    }
    let renames: HashMap<String, String> = output
        .iter()
        .flat_map(|definition| {
            definition.enums.iter().filter_map(|generated| {
                config
                    .enum_names
                    .iter()
                    .find(|(key, _)| {
                        key.split_once('.').is_some_and(|(owner, property)| {
                            owner == definition.name
                                && property.to_upper_camel_case() == generated.property
                        })
                    })
                    .map(|(_, chosen)| (generated.name.clone(), chosen.clone()))
            })
        })
        .collect();
    if renames.is_empty() {
        return;
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
/// Unifies open enums where one schema's value set extends another's.
///
/// An open value set already accepts values outside the listed constants, so a
/// schema that lists a superset of another's values is widening that same type
/// rather than describing a new one. Merging those keeps the union of the named
/// constants, so values that would otherwise land in the unknown case stay
/// first-class.
///
/// Merging requires a subset relationship, not just a shared property name.
/// `Camera.type` and `Accessor.type` share a name but have disjoint values, so
/// they are unrelated types that must stay separate. Closed sets never merge:
/// there the listed values are a validation boundary.
fn merge_open_enums(output: &mut [StructDef]) {
    let values = |generated: &EnumDef| -> Vec<serde_json::Value> {
        generated.variants.iter().map(|v| v.value.clone()).collect()
    };
    let mut groups: HashMap<String, Vec<EnumDef>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for definition in output.iter() {
        for generated in &definition.enums {
            if !generated.open {
                continue;
            }
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
                        if !existing.variants.iter().any(|v| v.value == variant.value) {
                            existing.variants.push(variant.clone());
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
            let bucket = groups.get(property)?;
            let [single] = bucket.as_slice() else {
                return None;
            };
            Some((property.clone(), single.clone()))
        })
        .collect();

    let renames: HashMap<String, String> = output
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .filter(|generated| generated.open)
        .filter_map(|generated| {
            let target = unified.get(&generated.property)?;
            (target.name != generated.name).then(|| (generated.name.clone(), target.name.clone()))
        })
        .collect();

    // Drop every per-owner copy of a merged property, then re-add the single
    // unified definition so it is emitted exactly once.
    for definition in output.iter_mut() {
        definition
            .enums
            .retain(|generated| !(generated.open && unified.contains_key(&generated.property)));
    }
    let relocated: Vec<EnumDef> = order
        .iter()
        .filter_map(|property| unified.get(property).cloned())
        .collect();
    if let Some(host) = output.first_mut() {
        host.enums.extend(relocated);
    }

    if renames.is_empty() {
        return;
    }
    apply_renames(output, &renames);
}

fn apply_renames(output: &mut [StructDef], renames: &HashMap<String, String>) {
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
            if let Some(short) = renames.get(&extra.rust_type) {
                extra.rust_type = short.clone();
            }
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

fn shorten_enum_names(output: &mut [StructDef], config: &Config) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for definition in output.iter() {
        for generated in &definition.enums {
            *counts.entry(generated.property.as_str()).or_default() += 1;
        }
    }
    // A short name is only safe if exactly one enum wants it and no struct,
    // alias, union, already-named enum, or explicitly configured name occupies
    // it. Enum names must be included: an enum whose generated name is already
    // the short form would otherwise be silently redefined by a different enum
    // shortening onto it.
    let taken: HashSet<&str> = output
        .iter()
        .map(|definition| definition.name.as_str())
        .chain(
            output
                .iter()
                .flat_map(|definition| definition.unions.iter().map(|u| u.name.as_str())),
        )
        .chain(
            output
                .iter()
                .flat_map(|definition| definition.enums.iter().map(|e| e.name.as_str())),
        )
        .chain(config.enum_names.values().map(String::as_str))
        .collect();
    // An enum named explicitly in config keeps that name.
    let pinned: HashSet<&str> = config.enum_names.values().map(String::as_str).collect();
    let renames: HashMap<String, String> = output
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .filter(|generated| {
            counts.get(generated.property.as_str()) == Some(&1)
                && !taken.contains(generated.property.as_str())
                && !pinned.contains(generated.name.as_str())
        })
        .map(|generated| (generated.name.clone(), generated.property.clone()))
        .collect();
    if renames.is_empty() {
        return;
    }
    apply_renames(output, &renames);
}

fn fields_for<P: GenerationPolicy>(
    context: &mut Context<'_, P>,
    source: &SchemaId,
    node: &SchemaNode,
    enums: &mut Vec<EnumDef>,
    unions: &mut Vec<UnionDef>,
) -> Vec<FieldDef> {
    let (config, policy, settings) = (context.config, context.policy, context.settings);
    let Some(object) = node.object.as_ref() else {
        return Vec::new();
    };
    let mut properties = object.properties.clone();
    let mut required = object.required.clone();
    if let Some(reference) = &node.reference {
        if let Some(resolved) = context.graph.resolve(source, reference) {
            merge_object_branch(
                context.graph,
                source,
                &resolved,
                &mut properties,
                &mut required,
                &mut context.sink,
            );
        } else {
            context
                .sink
                .error(node.location.clone(), "unable to resolve object $ref");
        }
    }
    for branch in &node.combinators.all_of {
        merge_object_branch(
            context.graph,
            source,
            branch,
            &mut properties,
            &mut required,
            &mut context.sink,
        );
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
                context.sink.warn(
                    schema.location.clone(),
                    format!("invalid Rust type override for {json_name}"),
                );
                RustType::Value
            })
        } else if let Some(finite) = finite_values(schema, settings.infer_open_enums) {
            let enum_name = format!(
                "{}{}",
                owner_name(node, policy),
                json_name.to_upper_camel_case()
            );
            push_enum_def(enums, enum_name.clone(), schema, finite, json_name);
            RustType::Named(enum_name)
        } else if let Some((finite, items)) = array_item_enum(schema, settings.infer_open_enums) {
            // An enumeration declared on `items` constrains each element, so the
            // named enum becomes the element type rather than the field type.
            let enum_name = format!(
                "{}{}",
                owner_name(node, policy),
                json_name.to_upper_camel_case()
            );
            push_enum_def(enums, enum_name.clone(), items, finite, json_name);
            collection_of(schema, RustType::Named(enum_name))
        } else if let Some(union_type) = union_type(
            context,
            source,
            schema,
            &format!(
                "{}{}",
                owner_name(node, policy),
                json_name.to_upper_camel_case()
            ),
        ) {
            unions.push(union_type.1);
            union_type.0
        } else {
            type_for(context, source, schema)
        };
        // Collections represent absence as emptiness, so wrapping them in an
        // `Option` would add a second way to say the same thing.
        let is_collection = matches!(
            ty,
            RustType::Vec(_)
                | RustType::BoxedSlice(_)
                | RustType::InlineVec(_, _)
                | RustType::Array(_, _)
                | RustType::Map(_, _)
                | RustType::SortedMap(_, _)
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
        let value = type_for(context, source, value_schema);
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

fn type_for<P: GenerationPolicy>(
    context: &mut Context<'_, P>,
    source: &SchemaId,
    node: &SchemaNode,
) -> RustType {
    let (config, policy, settings) = (context.config, context.policy, context.settings);
    if node.object.is_none()
        && node.reference.is_none()
        && node.combinators.all_of.len() == 1
        && node.types.only().is_none()
    {
        return type_for(context, source, &node.combinators.all_of[0]);
    }
    if node.bool_true {
        return RustType::Value;
    }
    if node.bool_false {
        context.sink.error(
            node.location.clone(),
            "false schema cannot be represented as a Rust field",
        );
        return RustType::Value;
    }
    if let Some(reference) = &node.reference {
        if let Some(resolved) = context.graph.resolve(source, reference) {
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
                context.discover(&resolved);
                return RustType::Named(name);
            }
            if let Some(override_name) = config
                .classes
                .get(&title)
                .and_then(|c| c.override_name.clone())
            {
                context.discover(&resolved);
                return RustType::Named(override_name);
            }
            let name = class_name(context.graph, config, &resolved, &title);
            context.discover(&resolved);
            return RustType::Named(name);
        }
        context
            .sink
            .error(node.location.clone(), "unable to resolve $ref");
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
                .unwrap_or_else(|| class_name(context.graph, config, node, title));
            context.discover(node);
            return RustType::Named(name);
        }
        let non_null: Vec<_> = union
            .iter()
            .filter(|branch| !branch.types.null && !branch.bool_true)
            .cloned()
            .collect();
        if union.len() == 2 && non_null.len() == 1 {
            let inner = type_for(context, source, &non_null[0]);
            return RustType::Option(Box::new(inner));
        }
        if let Some(primitive) = open_primitive_union(union) {
            return primitive;
        }
        let message = "general union schema is represented as serde_json::Value";
        if policy.allow_lossy() {
            context.sink.warn(node.location.clone(), message);
        } else {
            context.sink.error(
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
        Some("string") => settings.string.rust_type(),
        Some("integer") => integer_type(node, settings),
        Some("number") => settings.number.rust_type(),
        Some("array") => {
            let items = node
                .array
                .as_ref()
                .and_then(|a| a.items.as_deref())
                .cloned();
            let inner = items
                .map(|item| type_for(context, source, &item))
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
            let value_schema = node.object.as_ref().and_then(|object| {
                object
                    .additional_properties
                    .as_deref()
                    .filter(|schema| !schema.bool_false)
                    .cloned()
            });
            match value_schema {
                Some(value_schema) => {
                    let value = type_for(context, source, &value_schema);
                    settings.map.rust_type(RustType::String, value)
                }
                None => object_type(node),
            }
        }
        Some("null") => RustType::Value,
        _ if node
            .object
            .as_ref()
            .and_then(|object| object.additional_properties.as_ref())
            .is_some() =>
        {
            let value_schema = node
                .object
                .as_ref()
                .and_then(|object| object.additional_properties.as_deref())
                .expect("additional_properties present")
                .clone();
            if value_schema.bool_true {
                settings.map.rust_type(RustType::String, RustType::Value)
            } else {
                let value = type_for(context, source, &value_schema);
                settings.map.rust_type(RustType::String, value)
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

fn scalar_union(name: &str, node: &SchemaNode, settings: TypeSettings) -> Option<UnionDef> {
    let variants = [
        (node.types.boolean, "Boolean", RustType::Bool),
        (node.types.integer, "Integer", integer_type(node, settings)),
        (node.types.number, "Number", settings.number.rust_type()),
        (node.types.string, "String", settings.string.rust_type()),
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
fn integer_type(node: &SchemaNode, settings: TypeSettings) -> RustType {
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

/// Rust identifiers cannot begin with a digit, but schema enumerations
/// routinely do (`3dNodeIndexDocument`, `3DObject`). Spell the leading digit
/// out so such values still produce a legal, readable variant name.
fn spell_leading_digit(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return name.to_string();
    };
    let word = match first {
        '0' => "Zero",
        '1' => "One",
        '2' => "Two",
        '3' => "Three",
        '4' => "Four",
        '5' => "Five",
        '6' => "Six",
        '7' => "Seven",
        '8' => "Eight",
        '9' => "Nine",
        _ => return name.to_string(),
    };
    format!("{word}_{}", chars.as_str())
}

/// Records a named enum for `schema`, honouring the `x-rust-variant-names`
/// extension that lets schemas override variant spellings the generator would
/// otherwise derive from the literal values.
fn push_enum_def(
    enums: &mut Vec<EnumDef>,
    enum_name: String,
    schema: &SchemaNode,
    finite: FiniteValues,
    json_name: &str,
) {
    let rename = schema
        .metadata
        .extensions
        .get("x-rust-variant-names")
        .and_then(serde_json::Value::as_object);
    enums.push(EnumDef {
        name: enum_name,
        description: schema.metadata.description.clone(),
        variants: finite
            .variants
            .into_iter()
            .map(|mut variant| {
                if let Some(name) = rename
                    .and_then(|names| names.get(&variant.value.to_string()))
                    .or_else(|| {
                        rename
                            .zip(variant.value.as_str())
                            .and_then(|(n, k)| n.get(k))
                    })
                    .and_then(serde_json::Value::as_str)
                {
                    variant.name = name.to_owned();
                }
                variant
            })
            .collect(),
        numeric: finite.numeric,
        open: finite.open,
        property: json_name.to_upper_camel_case(),
    });
}

/// Detects an enumeration declared on an array's `items`, returning the values
/// alongside the item schema they came from.
///
/// Only plain arrays qualify: if the array itself carries an enum, or has no
/// item schema, the regular property-level handling applies instead.
fn array_item_enum(
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

/// Wraps `inner` in the collection type `schema` describes, preserving the
/// fixed-length array form when the schema pins both bounds to the same value.
fn collection_of(schema: &SchemaNode, inner: RustType) -> RustType {
    if let Some(array) = &schema.array
        && let (Some(min), Some(max)) = (array.min_items, array.max_items)
        && min == max
        && min <= 32
    {
        return RustType::Array(Box::new(inner), min as usize);
    }
    RustType::Vec(Box::new(inner))
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
/// (e.g. a bare `{"type": "integer"}`) to keep the property extensible. That
/// fallback is ignored here: the closed variants are what callers actually want
/// modelled, and keeping the property as a bare integer forces every consumer to
/// re-derive the mapping by hand.
fn finite_values(node: &SchemaNode, allow_open_fallback: bool) -> Option<FiniteValues> {
    fn variant_for(value: &serde_json::Value, description: Option<&str>) -> Option<EnumVariantDef> {
        // Prefer the branch's description as the variant name. Schemas that use
        // numeric consts have no other human-readable handle, and a description
        // like "UNSIGNED_BYTE" yields a far better name than "Value5121".
        let name = match description.map(str::trim).filter(|d| is_identifier_like(d)) {
            Some(description) => spell_leading_digit(description).to_upper_camel_case(),
            None => match value {
                serde_json::Value::String(value) => {
                    spell_leading_digit(value).to_upper_camel_case()
                }
                serde_json::Value::Number(number) => format!("Value{number}"),
                _ => return None,
            },
        };
        Some(EnumVariantDef {
            name,
            value: value.clone(),
        })
    }

    let from_values = |values: &Vec<serde_json::Value>| -> Option<FiniteValues> {
        let numeric = values.iter().all(serde_json::Value::is_number);
        if !numeric && !values.iter().all(serde_json::Value::is_string) {
            return None;
        }
        let variants: Option<Vec<_>> = values.iter().map(|v| variant_for(v, None)).collect();
        Some(FiniteValues {
            variants: variants?,
            numeric,
            open: false,
        })
    };

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

    let mut variants = Vec::new();
    let mut numeric = true;
    let mut saw_string = false;
    let mut open = false;
    for branch in branches {
        let Some(value) = &branch.const_value else {
            // An open fallback branch. Only tolerated when the consumer opts
            // in, and even then it must be a plain primitive: anything richer
            // means this is a real union, not an enum, and must not collapse.
            if !allow_open_fallback
                || branch.enum_values.is_some()
                || branch.combinators.any_of.len() + branch.combinators.one_of.len() > 0
                || branch.reference.is_some()
            {
                return None;
            }
            open = true;
            continue;
        };
        match value {
            serde_json::Value::Number(_) => {}
            serde_json::Value::String(_) => {
                numeric = false;
                saw_string = true;
            }
            _ => return None,
        }
        variants.push(variant_for(value, branch.metadata.description.as_deref())?);
    }
    if variants.is_empty() || (numeric && saw_string) {
        return None;
    }
    // Deduplicate by value, keeping the first name.
    variants.dedup_by(|a, b| a.value == b.value);
    Some(FiniteValues {
        variants,
        numeric,
        open,
    })
}

/// True when `text` looks like a symbolic constant name (e.g. `UNSIGNED_BYTE`)
/// rather than prose, and is therefore a good source for a variant name.
fn is_identifier_like(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && text.starts_with(|c: char| c.is_ascii_alphabetic())
}

fn union_type<P: GenerationPolicy>(
    context: &mut Context<'_, P>,
    source: &SchemaId,
    field: &SchemaNode,
    name: &str,
) -> Option<(RustType, UnionDef)> {
    let branches = if !field.combinators.one_of.is_empty() {
        field.combinators.one_of.clone()
    } else if !field.combinators.any_of.is_empty() {
        field.combinators.any_of.clone()
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
    if open_primitive_union(&branches).is_some() {
        return None;
    }
    if branches.len() < 2 {
        return None;
    }
    let union_name = name.to_upper_camel_case();
    let mut variants = Vec::new();
    for (index, branch) in branches.iter().enumerate() {
        let ty = type_for(context, source, branch);
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
            RustType::String | RustType::BoxedStr => "String".to_string(),
            RustType::Bool => "Boolean".to_string(),
            RustType::I64 | RustType::U64 | RustType::Usize | RustType::I32 | RustType::U32 => {
                "Integer".to_string()
            }
            RustType::F64 | RustType::F32 => "Number".to_string(),
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
        return if !title_name.is_empty() {
            title_name
        } else {
            "GeneratedType".into()
        };
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
        format!("r#{name}")
    } else {
        name
    }
}

pub fn parse_type(value: &str) -> Option<RustType> {
    match value {
        "bool" => Some(RustType::Bool),
        "String" => Some(RustType::String),
        "Box<str>" => Some(RustType::BoxedStr),
        "i64" => Some(RustType::I64),
        "i8" | "i16" | "i32" => Some(RustType::I32),
        "u8" | "u16" | "u32" => Some(RustType::U32),
        "u64" | "usize" => Some(RustType::U64),
        "f64" => Some(RustType::F64),
        "f32" => Some(RustType::F32),
        "serde_json::Value" => Some(RustType::Value),
        value if value.starts_with("Option<") && value.ends_with('>') => Some(RustType::Option(
            Box::new(parse_type(&value[7..value.len() - 1])?),
        )),
        value if value.starts_with("Vec<") && value.ends_with('>') => Some(RustType::Vec(
            Box::new(parse_type(&value[4..value.len() - 1])?),
        )),
        value if value.starts_with("Box<[") && value.ends_with("]>") => Some(RustType::BoxedSlice(
            Box::new(parse_type(&value[5..value.len() - 2])?),
        )),
        value if value.starts_with("Box<") && value.ends_with('>') => Some(RustType::Boxed(
            Box::new(parse_type(&value[4..value.len() - 1])?),
        )),
        value if value.starts_with('[') && value.ends_with(']') => {
            let (inner, length) = value[1..value.len() - 1].rsplit_once(';')?;
            Some(RustType::Array(
                Box::new(parse_type(inner.trim())?),
                length.trim().parse().ok()?,
            ))
        }
        value if value.starts_with("InlineVec<") && value.ends_with('>') => {
            let (inner, capacity) = value[10..value.len() - 1].rsplit_once(',')?;
            let capacity: usize = capacity.trim().parse().ok()?;
            // The generated type stores its length in a `u8`, and an inline
            // capacity anywhere near that bound has long since stopped being an
            // optimization.
            if capacity == 0 || capacity > u8::MAX as usize {
                return None;
            }
            Some(RustType::InlineVec(
                Box::new(parse_type(inner.trim())?),
                capacity,
            ))
        }
        value if value.starts_with("SortedMap<") && value.ends_with('>') => {
            let (key, val) = value[10..value.len() - 1].split_once(',')?;
            Some(RustType::SortedMap(
                Box::new(parse_type(key.trim())?),
                Box::new(parse_type(val.trim())?),
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
        assert_eq!(integer_type(&node, TypeSettings::default()), RustType::U64);
        assert_eq!(
            integer_type(
                &node,
                TypeSettings::default().with_integer(crate::settings::IntegerWidth::Bits32)
            ),
            RustType::U32
        );
    }
}
