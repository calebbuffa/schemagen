use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use schemagen::{Config, DefaultPolicy, Graph, generate_types_from_roots, render_module};

#[derive(Parser)]
#[command(name = "schemagen", about = "Generate Rust types from JSON Schema")]
struct Args {
    #[arg(long)]
    schema: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "generated.rs")]
    out_file: String,
    #[arg(long, default_value = "Generated data model. Do not edit.")]
    module_doc: String,
    #[arg(long)]
    allow_lossy: bool,
    #[arg(long = "additional-schema")]
    additional_schemas: Vec<PathBuf>,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    manifest: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let schema = args
        .schema
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", args.schema.display()))?;
    let schema_dir = schema.parent().context("schema has no parent directory")?;
    let schema_name = schema
        .file_name()
        .context("schema has no file name")?
        .to_string_lossy();
    let config_text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("cannot read {}", args.config.display()))?;
    let config: Config = serde_json::from_str(&config_text).context("cannot parse config")?;
    let policy = DefaultPolicy;
    let mut graph = Graph::new(schema_dir);
    let root = graph
        .load(&schema_name)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut roots = vec![root];
    for additional in config
        .additional_schemas
        .iter()
        .map(PathBuf::from)
        .chain(args.additional_schemas.iter().cloned())
    {
        let name = additional.to_string_lossy();
        let additional_root = graph
            .load(&name)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        roots.push(additional_root);
    }
    let definitions = generate_types_from_roots(&mut graph, roots, &config, &policy)
        .map_err(|error| anyhow::anyhow!(error))?;
    if !args.allow_lossy && graph.sink().has_errors() {
        bail!("schema loading failed");
    }
    for diagnostic in graph.sink().diagnostics() {
        eprintln!("{diagnostic}");
    }
    let output = render_module(&args.module_doc, &definitions, &config, &policy)
        .map_err(|error| anyhow::anyhow!(error))?;
    let output_path = args.output.join(args.out_file);
    if args.check {
        let existing = std::fs::read_to_string(&output_path).unwrap_or_default();
        if existing != output {
            bail!("{} is out of date", output_path.display());
        }
        println!("check ok: {}", output_path.display());
        return Ok(());
    }
    std::fs::create_dir_all(&args.output)?;
    std::fs::write(&output_path, output)
        .with_context(|| format!("cannot write {}", output_path.display()))?;
    println!(
        "Wrote {} structs to {}",
        definitions.len(),
        output_path.display()
    );
    if args.manifest {
        let manifest_path = args.output.join("MANIFEST.md");
        let mut manifest = String::from("# Generated Schemas\n\n");
        for definition in &definitions {
            manifest.push_str(&format!("- `{}`\n", definition.name));
        }
        std::fs::write(manifest_path, manifest)?;
    }
    Ok(())
}
