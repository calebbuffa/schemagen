//! Schema IR to Rust definitions.
//!
//! Resolution is a mutual recursion between a handful of functions that all
//! need the same state: the schema graph to resolve `$ref`s against, the
//! consumer's config and policy, the primitive settings, the schemas found so
//! far that still need generating, and a place to report diagnostics. That
//! state lives on [`Resolver`], and the recursion is split across its `impl`
//! blocks by concern:
//!
//! - `names`: legal, collision-free identifiers
//! - `types`: schema node to [`RustType`]
//! - `fields`: which properties a struct has and how each is typed
//! - `enums`: finite value sets, and unifying them across the output
//! - `custom`: types declared in configuration rather than a schema
//!
//! Every problem is reported through the graph's [`Sink`] and generation
//! carries on, so one run lists all of them rather than the first.

mod custom;
mod enums;
mod fields;
mod names;
mod types;

use std::collections::{BTreeSet, HashMap, HashSet};

use heck::ToUpperCamelCase;

use crate::config::{ClassConfig, Config, TypeNaming};
use crate::diagnostics::{Location, Sink};
use crate::error::{Error, Result};
use crate::graph::{Graph, SchemaId};
use crate::ir::SchemaNode;
use crate::model::{Generated, StructDef};
use crate::policy::GenerationPolicy;
use crate::settings::TypeSettings;
use names::{check_ident, file_stem, upper_camel};

/// Which configuration entries a generation run consulted.
///
/// Anything left over names something the schemas do not contain. With
/// several modules generated from one config, merge the usages before
/// reporting, since each module sees only its own schemas.
#[derive(Debug, Clone, Default)]
pub struct ConfigUsage {
    classes: BTreeSet<String>,
    field_types: BTreeSet<String>,
    enum_names: BTreeSet<String>,
    enum_aliases: BTreeSet<String>,
}

impl ConfigUsage {
    /// Folds another run's usage into this one.
    pub fn merge(&mut self, other: &ConfigUsage) {
        self.classes.extend(other.classes.iter().cloned());
        self.field_types.extend(other.field_types.iter().cloned());
        self.enum_names.extend(other.enum_names.iter().cloned());
        self.enum_aliases.extend(other.enum_aliases.iter().cloned());
    }

    /// Warns about every `config` entry that was never consulted.
    pub fn report_unused(&self, config: &Config, sink: &mut Sink) {
        let sections: [(&str, Vec<&String>, &BTreeSet<String>); 4] = [
            ("classes", config.classes.keys().collect(), &self.classes),
            (
                "fieldTypes",
                config.field_types.keys().collect(),
                &self.field_types,
            ),
            (
                "enumNames",
                config.enum_names.keys().collect(),
                &self.enum_names,
            ),
            (
                "enumAliases",
                config.enum_aliases.keys().collect(),
                &self.enum_aliases,
            ),
        ];
        for (section, keys, used) in sections {
            for key in keys.into_iter().filter(|key| !used.contains(*key)) {
                sink.warn(
                    Location::new("config").with_pointer(format!("/{section}/{key}")),
                    format!("`{key}` matches nothing in the schemas"),
                );
            }
        }
    }
}

/// Resolves everything reachable from `roots` into definitions.
///
/// Returns every problem found as [`Error::Diagnostics`] if resolution
/// produced any errors. Warnings are left on the graph's sink.
pub fn generate<P: GenerationPolicy + ?Sized>(
    graph: &mut Graph,
    roots: &[SchemaId],
    config: &Config,
    policy: &P,
) -> Result<(Generated, ConfigUsage)> {
    config.validate()?;
    let mut queue = std::collections::VecDeque::new();
    for root in graph.reachable_documents(roots) {
        let node = graph
            .root(&root)
            .ok_or_else(|| Error::SchemaNotFound(root.file.clone()))?;
        queue.push_back((root, node));
    }
    let mut resolver = Resolver {
        settings: policy.settings(),
        graph,
        config,
        policy,
        discovered: Vec::new(),
        used: ConfigUsage::default(),
        pinned: HashSet::new(),
    };
    let mut output = resolver.resolve_queue(queue);
    let mut generated = resolver.custom_types();
    resolver.finish(&mut output, &generated);
    generated.definitions = output;
    let usage = resolver.used;
    let errors = resolver_errors(graph);
    if errors.is_empty() {
        Ok((generated, usage))
    } else {
        Err(Error::Diagnostics(errors))
    }
}

