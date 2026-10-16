//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use schemagen::{
    Config, DefaultPolicy, Generated, GenerationPolicy, Graph, generate, render_module,
};
use serde_json::Value;

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

pub fn config(json: &str) -> Config {
    Config::from_json_str(json).expect("test config must parse")
}

/// Generates from one in-memory schema under `policy`.
pub fn generate_with(
    name: &str,
    schema: Value,
    config: &Config,
    policy: &dyn GenerationPolicy,
) -> Generated {
    try_generate_with(name, schema, config, policy).expect("generation must succeed")
}

pub fn try_generate_with(
    name: &str,
    schema: Value,
    config: &Config,
    policy: &dyn GenerationPolicy,
) -> schemagen::Result<Generated> {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document(name, schema);
    generate(&mut graph, &[root], config, policy).map(|(generated, _)| generated)
}

pub fn generate_default(name: &str, schema: Value) -> Generated {
    generate_with(name, schema, &Config::default(), &DefaultPolicy)
}

pub fn render(generated: &Generated, config: &Config, policy: &dyn GenerationPolicy) -> String {
    render_module("Test module.", generated, config, policy).expect("rendering must succeed")
}

const MANIFEST: &str = r#"[package]
name = "NAME"
version = "0.0.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace]
"#;

/// Compiles `source` as a library crate and runs `tests` against it as an
/// integration test.
pub fn run_generated_tests(name: &str, source: &str, tests: &str) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::create_dir(temp.path().join("tests")).unwrap();
    std::fs::write(temp.path().join("src/lib.rs"), source).unwrap();
    std::fs::write(
        temp.path().join("tests/generated.rs"),
        format!("use {}::*;\n{tests}", name.replace('-', "_")),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        MANIFEST.replace("NAME", name),
    )
    .unwrap();
    run_cargo(temp.path(), &["test", "--quiet", "--", "--test-threads=1"]);
}

/// Compiles `source` and fails on any lint, so generated code is held to what
/// a consumer's own CI would say about it.
pub fn assert_compiles_cleanly(name: &str, source: &str) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/lib.rs"), source).unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        MANIFEST.replace("NAME", name),
    )
    .unwrap();
    run_cargo(temp.path(), &["clippy", "--quiet", "--", "-D", "warnings"]);
}

fn run_cargo(dir: &Path, args: &[&str]) {
    let output = Command::new("cargo")
        .args(args)
        .env("CARGO_INCREMENTAL", "0")
        // One shared target directory, so serde is compiled once rather than
        // once per generated crate.
        .env(
            "CARGO_TARGET_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/target/generated-crates"),
        )
        .current_dir(dir)
        .output()
        .expect("cargo must run");
    assert!(
        output.status.success(),
        "cargo {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
