//! The command-line driver, reusable from a consumer's own build tooling.
//!
//! A consumer with rules of its own writes a small binary that parses
//! [`Args`], supplies its [`GenerationPolicy`], and calls [`run`]. One that
//! needs several modules uses the pieces ([`Source::load`], [`finish`],
//! [`emit`]) directly.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::diagnostics::{Diagnostic, Level, Sink};
use crate::error::{Error, Result};
use crate::graph::{Graph, SchemaId};
use crate::model::Generated;
use crate::policy::GenerationPolicy;
use crate::render::render_module;
use crate::resolve::{ConfigUsage, generate};

/// The options every schemagen command line shares.
#[derive(clap::Args, Debug, Clone)]
pub struct Args {
    /// A schema file, or a directory searched recursively for `*.schema.json`.
    /// Repeatable.
    #[arg(long = "schema", required = true)]
    pub schemas: Vec<PathBuf>,

    /// An extra directory in which `$ref`s are resolved. Repeatable.
    #[arg(long = "search-path")]
    pub search_paths: Vec<PathBuf>,

    /// The configuration file; without one everything is inferred.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// The Rust file to write.
    #[arg(long)]
    pub output: PathBuf,

    /// A Markdown listing of the generated types, written beside the output.
    #[arg(long)]
    pub manifest: Option<PathBuf>,

    /// The module-level documentation of the generated file.
    #[arg(long, default_value = "Generated from JSON Schema. Do not edit.")]
    pub module_doc: String,

    /// Verify that the files on disk match what would be generated, without
    /// writing anything.
    #[arg(long)]
    pub check: bool,

    /// Treat warnings as errors.
    #[arg(long)]
    pub deny_warnings: bool,
}

/// Where schemas come from.
///
/// Only local files are supported, but the loading is confined to [`Source::load`]
/// so that another origin means another way of filling a [`Graph`].
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// Directory against which relative `$ref`s and `additionalSchemas`
    /// resolve. Defaults to the first input's directory.
    pub base: Option<PathBuf>,
    pub search_paths: Vec<PathBuf>,
    /// Files and directories to generate from.
    pub inputs: Vec<PathBuf>,
}

impl From<&Args> for Source {
    fn from(args: &Args) -> Self {
        Self {
            base: None,
            search_paths: args.search_paths.clone(),
            inputs: args.schemas.clone(),
        }
    }
}

/// Schemas read into memory, ready for [`generate`].
pub struct Loaded {
    pub graph: Graph,
    pub roots: Vec<SchemaId>,
}

impl Source {
    /// Reads every input and the config's `additionalSchemas`.
    pub fn load(&self, config: &Config) -> Result<Loaded> {
        let first = self
            .inputs
            .first()
            .ok_or_else(|| Error::config("no schema inputs were given"))?;
        let base = match &self.base {
            Some(base) => base.clone(),
            None if first.is_dir() => first.clone(),
            None => first
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        };
        let base = base.canonicalize().map_err(|e| Error::io(&base, e))?;
        let mut graph = Graph::new(&base);
        graph.add_search_path(&base);
        for path in &self.search_paths {
            graph.add_search_path(path);
        }
        let mut roots = Vec::new();
        for input in &self.inputs {
            let input = input.canonicalize().map_err(|e| Error::io(input, e))?;
            if input.is_dir() {
                roots.extend(graph.load_tree(&input).map_err(|e| Error::io(&input, e))?);
            } else {
                roots.push(graph.load(&input.to_string_lossy())?);
            }
        }
        for additional in &config.additional_schemas {
            roots.push(graph.load(additional)?);
        }
        Ok(Loaded { graph, roots })
    }
}

/// Reads the config named by `args`, or an empty one.
pub fn load_config(args: &Args) -> Result<Config> {
    args.config
        .as_deref()
        .map_or_else(|| Ok(Config::default()), Config::from_path)
}