fn resolver_errors(graph: &Graph) -> Vec<crate::diagnostics::Diagnostic> {
    graph.sink().errors()
}

/// The state every resolution step needs.
pub(crate) struct Resolver<'a, P: GenerationPolicy + ?Sized> {
    graph: &'a mut Graph,
    config: &'a Config,
    policy: &'a P,
    settings: TypeSettings,
    /// Schemas referenced by the one being resolved, to be generated in turn.
    discovered: Vec<(SchemaId, SchemaNode)>,
    used: ConfigUsage,
    /// Enum names the config fixed, which no pass may rename or merge away.
    pinned: HashSet<String>,
}

impl<P: GenerationPolicy + ?Sized> Resolver<'_, P> {
    fn sink(&mut self) -> &mut Sink {
        self.graph.sink_mut()
    }

    fn discover(&mut self, node: &SchemaNode) {
        let id = SchemaId::new(node.location.file.clone());
        self.discovered.push((id, node.clone()));
    }

    /// What a schema is called in config: its title, or for untitled schemas
    /// the last segment of its pointer, or failing that its file name.
    fn label(&self, node: &SchemaNode) -> String {
        if let Some(title) = &node.metadata.title {
            return title.clone();
        }
        match node
            .location
            .pointer
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
        {
            Some(last) if !last.is_empty() => last.to_string(),
            _ => file_stem(&node.location),
        }
    }

    /// The class configured for `label`, recording that it was consulted.
    fn class(&mut self, label: &str) -> Option<ClassConfig> {
        let (key, class) = self.config.class(label)?;
        self.used.classes.insert(key.to_string());
        Some(class.clone())
    }

    /// The Rust name of the type generated for `node`.
    ///
    /// Precedence: the policy, then the class's `overrideName`, then the
    /// configured naming scheme.
    fn type_name(&mut self, node: &SchemaNode) -> String {
        let label = self.label(node);
        let name = self.derive_name(node, &label);
        if let Err(error) = check_ident(&name, "a type name", &label) {
            self.sink().error(node.location.clone(), error.to_string());
        }
        name
    }

    fn derive_name(&mut self, node: &SchemaNode, label: &str) -> String {
        if let Some(name) = self.policy.type_name(label, node) {
            return name;
        }
        if let Some(name) = self.class(label).and_then(|class| class.override_name) {
            return name;
        }
        let stem = upper_camel(&file_stem(&node.location));
        let own = upper_camel(label);
        match self.config.type_naming {
            TypeNaming::Title => {
                if node.metadata.title.is_some() && self.graph.title_count(label) > 1 {
                    format!("{stem}{own}")
                } else {
                    own
                }
            }
            TypeNaming::FileStem => match node.location.pointer.as_deref() {
                Some(pointer) if !pointer.is_empty() => {
                    format!("{stem}{}", label.to_upper_camel_case())
                }
                _ => stem,
            },
        }
    }

    fn resolve_queue(
        &mut self,
        mut queue: std::collections::VecDeque<(SchemaId, SchemaNode)>,
    ) -> Vec<StructDef> {
        let mut seen: HashSet<Location> = HashSet::new();
        let mut names: HashMap<String, Location> = HashMap::new();
        let mut output = Vec::new();
        while let Some((source, node)) = queue.pop_front() {
            if !seen.insert(node.location.clone()) {
                continue;
            }
            let label = self.label(&node);
            if self.class(&label).is_some_and(|class| class.is_skipped()) {
                continue;
            }
            if !self.policy.should_generate(&label, &node) {
                continue;
            }
            let name = self.type_name(&node);
            if let Some(first) = names.get(&name) {
                let message = format!(
                    "`{name}` is produced by two different schemas (also {first}); \
                     give one an `overrideName` or use `typeNaming: fileStem`"
                );
                self.sink().error(node.location.clone(), message);
                continue;
            }
            names.insert(name.clone(), node.location.clone());
            self.discovered.clear();
            output.push(self.definition_for(&source, &node, name, label));
            queue.extend(self.discovered.drain(..));
        }
        output
    }

    fn definition_for(
        &mut self,
        source: &SchemaId,
        node: &SchemaNode,
        name: String,
        label: String,
    ) -> StructDef {
        let settings = self.settings;
        let mut enums = Vec::new();
        let mut unions = Vec::new();
        let is_object = node.object.is_some() || node.reference.is_some();
        let root_primitive = (!is_object)
            .then(|| types::open_union_primitive(node, settings))
            .flatten();
        let root_union = (!is_object && root_primitive.is_none())
            .then(|| self.union_type(source, node, &name))
            .flatten()
            .map(|(_, definition)| definition);
        let fields = self.fields_for(source, node, &name, &mut enums, &mut unions);
        let alias = (!is_object).then(|| {
            root_primitive
                .clone()
                .unwrap_or_else(|| self.type_for(source, node))
        });
        let mut definition = StructDef {
            name: name.clone(),
            title: label.clone(),
            source_file: file_stem(&node.location),
            description: node.metadata.description.clone().or(Some(label)),
            is_object,
            alias,
            fields,
            extra_fields: Vec::new(),
            extensions: is_object && self.config.extensible,
            enums,
            unions,
        };
        if !is_object
            && let Some(union) = root_union.or_else(|| types::scalar_union(&name, node, settings))
        {
            definition.alias = None;
            definition.unions.push(union);
        }
        self.apply_class(node, &mut definition);
        if definition.is_object {
            self.policy.augment_struct(&mut definition);
        }
        definition
    }

    /// Adds the members the class declares beyond the schema's own.
    fn apply_class(&mut self, node: &SchemaNode, definition: &mut StructDef) {
        let Some(class) = self.class(&self.label(node)) else {
            return;
        };
        for extra in &class.extra_fields {
            if definition.fields.iter().any(|f| f.rust_name == extra.name)
                || definition.extra_fields.iter().any(|f| f.name == extra.name)
            {
                self.sink().error(
                    node.location.clone(),
                    format!(
                        "{}: extra field `{}` collides with another member",
                        definition.name, extra.name
                    ),
                );
                continue;
            }
            definition.extra_fields.push(crate::model::ExtraFieldDef {
                name: extra.name.clone(),
                ty: extra.ty.clone(),
                skip_serde: extra.skip_serde,
                description: extra.doc.clone(),
            });
        }
    }

    /// Whole-output passes: unify open enums, shorten names where that is
    /// unambiguous, and verify the result has no duplicate names.
    fn finish(&mut self, output: &mut [StructDef], custom: &Generated) {
        output.sort_by(|a, b| a.name.cmp(&b.name));
        if self.settings.infer_open_enums {
            enums::merge_open_enums(output, &self.pinned);
        }
        let reserved: HashSet<String> = custom
            .custom_enums
            .iter()
            .map(|e| e.name.clone())
            .chain(custom.aliases.iter().map(|a| a.name.clone()))
            .collect();
        enums::shorten_enum_names(output, &self.pinned, &reserved);
        self.check_unique_names(output, custom);
    }

    fn check_unique_names(&mut self, output: &[StructDef], custom: &Generated) {
        let mut seen: HashSet<&str> = HashSet::new();
        let names = output
            .iter()
            .flat_map(|definition| {
                std::iter::once(definition.name.as_str())
                    .chain(definition.enums.iter().map(|e| e.name.as_str()))
                    .chain(definition.unions.iter().map(|u| u.name.as_str()))
            })
            .chain(custom.custom_enums.iter().map(|e| e.name.as_str()))
            .chain(custom.aliases.iter().map(|a| a.name.as_str()));
        let mut duplicates = Vec::new();
        for name in names {
            if !seen.insert(name) {
                duplicates.push(name.to_string());
            }
        }
        for name in duplicates {
            self.sink().error(
                Location::new("generated"),
                format!(
                    "the type name `{name}` is generated more than once; name one with \
                     `enumNames` or `overrideName`"
                ),
            );
        }
    }
}
