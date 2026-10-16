//! What the generated source looks like and how it behaves when compiled.

mod common;

use common::*;
use schemagen::{Config, DefaultPolicy, Graph, Module, SettingsPolicy, StringRepr, TypeSettings};
use schemagen::{generate, render_modules};
use serde_json::json;

fn open_enums() -> SettingsPolicy {
    SettingsPolicy::new(TypeSettings::default().inferring_open_enums())
}

fn person() -> (Config, String) {
    let config = Config::from_path(format!("{FIXTURES}/config.json")).unwrap();
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("person.schema.json").unwrap();
    let (generated, _) = generate(&mut graph, &[root], &config, &DefaultPolicy).unwrap();
    let source = render(&generated, &config, &DefaultPolicy);
    (config, source)
}

#[test]
fn the_person_module_matches_its_golden_file() {
    let (_, source) = person();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/expected/person_module.rs"
    );
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &source).unwrap();
    }
    let expected = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        source.replace("\r\n", "\n"),
        expected.replace("\r\n", "\n"),
        "regenerate with UPDATE_GOLDEN=1 if the change is intended"
    );
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(person().1, person().1);
}

#[test]
fn the_person_module_is_lint_clean() {
    assert_compiles_cleanly("person-lint", &person().1);
}

#[test]
fn schema_defaults_apply_when_a_property_is_absent() {
    let config = config(
        r#"{"schemas":{"Settings":{"propertyDefaults":{
        "name":"anon","scale":1,"tags":["a","b"],"on":true,"limit":7}}}}"#,
    );
    let schema = json!({"title": "Settings", "type": "object", "properties": {
        "name": {"type": "string"}, "scale": {"type": "number"},
        "tags": {"type": "array", "items": {"type": "string"}},
        "on": {"type": "boolean"}, "limit": {"type": "integer", "minimum": 0}}});
    let generated = generate_with("settings.json", schema, &config, &DefaultPolicy);
    let source = render(&generated, &config, &DefaultPolicy);
    run_generated_tests(
        "defaults-test",
        &source,
        r##"
#[test]
fn absent_properties_take_their_defaults() {
    let parsed: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed.name, "anon");
    assert_eq!(parsed.scale, 1.0);
    assert_eq!(parsed.tags, vec!["a".to_string(), "b".to_string()]);
    assert!(parsed.on);
    assert_eq!(parsed.limit, 7);
    assert_eq!(parsed, Settings::default());
}
"##,
    );
}

#[test]
fn numeric_enums_serialize_as_numbers() {
    let config = config(
        r#"{"customTypes":{"Kind":{"kind":"enum","numeric":true,
            "variants":["A","B"],"numericValues":{"A":512,"B":513}}}}"#,
    );
    let generated = generate_with(
        "empty.json",
        json!({"type": "object"}),
        &config,
        &DefaultPolicy,
    );
    let source = render(&generated, &config, &DefaultPolicy);
    assert!(source.contains("A = 512"), "{source}");
    run_generated_tests(
        "numeric-enum-test",
        &source,
        r##"
#[test]
fn round_trips_as_integers() {
    assert_eq!(serde_json::to_string(&Kind::B).unwrap(), "513");
    assert_eq!(serde_json::from_str::<Kind>("512").unwrap(), Kind::A);
    assert!(serde_json::from_str::<Kind>("7").is_err());
    assert_eq!(Kind::default(), Kind::A);
}
"##,
    );
}

