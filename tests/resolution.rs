//! What the resolver makes of schemas: names, types, enums and diagnostics.

mod common;

use common::*;
use schemagen::{
    Config, DefaultPolicy, Error, FloatWidth, Generated, Graph, RustType, SettingsPolicy,
    StructDef, TypeSettings, generate,
};
use serde_json::json;

fn field<'a>(definition: &'a StructDef, json_name: &str) -> &'a schemagen::model::FieldDef {
    definition
        .fields
        .iter()
        .find(|field| field.json_name == json_name)
        .unwrap_or_else(|| panic!("no field `{json_name}`"))
}

fn open_enums() -> SettingsPolicy {
    SettingsPolicy::new(TypeSettings::default().inferring_open_enums())
}

fn all_enums(generated: &Generated) -> Vec<&schemagen::EnumDef> {
    generated
        .definitions
        .iter()
        .flat_map(|d| d.enums.iter())
        .collect()
}

fn optional(ty: RustType) -> RustType {
    RustType::Option(Box::new(ty))
}

#[test]
fn person_and_the_address_it_references() {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("person.schema.json").unwrap();
    let (generated, _) = generate(&mut graph, &[root], &Config::default(), &DefaultPolicy).unwrap();
    let person = generated
        .definitions
        .iter()
        .find(|d| d.name == "Person")
        .unwrap();
    assert!(generated.definitions.iter().any(|d| d.name == "Address"));
    // `mood` is unique across the output, so the owner qualifier is dropped.
    assert_eq!(
        field(person, "mood").ty,
        optional(RustType::Named("Mood".into()))
    );
}

#[test]
fn a_standalone_schema_yields_one_struct() {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("address.schema.json").unwrap();
    let (generated, _) = generate(&mut graph, &[root], &Config::default(), &DefaultPolicy).unwrap();
    assert_eq!(generated.definitions.len(), 1);
    assert_eq!(generated.definitions[0].name, "Address");
}

#[test]
fn transitive_references_are_discovered() {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("person.schema.json").unwrap();
    let documents = graph.reachable_documents(&[root]);
    assert_eq!(documents.len(), 2);
    assert!(
        documents
            .iter()
            .any(|d| d.file.ends_with("address.schema.json"))
    );
}

#[test]
fn config_defaults_are_applied() {
    let config = config(r#"{"schemas":{"Address":{"propertyDefaults":{"city":"unknown"}}}}"#);
    let mut graph = Graph::new(FIXTURES);
    let root = graph.load("address.schema.json").unwrap();
    let (generated, _) = generate(&mut graph, &[root], &config, &DefaultPolicy).unwrap();
    assert_eq!(
        field(&generated.definitions[0], "city").default,
        Some(json!("unknown"))
    );
}

#[test]
fn nullable_any_of_is_an_option() {
    let generated = generate_default(
        "nullable.json",
        json!({"title": "Nullable", "type": "object", "properties": {
            "value": {"anyOf": [{"type": "string"}, {"type": "null"}]}}}),
    );
    assert_eq!(
        field(&generated.definitions[0], "value").ty,
        optional(RustType::String)
    );
}

#[test]
fn a_finite_const_union_is_an_enum() {
    let generated = generate_default(
        "choice.json",
        json!({"title": "Choice", "type": "object", "properties": {
            "mode": {"anyOf": [{"const": "fast"}, {"const": "safe"}]}}}),
    );
    assert_eq!(generated.definitions[0].enums[0].variants.len(), 2);
    assert!(!generated.definitions[0].enums[0].open);
}

#[test]
fn a_trailing_primitive_makes_the_enum_open_when_inferred() {
    let schema = json!({"title": "OpenChoice", "type": "object", "properties": {
        "mode": {"anyOf": [{"const": "fast"}, {"const": "safe"}, {"type": "string"}]}}});
    let generated = generate_with(
        "open-choice.json",
        schema,
        &Config::default(),
        &open_enums(),
    );
    let found = &generated.definitions[0].enums[0];
    assert_eq!(found.variants.len(), 2);
    assert!(found.open && !found.numeric);
}

#[test]
fn without_open_inference_the_primitive_wins() {
    let generated = generate_default(
        "open-choice-default.json",
        json!({"title": "OpenChoice", "type": "object", "properties": {
            "mode": {"anyOf": [{"const": "fast"}, {"type": "string"}]}}}),
    );
    assert_eq!(
        field(&generated.definitions[0], "mode").ty,
        optional(RustType::String)
    );
}

#[test]
fn numeric_enums_are_named_from_descriptions() {
    let schema = json!({"title": "Numeric", "type": "object", "properties": {
        "componentType": {"anyOf": [
            {"const": 5121, "description": "UNSIGNED_BYTE", "type": "integer"},
            {"const": 5126, "description": "FLOAT", "type": "integer"},
            {"type": "integer"}]}}});
    let generated = generate_with("numeric.json", schema, &Config::default(), &open_enums());
    let found = &generated.definitions[0].enums[0];
    assert!(found.numeric && found.open);
    let names: Vec<_> = found.variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["UnsignedByte", "Float"]);
    assert_eq!(found.variants[0].value, json!(5121));
    assert_eq!(found.name, "ComponentType");
}