/// Reports what the run found, and decides whether it succeeded.
///
/// Every diagnostic is printed to stderr. Errors always fail; warnings fail
/// only with `deny_warnings`.
pub fn finish(sink: &Sink, deny_warnings: bool) -> Result<()> {
    for diagnostic in sink.diagnostics() {
        eprintln!("{diagnostic}");
    }
    let failing: Vec<Diagnostic> = sink
        .diagnostics()
        .iter()
        .filter(|d| d.level == Level::Error || deny_warnings)
        .cloned()
        .collect();
    if failing.is_empty() {
        Ok(())
    } else {
        Err(Error::Diagnostics(failing))
    }
}

/// A Markdown listing of what was generated and where custom types came from.
pub fn render_manifest(generated: &Generated) -> String {
    let mut names: Vec<&str> = generated
        .definitions
        .iter()
        .filter(|d| d.is_object || d.alias.is_some())
        .map(|d| d.name.as_str())
        .chain(generated.custom_enums.iter().map(|e| e.name.as_str()))
        .chain(generated.aliases.iter().map(|a| a.name.as_str()))
        .collect();
    names.sort_unstable();
    let mut text = String::from("# Generated types\n\n");
    for name in names {
        text.push_str(&format!("- `{name}`\n"));
    }
    if !generated.custom_origins.is_empty() {
        text.push_str("\n## Defined by the configuration\n\n");
        for (name, origin) in &generated.custom_origins {
            text.push_str(&format!("- `{name}`: {origin}\n"));
        }
    }
    text
}

/// Compares two Rust sources as token streams, so a different rustfmt or
/// prettyplease version does not read as drift.
fn same_rust(existing: &str, expected: &str) -> bool {
    match (syn::parse_file(existing), syn::parse_file(expected)) {
        (Ok(a), Ok(b)) => {
            use quote::ToTokens;
            a.to_token_stream().to_string() == b.to_token_stream().to_string()
        }
        _ => false,
    }
}

/// Writes `text` to `path`, or with `check` verifies it is already there.
///
/// Rust files compare as token streams; anything else compares byte for byte.
pub fn emit(path: &Path, text: &str, check: bool) -> Result<()> {
    if check {
        let existing = std::fs::read_to_string(path).map_err(|_| Error::OutOfDate(path.into()))?;
        let rust = path.extension().is_some_and(|ext| ext == "rs");
        let same = if rust {
            same_rust(&existing, text)
        } else {
            existing == text
        };
        return if same {
            Ok(())
        } else {
            Err(Error::OutOfDate(path.into()))
        };
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// Runs the whole single-module pipeline for `args` under `policy`, reading
/// the config named by `args` (or none).
pub fn run(args: &Args, policy: &dyn GenerationPolicy) -> Result<()> {
    run_with(args, &load_config(args)?, policy)
}

/// Runs the whole single-module pipeline with a config built in code, such as
/// by [`Config::builder`]. `args.config` is ignored.
pub fn run_with(args: &Args, config: &Config, policy: &dyn GenerationPolicy) -> Result<()> {
    let mut loaded = Source::from(args).load(config)?;
    let generated = generate(&mut loaded.graph, &loaded.roots, config, policy);
    let (generated, usage) = match generated {
        Ok(result) => result,
        Err(error) => {
            // The error lists the failures; warnings found on the way are
            // still worth seeing.
            for warning in loaded.graph.sink().diagnostics() {
                if warning.level == Level::Warning {
                    eprintln!("{warning}");
                }
            }
            return Err(error);
        }
    };
    let mut merged = ConfigUsage::default();
    merged.merge(&usage);
    merged.report_unused(config, loaded.graph.sink_mut());
    finish(loaded.graph.sink(), args.deny_warnings)?;

    let source = render_module(&args.module_doc, &generated, config, policy)?;
    emit(&args.output, &source, args.check)?;
    if let Some(manifest) = &args.manifest {
        emit(manifest, &render_manifest(&generated), args.check)?;
    }
    if args.check {
        println!("up to date: {}", args.output.display());
    } else {
        println!("wrote {}", args.output.display());
    }
    Ok(())
}
