//! The command-line driver: writing, `--check` and warning policy.
#![cfg(feature = "cli")]

mod common;

use std::path::Path;

use common::FIXTURES;
use schemagen::driver::{Args, run};
use schemagen::{DefaultPolicy, Error};

fn args(output: &Path, config: Option<&Path>) -> Args {
    Args {
        schemas: vec![format!("{FIXTURES}/person.schema.json").into()],
        search_paths: vec![],
        config: config.map(Path::to_path_buf),
        output: output.to_path_buf(),
        manifest: Some(output.with_extension("md")),
        module_doc: "Driver test.".into(),
        check: false,
        deny_warnings: false,
    }
}

#[test]
fn writes_the_module_and_manifest_then_checks_them() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out").join("generated.rs");
    let mut args = args(&output, None);
    run(&args, &DefaultPolicy).unwrap();
    assert!(
        std::fs::read_to_string(&output)
            .unwrap()
            .contains("pub struct Person")
    );
    let manifest = std::fs::read_to_string(output.with_extension("md")).unwrap();
    assert!(manifest.contains("`Person`") && manifest.contains("`Address`"));

    args.check = true;
    run(&args, &DefaultPolicy).unwrap();
}

#[test]
fn check_reports_real_drift_but_not_formatting() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("generated.rs");
    let mut args = args(&output, None);
    run(&args, &DefaultPolicy).unwrap();
    args.check = true;

    let text = std::fs::read_to_string(&output).unwrap();
    // A formatter may re-wrap the file without changing what it means.
    std::fs::write(&output, text.replace("\n    ", "\n        ")).unwrap();
    run(&args, &DefaultPolicy).expect("formatting is not drift");

    std::fs::write(
        &output,
        text.replace("pub struct Person", "pub struct Persona"),
    )
    .unwrap();
    assert!(matches!(
        run(&args, &DefaultPolicy),
        Err(Error::OutOfDate(_))
    ));

    std::fs::remove_file(&output).unwrap();
    assert!(matches!(
        run(&args, &DefaultPolicy),
        Err(Error::OutOfDate(_))
    ));
}

#[test]
fn check_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("generated.rs");
    let mut args = args(&output, None);
    args.check = true;
    let _ = run(&args, &DefaultPolicy);
    assert!(!output.exists());
}

#[test]
fn deny_warnings_turns_stale_config_into_failure() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, r#"{"classes":{"Nothing":{"skip":true}}}"#).unwrap();
    let output = dir.path().join("generated.rs");
    let mut args = args(&output, Some(&config));
    run(&args, &DefaultPolicy).expect("a stale entry is only a warning");
    args.deny_warnings = true;
    let error = run(&args, &DefaultPolicy).unwrap_err();
    assert!(error.to_string().contains("Nothing"), "{error}");
}

#[test]
fn a_missing_schema_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = args(&dir.path().join("generated.rs"), None);
    args.schemas = vec![dir.path().join("absent.schema.json")];
    assert!(run(&args, &DefaultPolicy).is_err());
}
