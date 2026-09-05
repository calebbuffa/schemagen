//! Load raw JSON Schema documents and convert them to the normalized IR.

use crate::diagnostics::{Location, Sink};
use crate::ir::{
    ArrayConstraints, Combinators, Metadata, NumberConstraints, ObjectConstraints, RefTarget,
    SchemaNode, StringConstraints, TypeSet,
};
use serde_json::Value;
use std::path::Path;

const KNOWN: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$comment",
    "type",
    "enum",
    "const",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "propertyNames",
    "minProperties",
    "maxProperties",
    "items",
    "additionalItems",
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
];

pub fn convert(value: &Value, file: &str, sink: &mut Sink) -> SchemaNode {
    convert_at(value, file, "", sink)
}

fn convert_at(value: &Value, file: &str, pointer: &str, sink: &mut Sink) -> SchemaNode {
    let location = if pointer.is_empty() {
        Location::new(file)
    } else {
        Location::new(file).with_pointer(pointer)
    };
    if let Some(value) = value.as_bool() {
        return if value {
            SchemaNode::any(location)
        } else {
            SchemaNode::never(location)
        };
    }
    let Some(object) = value.as_object() else {
        sink.error(location.clone(), "schema must be an object or boolean");
        return SchemaNode::any(location);
    };
    let mut types = TypeSet::default();
    if let Some(type_value) = object.get("type") {
        match type_value {
            Value::String(name) => {
                if matches!(
                    name.as_str(),
                    "string" | "integer" | "number" | "boolean" | "object" | "array" | "null"
                ) {
                    types.set(name);
                } else {
                    sink.error(location.clone(), format!("unknown schema type: {name}"));
                }
            }
            Value::Array(values) => {
                for value in values {
                    if let Some(name) = value.as_str() {
                        if matches!(
                            name,
                            "string"
                                | "integer"
                                | "number"
                                | "boolean"
                                | "object"
                                | "array"
                                | "null"
                        ) {
                            types.set(name);
                        } else {
                            sink.error(location.clone(), format!("unknown schema type: {name}"));
                        }
                    } else {
                        sink.error(location.clone(), "type array must contain strings");
                    }
                }
            }
            _ => sink.error(
                location.clone(),
                "type must be a string or array of strings",
            ),
        }
    }
    let mut enum_values = None;
    if let Some(enum_value) = object.get("enum") {
        match enum_value.as_array() {
            Some(values) if values.is_empty() => {
                sink.error(location.clone(), "enum must not be empty")
            }
            Some(values) => enum_values = Some(values.clone()),
            None => sink.error(location.clone(), "enum must be an array"),
        }
    }
    for keyword in [
        "not",
        "if",
        "then",
        "else",
        "contains",
        "patternProperties",
        "propertyNames",
    ] {
        if object.contains_key(keyword) {
            sink.warn(
                location.clone(),
                format!("keyword `{keyword}` is preserved but may require a consumer policy"),
            );
        }
    }
    let metadata = metadata(object);
    let reference = object
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|value| {
            let parsed = RefTarget::parse(value);
            if parsed.is_none() {
                sink.error(location.clone(), format!("invalid $ref: {value}"));
            }
            parsed
        });

    SchemaNode {
        location: location.clone(),
        metadata,
        reference,
        types,
        object: object_constraints(object, file, pointer, sink),
        array: array_constraints(object, file, pointer, sink),
        number: number_constraints(object),
        string: string_constraints(object),
        combinators: combinators(object, file, pointer, sink),
        enum_values,
        const_value: object.get("const").cloned(),
        bool_true: false,
        bool_false: false,
    }
}

fn child(base: &str, key: &str) -> String {
    format!("{base}/{key}")
}
fn escaped(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn object_constraints(
    object: &serde_json::Map<String, Value>,
    file: &str,
    base: &str,
    sink: &mut Sink,
) -> Option<ObjectConstraints> {
    if ![
        "properties",
        "required",
        "additionalProperties",
        "patternProperties",
        "minProperties",
        "maxProperties",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
    {
        return None;
    }
    let mut result = ObjectConstraints::default();
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, value) in properties {
            result.properties.insert(
                name.clone(),
                convert_at(
                    value,
                    file,
                    &child(&child(base, "properties"), &escaped(name)),
                    sink,
                ),
            );
        }
    }
    if let Some(required) = object.get("required").and_then(Value::as_array) {
        for value in required {
            if let Some(name) = value.as_str() {
                result.required.push(name.to_string());
            } else {
                sink.error(
                    Location::new(file).with_pointer(child(base, "required")),
                    "required must contain strings",
                );
            }
        }
    }
    if let Some(value) = object.get("additionalProperties") {
        result.additional_properties = Some(Box::new(convert_at(
            value,
            file,
            &child(base, "additionalProperties"),
            sink,
        )));
    }
    if let Some(properties) = object.get("patternProperties").and_then(Value::as_object) {
        for (pattern, value) in properties {
            result.pattern_properties.insert(
                pattern.clone(),
                convert_at(
                    value,
                    file,
                    &child(&child(base, "patternProperties"), &escaped(pattern)),
                    sink,
                ),
            );
        }
    }
    result.min_properties = object
        .get("minProperties")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    result.max_properties = object
        .get("maxProperties")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    Some(result)
}

