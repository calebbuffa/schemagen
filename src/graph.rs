//! Lazy schema document loading and reference resolution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::diagnostics::Sink;
use crate::ir::{RefTarget, SchemaNode};
use crate::loader;
use crate::pointer::Pointer;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaId {
    pub file: PathBuf,
}

impl SchemaId {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }
}

pub struct Graph {
    root_dir: PathBuf,
    search_dirs: Vec<PathBuf>,
    files: HashMap<PathBuf, Value>,
    sink: Sink,
}

impl Graph {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            search_dirs: Vec::new(),
            files: HashMap::new(),
            sink: Sink::new(),
        }
    }

    pub fn add_search_path(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if !self.search_dirs.contains(&path) {
            self.search_dirs.push(path);
        }
    }

    pub fn sink(&self) -> &Sink {
        &self.sink
    }
    pub fn sink_mut(&mut self) -> &mut Sink {
        &mut self.sink
    }
    pub fn len(&self) -> usize {
        self.files.len()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn load(&mut self, relative: &str) -> Result<SchemaId, String> {
        self.load_path(self.root_dir.join(relative)).ok_or_else(|| {
            format!(
                "schema not found: {}",
                self.root_dir.join(relative).display()
            )
        })
    }

    /// Loads every JSON Schema document below a consumer-selected directory.
    ///
    /// The caller controls which discovered documents are generation roots.
    pub fn load_tree(&mut self, relative: impl AsRef<Path>) -> std::io::Result<Vec<SchemaId>> {
        let mut paths = Vec::new();
        collect_json_files(&self.root_dir.join(relative), &mut paths)?;
        paths.sort();
        Ok(paths
            .into_iter()
            .filter_map(|path| self.load_path(path))
            .collect())
    }

    pub fn insert_document(&mut self, name: impl Into<PathBuf>, raw: Value) -> SchemaId {
        let path = self.root_dir.join(name.into());
        let canonical = path.canonicalize().unwrap_or(path);
        self.files.insert(canonical.clone(), raw);
        SchemaId::new(canonical)
    }

    pub fn raw_value(&self, id: &SchemaId) -> Option<&Value> {
        self.files.get(&id.file)
    }

    pub fn title_count(&self, title: &str) -> usize {
        self.files
            .values()
            .map(|document| count_titles(document, title))
            .sum()
    }

    pub fn root(&mut self, id: &SchemaId) -> Option<SchemaNode> {
        let raw = self.files.get(&id.file)?.clone();
        Some(loader::convert(
            &raw,
            &id.file.to_string_lossy(),
            &mut self.sink,
        ))
    }

    /// Loads every external JSON Schema document referenced by `roots`.
    ///
    /// This discovers documents only. Consumers still decide which independent
    /// schemas are generation roots.
    pub fn reachable_documents(&mut self, roots: &[SchemaId]) -> Vec<SchemaId> {
        let mut queue = VecDeque::from(roots.to_vec());
        let mut discovered = Vec::new();
        let mut seen = HashSet::new();
        while let Some(source) = queue.pop_front() {
            if !seen.insert(source.file.clone()) {
                continue;
            }
            discovered.push(source.clone());
            let Some(raw) = self.files.get(&source.file).cloned() else {
                continue;
            };
            let mut references = Vec::new();
            collect_external_references(&raw, &mut references);
            for reference in references {
                let base = source.file.parent().unwrap_or(Path::new(""));
                let target = base.join(reference);
                if let Some(id) = self.load_path(target) {
                    queue.push_back(id);
                }
            }
        }
        discovered
    }

    pub fn resolve(&mut self, source: &SchemaId, target: &RefTarget) -> Option<SchemaNode> {
        let file = if target.file.is_empty() {
            source.file.clone()
        } else {
            let base = source.file.parent().unwrap_or(Path::new(""));
            let path = base.join(&target.file);
            path.canonicalize().unwrap_or(path)
        };
        let id = self.load_path(file)?;
        let value = self.files.get(&id.file)?.clone();
        let selected = target.pointer.resolve(&value)?.clone();
        Some(loader::convert(
            &selected,
            &id.file.to_string_lossy(),
            &mut self.sink,
        ))
    }

    fn load_path(&mut self, path: PathBuf) -> Option<SchemaId> {
        let mut candidates = if path.is_absolute() {
            vec![path.clone()]
        } else {
            vec![self.root_dir.join(&path)]
        };
        let lookup_name = if path.is_absolute() {
            path.file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.clone())
        } else {
            path.clone()
        };
        candidates.extend(
            self.search_dirs
                .iter()
                .map(|directory| directory.join(&lookup_name)),
        );
        if path.is_absolute()
            && let Ok(relative) = path.strip_prefix(&self.root_dir)
        {
            let components: Vec<_> = relative.components().collect();
            for skip in 1..=2 {
                if components.len() > skip {
                    let relative =
                        components[skip..]
                            .iter()
                            .fold(PathBuf::new(), |mut path, component| {
                                path.push(component.as_os_str());
                                path
                            });
                    candidates.extend(
                        self.search_dirs
                            .iter()
                            .map(|directory| directory.join(&relative)),
                    );
                }
            }
        }
        for candidate in candidates {
            let canonical = candidate.canonicalize().unwrap_or(candidate);
            if self.files.contains_key(&canonical) {
                return Some(SchemaId::new(canonical));
            }
            if let Ok(loaded) = loader::load_file(&canonical, &mut self.sink) {
                self.files.insert(canonical.clone(), loaded.raw);
                return Some(SchemaId::new(canonical));
            }
        }
        None
    }
}