fn two_owners(first: serde_json::Value, second: serde_json::Value) -> serde_json::Value {
    json!({"title": "Root", "type": "object",
        "properties": {"first": {"$ref": "#/$defs/Alpha"}, "second": {"$ref": "#/$defs/Beta"}},
        "$defs": {
            "Alpha": {"title": "Alpha", "type": "object", "properties": {"kind": first}},
            "Beta": {"title": "Beta", "type": "object", "properties": {"kind": second}}}})
}

#[test]
fn colliding_enum_property_names_stay_qualified() {
    let schema = two_owners(json!({"enum": ["a", "b"]}), json!({"enum": ["c", "d"]}));
    let generated = generate_default("collide.json", schema);
    let names: Vec<_> = all_enums(&generated)
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|n| n != "Kind"), "{names:?}");
}

#[test]
fn shortening_never_collides_with_an_existing_name() {
    // `Field.type` is named `FieldType`, and `Domain.fieldType` shortens onto
    // the same name. The shape comes from the I3S common schemas.
    let schema = json!({"title": "Root", "type": "object",
        "properties": {"domain": {"$ref": "#/$defs/Domain"}, "field": {"$ref": "#/$defs/Field"}},
        "$defs": {
            "Domain": {"title": "Domain", "type": "object",
                "properties": {"fieldType": {"enum": ["a", "b"]}}},
            "Field": {"title": "Field", "type": "object",
                "properties": {"type": {"enum": ["c", "d"]}}}}});
    let generated = generate_default("clash.json", schema);
    let mut names: Vec<_> = all_enums(&generated)
        .iter()
        .map(|e| e.name.clone())
        .chain(generated.definitions.iter().map(|d| d.name.clone()))
        .collect();
    names.sort();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(unique, names);
}

fn open_kind(values: &[&str]) -> serde_json::Value {
    let mut branches: Vec<_> = values
        .iter()
        .map(|value| json!({"const": value, "description": value}))
        .collect();
    branches.push(json!({"type": "string"}));
    json!({"anyOf": branches})
}

#[test]
fn open_enums_for_one_property_merge() {
    let schema = two_owners(open_kind(&["aaa"]), open_kind(&["aaa", "bbb"]));
    let generated = generate_with("merge.json", schema, &Config::default(), &open_enums());
    let enums = all_enums(&generated);
    assert_eq!(enums.len(), 1, "{enums:?}");
    assert_eq!(enums[0].name, "Kind");
    assert_eq!(enums[0].variants.len(), 2);
}

#[test]
fn open_enums_with_disjoint_values_stay_separate() {
    let schema = two_owners(
        open_kind(&["scalar", "vec2"]),
        open_kind(&["perspective", "orthographic"]),
    );
    let generated = generate_with("disjoint.json", schema, &Config::default(), &open_enums());
    assert_eq!(all_enums(&generated).len(), 2);
}

#[test]
fn float_width_follows_the_settings() {
    let schema = json!({"title": "Floats", "type": "object",
        "properties": {"scale": {"type": "number"}}});
    let single = SettingsPolicy::new(TypeSettings::default().with_number(FloatWidth::Single));
    let generated = generate_with("floats.json", schema, &Config::default(), &single);
    assert_eq!(
        field(&generated.definitions[0], "scale").ty,
        optional(RustType::F32)
    );
}

#[test]
fn a_titled_open_primitive_union_is_an_alias() {
    let generated = generate_default(
        "open-string.json",
        json!({"title": "OpenString", "anyOf": [{"const": "known"}, {"type": "string"}]}),
    );
    assert_eq!(generated.definitions[0].alias, Some(RustType::String));
    assert!(generated.definitions[0].unions.is_empty());
}