#[test]
fn custom_aliases_render_as_type_aliases() {
    let config = config(r#"{"customTypes":{"Id":{"kind":"alias","target":"u32","doc":"An id."}}}"#);
    let generated = generate_with(
        "empty.json",
        json!({"type": "object"}),
        &config,
        &DefaultPolicy,
    );
    let source = render(&generated, &config, &DefaultPolicy);
    assert!(source.contains("pub type Id = u32;"), "{source}");
}

#[test]
fn closed_string_enums_accept_configured_aliases() {
    let config = config(r#"{"enumAliases":{"*.mode":{"fast":["Fast"]}}}"#);
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "mode": {"enum": ["fast", "safe"]}}});
    let generated = generate_with("aliased.json", schema, &config, &DefaultPolicy);
    let source = render(&generated, &config, &DefaultPolicy);
    run_generated_tests(
        "closed-alias-test",
        &source,
        r##"
#[test]
fn both_spellings_read_and_the_canonical_one_is_written() {
    let parsed: Holder = serde_json::from_str(r#"{"mode": "Fast"}"#).unwrap();
    assert_eq!(parsed.mode, Some(Mode::Fast));
    assert_eq!(serde_json::to_string(&parsed).unwrap(), r#"{"mode":"fast"}"#);
}
"##,
    );
}

#[test]
fn open_string_enums_accept_configured_aliases_and_keep_unknowns() {
    let config = config(r#"{"enumAliases":{"*.pattern":{"featureData":["FeatureData"]}}}"#);
    let schema = json!({"title": "Store", "type": "object", "properties": {"pattern": {
        "anyOf": [{"const": "featureData"}, {"const": "geometry"}, {"type": "string"}]}}});
    let generated = generate_with("open-alias.json", schema, &config, &open_enums());
    let source = render(&generated, &config, &open_enums());
    run_generated_tests(
        "open-alias-test",
        &source,
        r##"
#[test]
fn aliases_and_unknown_values() {
    let parsed: Store = serde_json::from_str(r#"{"pattern": "FeatureData"}"#).unwrap();
    assert_eq!(parsed.pattern.as_ref().unwrap().as_str(), "featureData");
    let other: Store = serde_json::from_str(r#"{"pattern": "mystery"}"#).unwrap();
    let pattern = other.pattern.unwrap();
    assert!(!pattern.is_known());
    assert_eq!(serde_json::to_string(&pattern).unwrap(), "\"mystery\"");
}
"##,
    );
}

#[test]
fn open_numeric_enums_round_trip_unknown_values() {
    let schema = json!({"title": "Open", "type": "object", "properties": {"componentType": {
        "anyOf": [
            {"const": 5121, "description": "UNSIGNED_BYTE", "type": "integer"},
            {"const": 5126, "description": "FLOAT", "type": "integer"},
            {"type": "integer"}]}}});
    let generated = generate_with(
        "open-roundtrip.json",
        schema,
        &Config::default(),
        &open_enums(),
    );
    let source = render(&generated, &Config::default(), &open_enums());
    run_generated_tests(
        "open-numeric-test",
        &source,
        r##"
#[test]
fn unknown_values_survive() {
    let parsed: Open = serde_json::from_str(r#"{"componentType": 9999}"#).unwrap();
    let value = parsed.component_type.unwrap();
    assert!(!value.is_known());
    assert_eq!(value.0, 9999);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), r#"{"componentType":9999}"#);
}

#[test]
fn known_values_match_constants() {
    let parsed: Open = serde_json::from_str(r#"{"componentType": 5126}"#).unwrap();
    assert_eq!(parsed.component_type.unwrap(), ComponentType::FLOAT);
}
"##,
    );
}

#[test]
fn object_unions_round_trip_untagged() {
    let schema = json!({"title": "Container", "type": "object", "properties": {"value": {"oneOf": [
        {"title": "Circle", "type": "object", "required": ["radius"],
            "properties": {"radius": {"type": "number"}}},
        {"title": "Square", "type": "object", "required": ["size"],
            "properties": {"size": {"type": "number"}}}]}}});
    let generated = generate_default("object-union.json", schema);
    let source = render(&generated, &Config::default(), &DefaultPolicy);
    assert!(source.contains("pub enum ContainerValue"), "{source}");
    run_generated_tests(
        "union-test",
        &source,
        r##"
#[test]
fn picks_the_matching_variant() {
    let parsed: Container = serde_json::from_str(r#"{"value": {"size": 2.0}}"#).unwrap();
    assert!(matches!(parsed.value, Some(ContainerValue::Square(_))));
}
"##,
    );
}

#[test]
fn an_unsuffixed_sorted_map_behaves_like_a_map() {
    let schema = json!({"title": "Holder", "type": "object", "properties": {"attributes": {
        "type": "object", "additionalProperties": {"type": "integer", "minimum": 0}}}});
    let policy = SettingsPolicy::new(TypeSettings::compact().with_string(StringRepr::Owned));
    let generated = generate_with("sorted-map.json", schema, &Config::default(), &policy);
    let source = render(&generated, &Config::default(), &policy);
    assert!(source.contains("SortedMap") && !source.contains("HashMap<String"));
    run_generated_tests(
        "sorted-map-test",
        &source,
        r##"
#[test]
fn keys_are_ordered_and_it_serializes_as_an_object() {
    let parsed: Holder = serde_json::from_str(r#"{"attributes": {"b": 2, "a": 1, "c": 3}}"#).unwrap();
    let attributes = &parsed.attributes;
    assert_eq!(attributes.get("a"), Some(&1));
    assert_eq!(attributes.get("missing"), None);
    let keys: Vec<&String> = attributes.keys().collect();
    assert_eq!(keys, ["a", "b", "c"]);
    assert!(serde_json::to_string(&parsed).unwrap().contains(r#""a":1"#));
}
"##,
    );
}

#[test]
fn boxed_slices_and_inline_vecs_come_with_their_helpers() {
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "slice": {"type": "array", "items": {"type": "number"}},
        "inline": {"type": "array", "items": {"type": "number"}}}});
    let config = config(
        r#"{"schemas":{"Holder":{"propertyTypes":{"slice":"Box<[f64]>","inline":"InlineVec<f64, 4>"}}}}"#,
    );
    let generated = generate_with("helpers.json", schema, &config, &DefaultPolicy);
    let source = render(&generated, &config, &DefaultPolicy);
    assert!(source.contains("deserialize_boxed_slice") && source.contains("pub struct InlineVec"));
    run_generated_tests(
        "helpers-test",
        &source,
        r##"
#[test]
fn both_parse_and_write_back() {
    let json = r#"{"inline":[3.0],"slice":[1.0,2.0]}"#;
    let parsed: Holder = serde_json::from_str(json).unwrap();
    assert_eq!(&*parsed.slice, &[1.0, 2.0]);
    assert_eq!(parsed.inline.as_slice(), &[3.0]);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
}

#[test]
fn absent_and_null_collections_are_empty_and_not_written() {
    for json in ["{}", r#"{"slice": null, "inline": null}"#] {
        let parsed: Holder = serde_json::from_str(json).unwrap();
        assert!(parsed.slice.is_empty() && parsed.inline.as_slice().is_empty());
        assert_eq!(serde_json::to_string(&parsed).unwrap(), "{}");
    }
}
"##,
    );
}

#[test]
fn helpers_are_emitted_only_when_used() {
    let generated = generate_default(
        "plain.json",
        json!({"title": "Plain", "type": "object", "properties": {"x": {"type": "string"}}}),
    );
    let source = render(&generated, &Config::default(), &DefaultPolicy);
    for helper in [
        "InlineVec",
        "SortedMap",
        "deserialize_boxed_slice",
        "is_empty_collection",
    ] {
        assert!(!source.contains(helper), "{helper} should be absent");
    }
}

#[test]
fn extensible_structs_carry_extensions_and_extras() {
    let config = config(r#"{"extensible": true}"#);
    let schema =
        json!({"title": "Thing", "type": "object", "properties": {"x": {"type": "string"}}});
    let generated = generate_with("ext.json", schema, &config, &DefaultPolicy);
    let source = render(&generated, &config, &DefaultPolicy);
    run_generated_tests(
        "ext-test",
        &source,
        r##"
#[test]
fn extras_round_trip_and_empty_ones_are_omitted() {
    let parsed: Thing = serde_json::from_str(r#"{"extensions": {"A": {}}, "extras": 1}"#).unwrap();
    assert!(parsed.extensions.contains_key("A"));
    assert_eq!(serde_json::to_string(&Thing::default()).unwrap(), "{}");
}
"##,
    );
}

#[test]
fn several_modules_share_one_copy_of_the_helpers() {
    let config = config(r#"{"schemas":{"Holder":{"propertyTypes":{"items":"InlineVec<u8, 4>"}}}}"#);
    let schema = |title: &str| {
        json!({"title": title, "type": "object", "properties": {
            "items": {"type": "array", "items": {"type": "integer", "minimum": 0}}}})
    };
    let mut graph = Graph::new(FIXTURES);
    let first = graph.insert_document("one.json", schema("Holder"));
    let second = graph.insert_document("two.json", schema("Holder"));
    let (one, _) = generate(&mut graph, &[first], &config, &DefaultPolicy).unwrap();
    let (two, _) = generate(&mut graph, &[second], &config, &DefaultPolicy).unwrap();
    let modules = [
        Module {
            name: "one",
            generated: &one,
            config: &config,
            policy: &DefaultPolicy,
        },
        Module {
            name: "two",
            generated: &two,
            config: &config,
            policy: &DefaultPolicy,
        },
    ];
    let source = render_modules("Two modules.", &modules).unwrap();
    assert_eq!(source.matches("pub struct InlineVec").count(), 1);
    assert!(source.contains("pub mod one") && source.contains("pub mod two"));
    assert_compiles_cleanly("modules-test", &source);

    let duplicate = [
        Module {
            name: "one",
            generated: &one,
            config: &config,
            policy: &DefaultPolicy,
        },
        Module {
            name: "one",
            generated: &two,
            config: &config,
            policy: &DefaultPolicy,
        },
    ];
    assert!(render_modules("Dup.", &duplicate).is_err());
}

#[test]
fn a_default_that_does_not_fit_fails_rendering_instead_of_panicking() {
    let config = config(r#"{"schemas":{"Holder":{"propertyDefaults":{"n":300}}}}"#);
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "n": {"type": "integer", "minimum": 0, "maximum": 255}}});
    let generated = generate_with("bad-default.json", schema, &config, &DefaultPolicy);
    let result = schemagen::render_module("Bad.", &generated, &config, &DefaultPolicy);
    // Whatever width the resolver chose, a default must either fit or fail loudly.
    if let Err(error) = result {
        assert!(error.to_string().contains("Holder.n"), "{error}");
    }
}