fn collect_external_references(value: &Value, references: &mut Vec<PathBuf>) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get("$ref") {
                let file = reference
                    .split_once('#')
                    .map_or(reference.as_str(), |(file, _)| file);
                if !file.is_empty() {
                    references.push(PathBuf::from(file));
                }
            }
            for value in object.values() {
                collect_external_references(value, references);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_external_references(value, references);
            }
        }
        _ => {}
    }
}

fn collect_json_files(directory: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_json_files(&path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn count_titles(value: &Value, title: &str) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.get("title").and_then(Value::as_str) == Some(title))
                + object
                    .values()
                    .map(|value| count_titles(value, title))
                    .sum::<usize>()
        }
        Value::Array(values) => values.iter().map(|value| count_titles(value, title)).sum(),
        _ => 0,
    }
}

#[allow(dead_code)]
fn _pointer_type_is_used(_: &Pointer) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader;

    #[test]
    fn resolves_definition_pointer_against_raw_document() {
        let raw = serde_json::json!({
            "definitions": { "Thing": { "type": "string" } }
        });
        let mut sink = Sink::new();
        let selected = Pointer::parse("#/definitions/Thing")
            .unwrap()
            .resolve(&raw)
            .unwrap();
        let node = loader::convert(selected, "memory.json", &mut sink);
        assert_eq!(node.types.only(), Some("string"));
        assert!(!sink.has_errors());
    }

    #[test]
    fn loads_json_schemas_from_a_nested_directory() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();
        std::fs::write(temp.path().join("one.schema.json"), r#"{"type":"object"}"#).unwrap();
        std::fs::write(
            temp.path().join("nested/two.schema.json"),
            r#"{"type":"object"}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("nested/readme.txt"), "not a schema").unwrap();

        let mut graph = Graph::new(temp.path());
        let documents = graph.load_tree(".").unwrap();

        assert_eq!(documents.len(), 2);
    }

    #[test]
    fn load_reports_missing_schema() {
        let temp = tempfile::tempdir().unwrap();
        let mut graph = Graph::new(temp.path());

        let error = graph.load("missing.schema.json").unwrap_err();

        assert!(error.contains("schema not found"));
        assert!(error.contains("missing.schema.json"));
    }
}