#[test]
fn object_unions_become_untagged_enums() {
    let generated = generate_default(
        "object-union.json",
        json!({"title": "Container", "type": "object", "properties": {"value": {"oneOf": [
            {"title": "Circle", "type": "object", "properties": {"radius": {"type": "number"}}},
            {"title": "Square", "type": "object", "properties": {"size": {"type": "number"}}}]}}}),
    );
    let container = generated
        .definitions
        .iter()
        .find(|d| d.name == "Container")
        .unwrap();
    assert_eq!(container.unions[0].variants.len(), 2);
}

#[test]
fn custom_numeric_enums_take_their_declared_values() {
    let config = config(
        r#"{"customTypes":{"ComponentType":{"kind":"enum","numeric":true,
            "variants":["U8","U16"],"numericValues":{"U8":512,"U16":513}}}}"#,
    );
    let generated = generate_with(
        "empty.json",
        json!({"type": "object"}),
        &config,
        &DefaultPolicy,
    );
    let found = &generated.custom_enums[0];
    assert!(found.numeric);
    assert_eq!(found.variants[0].value, json!(512));
    assert_eq!(found.variants[1].value, json!(513));
}

#[test]
fn config_aliases_reach_the_enum_variant() {
    let config = config(r#"{"enumAliases":{"*.mode":{"fast":["Fast"]}}}"#);
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "mode": {"enum": ["fast", "safe"]}}});
    let generated = generate_with("aliases.json", schema, &config, &DefaultPolicy);
    let variants = &generated.definitions[0].enums[0].variants;
    assert_eq!(variants[0].aliases, ["Fast"]);
    assert!(variants[1].aliases.is_empty());
}

#[test]
fn an_alias_equal_to_another_value_is_an_error() {
    let config = config(r#"{"enumAliases":{"*.mode":{"fast":["safe"]}}}"#);
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "mode": {"enum": ["fast", "safe"]}}});
    assert!(try_generate_with("alias-clash.json", schema, &config, &DefaultPolicy).is_err());
}

#[test]
fn property_required_overrides_the_schema() {
    let config = config(r#"{"schemas":{"Holder":{"propertyRequired":{"name":false}}}}"#);
    let schema = json!({"title": "Holder", "type": "object", "required": ["name"],
        "properties": {"name": {"type": "string"}}});
    let generated = generate_with("required.json", schema, &config, &DefaultPolicy);
    let name = field(&generated.definitions[0], "name");
    assert!(!name.required);
    assert_eq!(name.ty, optional(RustType::String));
}

#[test]
fn field_types_resolve_by_owner_then_wildcard() {
    let schema = json!({"title": "Holder", "type": "object", "properties": {
        "a": {"type": "integer"}, "b": {"type": "integer"}, "c": {"type": "integer"}}});
    let config = config(
        r#"{"fieldTypes":{"Holder.a":"u8","*.b":"i16"},"schemas":{"Holder":{"propertyTypes":{"a":"u64"}}}}"#,
    );
    let generated = generate_with("types.json", schema, &config, &DefaultPolicy);
    let holder = &generated.definitions[0];
    // A schema's own `propertyTypes` outranks the shared `fieldTypes` table.
    assert_eq!(field(holder, "a").ty, optional(RustType::U64));
    assert_eq!(field(holder, "b").ty, optional(RustType::I16));
}

#[test]
fn skipped_schemas_become_value_or_their_replacement() {
    let schema = json!({"title": "Root", "type": "object", "properties": {
        "skipped": {"$ref": "#/$defs/Skipped"}, "swapped": {"$ref": "#/$defs/Swapped"}},
        "$defs": {
            "Skipped": {"title": "Skipped", "type": "object", "properties": {"x": {"type": "string"}}},
            "Swapped": {"title": "Swapped", "type": "object", "properties": {"x": {"type": "string"}}}}});
    let config = config(
        r#"{"schemas":{"Skipped":{"skip":true},"Swapped":{"skip":true,"replaceWith":"u32"}}}"#,
    );
    let generated = generate_with("skip.json", schema, &config, &DefaultPolicy);
    let root = generated
        .definitions
        .iter()
        .find(|d| d.name == "Root")
        .unwrap();
    assert_eq!(field(root, "skipped").ty, optional(RustType::Value));
    assert_eq!(field(root, "swapped").ty, optional(RustType::U32));
    assert!(generated.definitions.iter().all(|d| d.name != "Skipped"));
}

#[test]
fn two_schemas_with_one_name_are_an_error_with_a_remedy() {
    let schema = json!({"title": "Root", "type": "object", "properties": {
        "a": {"$ref": "#/$defs/A"}, "b": {"$ref": "#/$defs/B"}},
        "$defs": {
            "A": {"title": "Same", "type": "object", "properties": {"x": {"type": "string"}}},
            "B": {"title": "Same", "type": "object", "properties": {"y": {"type": "string"}}}}});
    let error = try_generate_with("dup.json", schema, &Config::default(), &DefaultPolicy)
        .expect_err("duplicate names must not generate");
    let text = error.to_string();
    assert!(text.contains("Same"), "{text}");
    assert!(
        text.contains("overrideName") || text.contains("fileStem"),
        "{text}"
    );
}

#[test]
fn a_root_union_is_one_type_not_a_duplicate_of_itself() {
    let schema = json!({"title": "Expression", "oneOf": [
        {"type": "boolean"},
        {"type": "string"}]});
    let generated = generate_default("root-union.json", schema);
    let names: Vec<_> = generated
        .definitions
        .iter()
        .flat_map(|d| d.unions.iter().map(|u| u.name.as_str()))
        .collect();
    assert_eq!(names, ["Expression"]);
}

#[test]
fn unused_config_entries_are_reported() {
    let config =
        config(r#"{"schemas":{"Nothing":{"skip":true}},"fieldTypes":{"Holder.gone":"u8"}}"#);
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document(
        "stale.json",
        json!({"title": "Holder", "type": "object", "properties": {"x": {"type": "string"}}}),
    );
    let (_, usage) = generate(&mut graph, &[root], &config, &DefaultPolicy).unwrap();
    let mut sink = schemagen::diagnostics::Sink::new();
    usage.report_unused(&config, &mut sink);
    let messages: Vec<_> = sink.diagnostics().iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("Nothing")));
    assert!(messages.iter().any(|m| m.contains("Holder.gone")));
}

