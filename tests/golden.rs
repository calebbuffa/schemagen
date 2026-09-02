use schemagen::{Config, DefaultPolicy, Graph, generate_types, render_module};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[test]
fn generates_person_and_referenced_address() {
    let config: Config = serde_json::from_str("{}").unwrap();
    let mut graph = Graph::new(FIXTURES);
    let policy = DefaultPolicy;
    let root = graph.load("person.schema.json").expect("person must load");
    let structs = generate_types(&mut graph, root, &config, &policy).expect("types must generate");
    assert!(structs.iter().any(|item| item.name == "Person"));
    assert!(structs.iter().any(|item| item.name == "Address"));
    let person = structs.iter().find(|item| item.name == "Person").unwrap();
    let mood = person
        .fields
        .iter()
        .find(|field| field.json_name == "mood")
        .unwrap();
    assert_eq!(
        mood.ty,
        schemagen::RustType::Option(Box::new(schemagen::RustType::Named("PersonMood".into(),)))
    );
    let output = render_module("Generated test model.", &structs, &config, &policy).unwrap();
    assert!(output.contains("pub struct Person"));
    assert!(output.contains("pub struct Address"));
    assert!(output.contains("pub enum PersonMood"));
    assert!(output.contains("rename = \"happy\""));
    let second_output = render_module("Generated test model.", &structs, &config, &policy).unwrap();
    assert_eq!(output, second_output);
}

#[test]
fn generates_standalone_schema() {
    let config: Config = serde_json::from_str("{}").unwrap();
    let mut graph = Graph::new(FIXTURES);
    let policy = DefaultPolicy;
    let root = graph
        .load("address.schema.json")
        .expect("address must load");
    let structs = generate_types(&mut graph, root, &config, &policy).expect("types must generate");
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "Address");
}

#[test]
fn discovers_transitive_external_reference_documents() {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("person.schema.json").unwrap();

    let documents = graph.reachable_documents(&[root]);

    assert_eq!(documents.len(), 2);
    assert!(
        documents
            .iter()
            .any(|document| document.file.ends_with("person.schema.json"))
    );
    assert!(
        documents
            .iter()
            .any(|document| document.file.ends_with("address.schema.json"))
    );
}

#[test]
fn config_property_default_is_preserved() {
    let config: Config =
        serde_json::from_str(r#"{"classes":{"Address":{"propertyDefaults":{"city":"unknown"}}}}"#)
            .unwrap();
    let mut graph = Graph::new(FIXTURES);
    let policy = DefaultPolicy;
    let root = graph.load("address.schema.json").unwrap();
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let city = structs[0]
        .fields
        .iter()
        .find(|field| field.json_name == "city")
        .unwrap();
    assert_eq!(city.default, Some(serde_json::json!("unknown")));
}

#[test]
fn nullable_any_of_resolves_to_option() {
    let schema = serde_json::json!({
        "title": "Nullable",
        "type": "object",
        "properties": {
            "value": { "anyOf": [{"type": "string"}, {"type": "null"}] }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("nullable.json", schema);
    let policy = DefaultPolicy;
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &policy).unwrap();
    let value = generated[0]
        .fields
        .iter()
        .find(|field| field.json_name == "value")
        .unwrap();
    assert_eq!(
        value.ty,
        schemagen::RustType::Option(Box::new(schemagen::RustType::String))
    );
}

#[test]
fn finite_const_union_generates_enum() {
    let schema = serde_json::json!({
        "title": "Choice",
        "type": "object",
        "properties": {
            "mode": { "anyOf": [{"const": "fast"}, {"const": "safe"}] }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("choice.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    let mode = generated[0]
        .fields
        .iter()
        .find(|field| field.json_name == "mode")
        .unwrap();
    assert!(matches!(
        &mode.ty,
        schemagen::RustType::Option(inner)
            if matches!(inner.as_ref(), schemagen::RustType::Named(_))
    ));
    assert_eq!(generated[0].enums[0].variants.len(), 2);
}

#[test]
fn open_primitive_union_resolves_to_primitive() {
    let schema = serde_json::json!({
        "title": "OpenChoice",
        "type": "object",
        "properties": {
            "mode": { "anyOf": [{"const": "fast"}, {"type": "string"}] }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("open-choice.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    let mode = generated[0]
        .fields
        .iter()
        .find(|field| field.json_name == "mode")
        .unwrap();
    assert_eq!(
        mode.ty,
        schemagen::RustType::Option(Box::new(schemagen::RustType::String))
    );
}

#[test]
fn titled_open_primitive_union_generates_an_alias() {
    let schema = serde_json::json!({
        "title": "OpenString",
        "anyOf": [{"const": "known"}, {"type": "string"}]
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("open-string.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    assert_eq!(generated[0].alias, Some(schemagen::RustType::String));
    assert!(generated[0].unions.is_empty());
}

#[test]
fn renders_numeric_custom_enum() {
    let config: Config = serde_json::from_str(
        r#"{"customTypes":{"ComponentType":{"kind":"enum","numeric":true,"variants":["U8","U16"],"numericValues":{"U8":512,"U16":513}}}}"#,
    )
    .unwrap();
    let policy = DefaultPolicy;
    let output = render_module("Numeric enum test.", &[], &config, &policy).unwrap();
    assert!(output.contains("serde_repr::Serialize_repr"));
    assert!(output.contains("U8 = 512"));
    assert!(output.contains("U16 = 513"));
    syn::parse_file(&output).expect("numeric enum output must parse as Rust");
}

#[test]
fn object_union_generates_untagged_enum() {
    let schema = serde_json::json!({
        "title": "Container",
        "type": "object",
        "properties": {
            "value": {
                "oneOf": [
                    {"title": "Circle", "type": "object", "properties": {"radius": {"type": "number"}}},
                    {"title": "Square", "type": "object", "properties": {"size": {"type": "number"}}}
                ]
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("object-union.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    let container = generated
        .iter()
        .find(|definition| definition.name == "Container")
        .unwrap();
    assert_eq!(container.unions[0].variants.len(), 2);
    let output = render_module("Object union test.", &generated, &config, &DefaultPolicy).unwrap();
    assert!(output.contains("pub enum ContainerValue"));
    assert!(output.contains("#[serde(untagged)]"));
    syn::parse_file(&output).unwrap();
}

#[test]
fn generated_module_compiles_as_rust_crate() {
    let config: Config = serde_json::from_str("{}").unwrap();
    let mut graph = Graph::new(FIXTURES);
    let policy = DefaultPolicy;
    let root = graph.load("person.schema.json").unwrap();
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let output = render_module("Compile test.", &structs, &config, &policy).unwrap();
    syn::parse_file(&output).expect("generated module must parse");

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/lib.rs"), output).unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "generated-compile-test"
version = "0.1.0"
edition = "2024"
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_repr = "0.1"
"#,
    )
    .unwrap();
    let status = std::process::Command::new("cargo")
        .args(["check", "--offline", "--quiet"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success(), "generated module must compile");
}

#[test]
fn loader_reports_invalid_enum_and_lossy_keywords() {
    let schema = serde_json::json!({
        "type": "string",
        "enum": [],
        "if": {"type": "string"}
    });
    let mut sink = schemagen::diagnostics::Sink::new();
    schemagen::loader::convert(&schema, "memory.json", &mut sink);
    assert!(sink.has_errors());
    assert!(sink.has_warnings());
    assert!(
        sink.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("enum must not be empty"))
    );
}