fn array_constraints(
    object: &serde_json::Map<String, Value>,
    file: &str,
    base: &str,
    sink: &mut Sink,
) -> Option<ArrayConstraints> {
    if !["items", "minItems", "maxItems", "uniqueItems"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return None;
    }
    let mut result = ArrayConstraints::default();
    if let Some(value) = object.get("items") {
        if let Some(array) = value.as_array() {
            sink.warn(Location::new(file).with_pointer(child(base, "items")), "tuple validation is not representable by the current IR; using the first item schema");
            if let Some(first) = array.first() {
                result.items = Some(Box::new(convert_at(
                    first,
                    file,
                    &child(base, "items"),
                    sink,
                )));
            }
        } else {
            result.items = Some(Box::new(convert_at(
                value,
                file,
                &child(base, "items"),
                sink,
            )));
        }
    }
    result.min_items = object
        .get("minItems")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    result.max_items = object
        .get("maxItems")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    result.unique_items = object
        .get("uniqueItems")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(result)
}

fn number_constraints(object: &serde_json::Map<String, Value>) -> Option<NumberConstraints> {
    let mut result = NumberConstraints::default();
    result.minimum = object.get("minimum").and_then(Value::as_f64);
    result.maximum = object.get("maximum").and_then(Value::as_f64);
    result.exclusive_minimum = object.get("exclusiveMinimum").and_then(Value::as_f64);
    result.exclusive_maximum = object.get("exclusiveMaximum").and_then(Value::as_f64);
    result.multiple_of = object.get("multipleOf").and_then(Value::as_f64);
    if result.minimum.is_none()
        && result.maximum.is_none()
        && result.exclusive_minimum.is_none()
        && result.exclusive_maximum.is_none()
        && result.multiple_of.is_none()
    {
        None
    } else {
        Some(result)
    }
}

fn string_constraints(object: &serde_json::Map<String, Value>) -> Option<StringConstraints> {
    let mut result = StringConstraints::default();
    result.min_length = object
        .get("minLength")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    result.max_length = object
        .get("maxLength")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    result.pattern = object
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::to_string);
    result.format = object
        .get("format")
        .and_then(Value::as_str)
        .map(str::to_string);
    if result.min_length.is_none()
        && result.max_length.is_none()
        && result.pattern.is_none()
        && result.format.is_none()
    {
        None
    } else {
        Some(result)
    }
}

fn combinators(
    object: &serde_json::Map<String, Value>,
    file: &str,
    base: &str,
    sink: &mut Sink,
) -> Combinators {
    let mut result = Combinators::default();
    for (key, target) in [
        ("allOf", &mut result.all_of),
        ("anyOf", &mut result.any_of),
        ("oneOf", &mut result.one_of),
    ] {
        if let Some(values) = object.get(key).and_then(Value::as_array) {
            for (index, value) in values.iter().enumerate() {
                target.push(convert_at(
                    value,
                    file,
                    &child(&child(base, key), &index.to_string()),
                    sink,
                ));
            }
        }
    }
    for (key, slot) in [
        ("not", &mut result.not),
        ("if", &mut result.if_schema),
        ("then", &mut result.then_schema),
        ("else", &mut result.else_schema),
    ] {
        if let Some(value) = object.get(key) {
            *slot = Some(Box::new(convert_at(value, file, &child(base, key), sink)));
        }
    }
    result
}

fn metadata(object: &serde_json::Map<String, Value>) -> Metadata {
    let mut result = Metadata::default();
    result.title = object
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    result.description = object
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    result.default = object.get("default").cloned();
    result.examples = object
        .get("examples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    result.deprecated = object
        .get("deprecated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    result.read_only = object
        .get("readOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    result.write_only = object
        .get("writeOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for (key, value) in object {
        if !KNOWN.contains(&key.as_str()) {
            result.extensions.insert(key.clone(), value.clone());
        }
    }
    result
}

pub struct LoadedFile {
    pub raw: Value,
    pub root: SchemaNode,
}

pub fn load_file(path: &Path, sink: &mut Sink) -> Result<LoadedFile, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    let raw: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            sink.error(Location::new(path), format!("parse error: {error}"));
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    };
    let root = convert(&raw, &path.to_string_lossy(), sink);
    Ok(LoadedFile { raw, root })
}