#[test]
fn stale_property_keys_in_a_schema_warn() {
    let config = config(r#"{"schemas":{"Holder":{"propertyTypes":{"missing":"u8"}}}}"#);
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document(
        "stale-property.json",
        json!({"title": "Holder", "type": "object", "properties": {"x": {"type": "string"}}}),
    );
    generate(&mut graph, &[root], &config, &DefaultPolicy).unwrap();
    assert!(
        graph
            .sink()
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("missing")),
        "{:?}",
        graph.sink().diagnostics()
    );
}

#[test]
fn a_general_union_without_a_title_warns_and_becomes_value() {
    let mut graph = Graph::new(FIXTURES);
    let root = graph.insert_document(
        "lossy.json",
        json!({"title": "Holder", "type": "object", "properties": {
            "x": {"type": "array", "items": {"anyOf": [
                {"type": "string"}, {"type": "array", "items": {"type": "string"}}]}}}}),
    );
    let (generated, _) = generate(&mut graph, &[root], &Config::default(), &DefaultPolicy).unwrap();
    assert_eq!(
        field(&generated.definitions[0], "x").ty,
        RustType::Vec(Box::new(RustType::Value))
    );
    assert!(graph.sink().has_warnings());
}

#[test]
fn removed_settings_in_config_point_at_their_replacement() {
    let error = Config::from_json_str(r#"{"numberType":"f32"}"#).unwrap_err();
    assert!(matches!(error, Error::Config(_)));
    assert!(error.to_string().contains("GenerationPolicy"), "{error}");
}

#[test]
fn unknown_config_keys_are_rejected() {
    assert!(Config::from_json_str(r#"{"surprise":1}"#).is_err());
    assert!(Config::from_json_str(r#"{"schemas":{"A":{"surprise":1}}}"#).is_err());
}

#[test]
fn loader_reports_invalid_enum_and_lossy_keywords() {
    let schema = json!({"type": "string", "enum": [], "if": {"type": "string"}});
    let mut sink = schemagen::diagnostics::Sink::new();
    schemagen::loader::convert(&schema, "memory.json", &mut sink);
    assert!(sink.has_errors());
    assert!(sink.has_warnings());
    assert!(
        sink.diagnostics()
            .iter()
            .any(|d| d.message.contains("enum must not be empty"))
    );
}
