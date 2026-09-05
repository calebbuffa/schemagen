use schemagen::{
    Config, DefaultPolicy, Graph, SettingsPolicy, TypeSettings, generate_types, render_module,
};

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
    // `mood` is unique across the output, so the owner qualifier is dropped.
    assert_eq!(
        mood.ty,
        schemagen::RustType::Option(Box::new(schemagen::RustType::Named("Mood".into(),)))
    );
    let output = render_module("Generated test model.", &structs, &config, &policy).unwrap();
    assert!(output.contains("pub struct Person"));
    assert!(output.contains("pub struct Address"));
    assert!(output.contains("pub enum Mood"));
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
fn infer_open_enums_collapses_const_union_with_fallback() {
    let schema = serde_json::json!({
        "title": "OpenChoice",
        "type": "object",
        "properties": {
            "mode": { "anyOf": [{"const": "fast"}, {"const": "safe"}, {"type": "string"}] }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("open-choice-enum.json", schema);
    let config = Config::default();
    let open_enums = SettingsPolicy::new(TypeSettings::default().inferring_open_enums());
    let generated = generate_types(&mut graph, root, &config, &open_enums).unwrap();
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
    assert!(!generated[0].enums[0].numeric);
    assert!(
        generated[0].enums[0].open,
        "a trailing open primitive branch means the value set is not closed"
    );
}

#[test]
fn numeric_const_union_generates_enum_named_from_descriptions() {
    let schema = serde_json::json!({
        "title": "Numeric",
        "type": "object",
        "properties": {
            "componentType": {
                "anyOf": [
                    {"const": 5121, "description": "UNSIGNED_BYTE", "type": "integer"},
                    {"const": 5126, "description": "FLOAT", "type": "integer"},
                    {"type": "integer"}
                ]
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("numeric-choice.json", schema);
    let config = Config::default();
    let open_enums = SettingsPolicy::new(TypeSettings::default().inferring_open_enums());
    let generated = generate_types(&mut graph, root, &config, &open_enums).unwrap();
    let generated_enum = &generated[0].enums[0];
    assert!(generated_enum.numeric);
    assert!(generated_enum.open);
    let names: Vec<_> = generated_enum
        .variants
        .iter()
        .map(|variant| variant.name.as_str())
        .collect();
    assert_eq!(names, ["UnsignedByte", "Float"]);
    assert_eq!(generated_enum.variants[0].value, serde_json::json!(5121));
    // `componentType` is unique here, so the `Numeric` owner prefix is dropped.
    assert_eq!(generated_enum.name, "ComponentType");
}

#[test]
fn colliding_enum_property_names_stay_qualified() {
    let schema = serde_json::json!({
        "title": "Root",
        "type": "object",
        "properties": {
            "first": { "$ref": "#/$defs/Alpha" },
            "second": { "$ref": "#/$defs/Beta" }
        },
        "$defs": {
            "Alpha": {
                "title": "Alpha",
                "type": "object",
                "properties": { "kind": { "enum": ["a", "b"] } }
            },
            "Beta": {
                "title": "Beta",
                "type": "object",
                "properties": { "kind": { "enum": ["c", "d"] } }
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("collide.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    let names: Vec<_> = generated
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .map(|generated| generated.name.clone())
        .collect();
    // Two enums both want `Kind`, so neither may claim the short name.
    assert!(
        names.iter().all(|name| name != "Kind"),
        "ambiguous names must stay qualified, got {names:?}"
    );
    assert_eq!(names.len(), 2);
}

#[test]
fn shortening_never_collides_with_an_existing_enum_name() {
    // `Field.type` is named `FieldType` from owner+property, while
    // `Domain.fieldType` shortens onto that same name. The two enums derive
    // from different properties (`type` vs `fieldType`), so a per-property
    // uniqueness check sees no conflict and emits two `FieldType` enums.
    // This is the exact shape found in the i3s cmn schemas.
    let schema = serde_json::json!({
        "title": "Root",
        "type": "object",
        "properties": {
            "domain": { "$ref": "#/$defs/Domain" },
            "field": { "$ref": "#/$defs/Field" }
        },
        "$defs": {
            "Domain": {
                "title": "Domain",
                "type": "object",
                "properties": { "fieldType": { "enum": ["a", "b"] } }
            },
            "Field": {
                "title": "Field",
                "type": "object",
                "properties": { "type": { "enum": ["c", "d"] } }
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("enum-name-clash.json", schema);
    let config: Config = serde_json::from_str("{}").unwrap();
    let generated = generate_types(&mut graph, root, &config, &DefaultPolicy).unwrap();
    let mut names: Vec<_> = generated
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .map(|generated| generated.name.clone())
        .chain(generated.iter().map(|definition| definition.name.clone()))
        .collect();
    names.sort();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(
        unique, names,
        "generated names must be unique, got {names:?}"
    );
}

#[test]
fn number_type_config_controls_float_width() {
    let schema = serde_json::json!({
        "title": "Floats",
        "type": "object",
        "properties": { "scale": { "type": "number" } }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("floats.json", schema);
    let config = Config::default();
    let single =
        SettingsPolicy::new(TypeSettings::default().with_number(schemagen::FloatWidth::Single));
    let generated = generate_types(&mut graph, root, &config, &single).unwrap();
    let scale = generated[0]
        .fields
        .iter()
        .find(|field| field.json_name == "scale")
        .unwrap();
    assert_eq!(
        scale.ty,
        schemagen::RustType::Option(Box::new(schemagen::RustType::F32))
    );
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
fn open_enum_round_trips_unknown_values() {
    let schema = serde_json::json!({
        "title": "Open",
        "type": "object",
        "properties": {
            "componentType": {
                "anyOf": [
                    {"const": 5121, "description": "UNSIGNED_BYTE", "type": "integer"},
                    {"const": 5126, "description": "FLOAT", "type": "integer"},
                    {"type": "integer"}
                ]
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("open-roundtrip.json", schema);
    let config = Config::default();
    let open_enums = SettingsPolicy::new(TypeSettings::default().inferring_open_enums());
    let policy = open_enums;
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let output = render_module("Open enum test.", &structs, &config, &policy).unwrap();
    syn::parse_file(&output).expect("generated module must parse");

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let test_source = r##"
#[test]
fn unknown_value_survives_a_round_trip() {
    // A value outside the schema's named set is still legal, so it must
    // deserialize and serialize back unchanged rather than erroring.
    let parsed: Open = serde_json::from_str(r#"{"componentType": 9999}"#).unwrap();
    let component_type = parsed.component_type.clone().unwrap();
    assert!(!component_type.is_known());
    assert_eq!(component_type.0, 9999);
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        r#"{"componentType":9999}"#
    );
}

#[test]
fn known_values_match_against_constants() {
    let parsed: Open = serde_json::from_str(r#"{"componentType": 5126}"#).unwrap();
    let component_type = parsed.component_type.unwrap();
    assert!(component_type.is_known());
    assert_eq!(component_type, ComponentType::FLOAT);
}
"##;
    std::fs::write(
        temp.path().join("src/lib.rs"),
        format!("{output}\n{test_source}"),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "open-enum-test"
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
        .arg("test")
        // These crates count allocations through a global allocator, so their
        // tests must not run concurrently with each other.
        .args(["--", "--test-threads=1"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success(), "generated open enum must round-trip");
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

#[test]
fn open_enums_for_the_same_property_merge_into_one_type() {
    let open = |values: Vec<&str>| {
        let mut branches: Vec<serde_json::Value> = values
            .iter()
            .map(|value| serde_json::json!({ "const": value, "description": value }))
            .collect();
        branches.push(serde_json::json!({ "type": "string" }));
        serde_json::json!({ "anyOf": branches })
    };
    let schema = serde_json::json!({
        "title": "Root",
        "type": "object",
        "properties": {
            "first": { "$ref": "#/$defs/Alpha" },
            "second": { "$ref": "#/$defs/Beta" }
        },
        "$defs": {
            "Alpha": {
                "title": "Alpha",
                "type": "object",
                "properties": { "kind": open(vec!["aaa"]) }
            },
            "Beta": {
                "title": "Beta",
                "type": "object",
                "properties": { "kind": open(vec!["aaa", "bbb"]) }
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("merge.json", schema);
    let config = Config::default();
    let open_enums = SettingsPolicy::new(TypeSettings::default().inferring_open_enums());
    let generated = generate_types(&mut graph, root, &config, &open_enums).unwrap();
    let enums: Vec<_> = generated
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .collect();
    // An extension widening an open set must not fork a second type.
    assert_eq!(enums.len(), 1, "expected one merged enum, got {enums:?}");
    assert_eq!(enums[0].name, "Kind");
    let mut values: Vec<_> = enums[0]
        .variants
        .iter()
        .map(|variant| variant.value.as_str().unwrap().to_string())
        .collect();
    values.sort();
    assert_eq!(values, vec!["aaa", "bbb"]);
}

#[test]
fn open_enums_with_disjoint_values_do_not_merge() {
    let open = |values: Vec<&str>| {
        let mut branches: Vec<serde_json::Value> = values
            .iter()
            .map(|value| serde_json::json!({ "const": value, "description": value }))
            .collect();
        branches.push(serde_json::json!({ "type": "string" }));
        serde_json::json!({ "anyOf": branches })
    };
    let schema = serde_json::json!({
        "title": "Root",
        "type": "object",
        "properties": {
            "first": { "$ref": "#/$defs/Alpha" },
            "second": { "$ref": "#/$defs/Beta" }
        },
        "$defs": {
            "Alpha": {
                "title": "Alpha",
                "type": "object",
                "properties": { "kind": open(vec!["scalar", "vec2"]) }
            },
            "Beta": {
                "title": "Beta",
                "type": "object",
                "properties": { "kind": open(vec!["perspective", "orthographic"]) }
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("disjoint.json", schema);
    let config = Config::default();
    let open_enums = SettingsPolicy::new(TypeSettings::default().inferring_open_enums());
    let generated = generate_types(&mut graph, root, &config, &open_enums).unwrap();
    let enums: Vec<_> = generated
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .collect();
    // Same property name, unrelated value sets: these are two different types.
    assert_eq!(
        enums.len(),
        2,
        "disjoint sets must not merge, got {enums:?}"
    );
}

#[test]
fn sorted_slice_map_representation_round_trips() {
    // Map-shaped schema objects can be represented as a sorted slice instead of
    // a hash map. The wire format must be identical either way.
    let schema = serde_json::json!({
        "title": "Holder",
        "type": "object",
        "properties": {
            "attributes": {
                "type": "object",
                "additionalProperties": {"type": "integer", "minimum": 0}
            }
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("sorted-map.json", schema);
    let config = Config::default();
    let policy =
        SettingsPolicy::new(TypeSettings::compact().with_string(schemagen::StringRepr::Owned));
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let output = render_module("Sorted map test.", &structs, &config, &policy).unwrap();
    syn::parse_file(&output).expect("generated module must parse");
    assert!(output.contains("SortedMap"));
    assert!(!output.contains("HashMap<String"));

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let test_source = r##"
#[test]
fn sorted_map_behaves_like_a_map() {
    let parsed: Holder =
        serde_json::from_str(r#"{"attributes": {"b": 2, "a": 1, "c": 3}}"#).unwrap();
    let attributes = &parsed.attributes;
    assert_eq!(attributes.len(), 3);
    assert_eq!(attributes.get("a"), Some(&1));
    assert_eq!(attributes.get("c"), Some(&3));
    assert_eq!(attributes.get("missing"), None);
    assert!(attributes.contains_key("b"));
    // Entries are stored in key order regardless of input order.
    let keys: Vec<&String> = attributes.keys().collect();
    assert_eq!(keys, vec!["a", "b", "c"]);
    // Serializing produces a JSON object, not an array of pairs.
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(json.contains(r#""a":1"#), "unexpected: {json}");
}
"##;
    std::fs::write(temp.path().join("src/lib.rs"), &output).unwrap();
    std::fs::create_dir(temp.path().join("tests")).unwrap();
    std::fs::write(
        temp.path().join("tests/roundtrip.rs"),
        format!("use sortedmaptest::*;\n{test_source}"),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "sortedmaptest"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace]
"#,
    )
    .unwrap();
    let status = std::process::Command::new("cargo")
        .arg("test")
        // These crates count allocations through a global allocator, so their
        // tests must not run concurrently with each other.
        .args(["--", "--test-threads=1"])
        .current_dir(temp.path())
        .status()
        .expect("cargo must run");
    assert!(status.success(), "generated crate tests must pass");
}

#[test]
fn boxed_slice_fields_allocate_exactly_once() {
    // Deriving `Box<[T]>` allocates twice for most lengths: once to grow a
    // `Vec` and again to shrink it to fit. The generated deserializer must be
    // at least as cheap at every length, so that the saving needs no tuning.
    let schema = serde_json::json!({
        "title": "Holder",
        "type": "object",
        "properties": {
            "values": {"type": "array", "items": {"type": "number"}}
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("boxed-slice.json", schema);
    let config: Config = serde_json::from_str(
        r#"{"classes": {"Holder": {"propertyOverrides": {"values": "Box<[f64]>"}}}}"#,
    )
    .unwrap();
    let policy = DefaultPolicy;
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let output = render_module("Boxed slice test.", &structs, &config, &policy).unwrap();
    syn::parse_file(&output).expect("generated module must parse");
    assert!(output.contains("deserialize_boxed_slice"));

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let test_source = r##"
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTING.with(|on| {
            if on.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    // Growing and shrinking are the costs being measured, so count them too.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        COUNTING.with(|on| {
            if on.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|on| on.set(true));
    let value = body();
    COUNTING.with(|on| on.set(false));
    (value, ALLOCATIONS.with(|n| n.get()))
}

#[test]
fn never_costs_more_than_the_derived_boxed_slice() {
    for length in [0usize, 1, 2, 3, 4, 5, 31, 32, 33, 100] {
        let source = format!(
            "[{}]",
            (0..length).map(|i| format!("{i}.0")).collect::<Vec<_>>().join(",")
        );
        let field = format!(r#"{{"values": {source}}}"#);

        let (derived, derived_allocations) =
            allocations_during(|| serde_json::from_str::<Box<[f64]>>(&source).unwrap());
        let (parsed, generated_allocations) =
            allocations_during(|| serde_json::from_str::<Holder>(&field).unwrap());
        let values = &parsed.values;

        assert_eq!(&**values, &*derived, "length {length} parsed differently");
        assert!(
            generated_allocations <= derived_allocations,
            "length {length}: generated took {generated_allocations} allocations, \
             derived took {derived_allocations}"
        );
    }
}

#[test]
fn round_trips_across_the_scratch_boundary() {
    for length in [0usize, 32, 33, 200] {
        let values = (0..length).map(|i| i as f64).collect::<Vec<_>>();
        let holder: Holder =
            serde_json::from_str(&serde_json::to_string(&values).map(|v| format!(r#"{{"values":{v}}}"#)).unwrap())
                .unwrap();
        assert_eq!(&*holder.values, &values[..]);
    }
}
"##;
    std::fs::write(temp.path().join("src/lib.rs"), &output).unwrap();
    std::fs::create_dir(temp.path().join("tests")).unwrap();
    std::fs::write(
        temp.path().join("tests/roundtrip.rs"),
        format!("use boxedslicetest::*;\n{test_source}"),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "boxedslicetest"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace]
"#,
    )
    .unwrap();
    let status = std::process::Command::new("cargo")
        .arg("test")
        // These crates count allocations through a global allocator, so their
        // tests must not run concurrently with each other.
        .args(["--", "--test-threads=1"])
        .current_dir(temp.path())
        .status()
        .expect("cargo must run");
    assert!(status.success(), "generated crate tests must pass");
}

#[test]
fn inline_vec_stores_short_sequences_without_allocating() {
    // Short arrays are the common case in schema data, and a heap allocation
    // per value dominates both footprint and parse time once the array count
    // scales with document size.
    let schema = serde_json::json!({
        "title": "Holder",
        "type": "object",
        "properties": {
            "values": {"type": "array", "items": {"type": "number"}}
        }
    });
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document("inline-vec.json", schema);
    let config: Config = serde_json::from_str(
        r#"{"classes": {"Holder": {"propertyOverrides": {"values": "InlineVec<f64, 4>"}}}}"#,
    )
    .unwrap();
    let policy = DefaultPolicy;
    let structs = generate_types(&mut graph, root, &config, &policy).unwrap();
    let output = render_module("Inline vec test.", &structs, &config, &policy).unwrap();
    syn::parse_file(&output).expect("generated module must parse");
    assert!(output.contains("InlineVec"));

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let test_source = r##"
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let value = body();
    (value, ALLOCATIONS.load(Ordering::Relaxed) - before)
}

#[test]
fn short_sequences_round_trip_without_allocating() {
    // `serde_json` allocates a fixed amount of scratch for any parse, so the
    // baseline is what an empty sequence costs; anything above it is ours.
    let (_, baseline) =
        allocations_during(|| serde_json::from_str::<InlineVec<f64, 4>>("[]").unwrap());

    for source in ["[1.0]", "[1.0,2.0,3.0]", "[1.0,2.0,3.0,4.0]"] {
        let (parsed, allocations) =
            allocations_during(|| serde_json::from_str::<InlineVec<f64, 4>>(source).unwrap());
        assert_eq!(
            allocations, baseline,
            "{source} fits inline and must not allocate beyond the parser baseline"
        );
        assert_eq!(serde_json::to_string(&parsed).unwrap(), source);

        // The boxed slice this replaces allocates twice for these lengths:
        // once growing a `Vec` and again shrinking it to fit.
        let (_, boxed) =
            allocations_during(|| serde_json::from_str::<Box<[f64]>>(source).unwrap());
        assert!(
            boxed > allocations,
            "{source}: Box<[f64]> took {boxed} allocations, InlineVec took {allocations}"
        );
    }
}

#[test]
fn sequences_longer_than_capacity_spill_to_the_heap() {
    let source = "[1.0,2.0,3.0,4.0,5.0,6.0]";
    let parsed: InlineVec<f64, 4> = serde_json::from_str(source).unwrap();
    assert_eq!(parsed.len(), 6);
    assert_eq!(parsed.as_slice()[5], 6.0);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), source);
}

#[test]
fn elements_are_dropped_exactly_once_in_both_representations() {
    // The inline representation drops its elements by hand, so a leak or a
    // double free would only show up for a type that tracks its own drops.
    use std::sync::atomic::AtomicIsize;
    static LIVE: AtomicIsize = AtomicIsize::new(0);

    struct Tracked(#[allow(dead_code)] String);
    impl Tracked {
        fn new() -> Self {
            LIVE.fetch_add(1, Ordering::Relaxed);
            Self("payload".to_string())
        }
    }
    // Cloning produces another live value, so it must count as one too.
    impl Clone for Tracked {
        fn clone(&self) -> Self {
            LIVE.fetch_add(1, Ordering::Relaxed);
            Self(self.0.clone())
        }
    }
    impl Drop for Tracked {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    for count in [0usize, 1, 4, 5, 32] {
        let value: InlineVec<Tracked, 4> = (0..count).map(|_| Tracked::new()).collect();
        assert_eq!(value.len(), count);
        let cloned = value.clone();
        assert_eq!(cloned.len(), count);
        drop(value);
        drop(cloned);
        assert_eq!(
            LIVE.load(Ordering::Relaxed),
            0,
            "length {count} leaked or double-freed"
        );
    }
}

#[test]
fn field_use_round_trips() {
    let parsed: Holder = serde_json::from_str(r#"{"values": [1.0, 2.0, 3.0]}"#).unwrap();
    let values = &parsed.values;
    assert_eq!(&values[..], &[1.0, 2.0, 3.0]);
    // `Deref` makes the usual slice methods available.
    assert_eq!(values.iter().copied().sum::<f64>(), 6.0);
}
"##;
    std::fs::write(temp.path().join("src/lib.rs"), &output).unwrap();
    std::fs::create_dir(temp.path().join("tests")).unwrap();
    std::fs::write(
        temp.path().join("tests/roundtrip.rs"),
        format!("use inlinevectest::*;\n{test_source}"),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "inlinevectest"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace]
"#,
    )
    .unwrap();
    let status = std::process::Command::new("cargo")
        .arg("test")
        // These crates count allocations through a global allocator, so their
        // tests must not run concurrently with each other.
        .args(["--", "--test-threads=1"])
        .current_dir(temp.path())
        .status()
        .expect("cargo must run");
    assert!(status.success(), "generated crate tests must pass");
}
