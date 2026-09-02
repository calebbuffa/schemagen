//! The normalized schema IR.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostics::Location;
use crate::pointer::Pointer;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefTarget {
    pub file: String,
    pub pointer: Pointer,
}

impl RefTarget {
    pub fn parse(value: &str) -> Option<Self> {
        let (file, pointer_str) = match value.find('#') {
            Some(0) => ("", value),
            Some(idx) => (&value[..idx], &value[idx..]),
            None => (value, "#"),
        };
        let pointer = Pointer::parse(pointer_str)?;
        Some(Self {
            file: file.to_string(),
            pointer,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObjectConstraints {
    pub properties: BTreeMap<String, SchemaNode>,
    pub required: Vec<String>,
    pub additional_properties: Option<Box<SchemaNode>>,
    pub min_properties: Option<u32>,
    pub max_properties: Option<u32>,
    pub pattern_properties: BTreeMap<String, SchemaNode>,
}

#[derive(Debug, Clone, Default)]
pub struct ArrayConstraints {
    pub items: Option<Box<SchemaNode>>,
    pub min_items: Option<u32>,
    pub max_items: Option<u32>,
    pub unique_items: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NumberConstraints {
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub exclusive_minimum: Option<f64>,
    pub exclusive_maximum: Option<f64>,
    pub multiple_of: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct StringConstraints {
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    pub pattern: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub default: Option<Value>,
    pub examples: Vec<Value>,
    pub deprecated: bool,
    pub read_only: bool,
    pub write_only: bool,
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct Combinators {
    pub all_of: Vec<SchemaNode>,
    pub any_of: Vec<SchemaNode>,
    pub one_of: Vec<SchemaNode>,
    pub not: Option<Box<SchemaNode>>,
    pub if_schema: Option<Box<SchemaNode>>,
    pub then_schema: Option<Box<SchemaNode>>,
    pub else_schema: Option<Box<SchemaNode>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeSet {
    pub string: bool,
    pub integer: bool,
    pub number: bool,
    pub boolean: bool,
    pub object: bool,
    pub array: bool,
    pub null: bool,
}

impl TypeSet {
    pub fn single(ty: &str) -> Self {
        let mut s = Self::default();
        s.set(ty);
        s
    }

    pub fn set(&mut self, ty: &str) {
        match ty {
            "string" => self.string = true,
            "integer" => self.integer = true,
            "number" => self.number = true,
            "boolean" => self.boolean = true,
            "object" => self.object = true,
            "array" => self.array = true,
            "null" => self.null = true,
            _ => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        !(self.string
            || self.integer
            || self.number
            || self.boolean
            || self.object
            || self.array
            || self.null)
    }

    pub fn only(&self) -> Option<&'static str> {
        let mut found = None;
        let mut count = 0;
        let mut check = |flag: bool, name: &'static str| {
            if flag {
                found = Some(name);
                count += 1;
            }
        };
        check(self.string, "string");
        check(self.integer, "integer");
        check(self.number, "number");
        check(self.boolean, "boolean");
        check(self.object, "object");
        check(self.array, "array");
        check(self.null, "null");
        if count == 1 { found } else { None }
    }
}

#[derive(Debug, Clone)]
pub struct SchemaNode {
    pub location: Location,
    pub metadata: Metadata,
    pub reference: Option<RefTarget>,
    pub types: TypeSet,
    pub object: Option<ObjectConstraints>,
    pub array: Option<ArrayConstraints>,
    pub number: Option<NumberConstraints>,
    pub string: Option<StringConstraints>,
    pub combinators: Combinators,
    pub enum_values: Option<Vec<Value>>,
    pub const_value: Option<Value>,
    pub bool_true: bool,
    pub bool_false: bool,
}

impl SchemaNode {
    pub fn any(location: Location) -> Self {
        Self {
            location,
            metadata: Metadata::default(),
            reference: None,
            types: TypeSet::default(),
            object: None,
            array: None,
            number: None,
            string: None,
            combinators: Combinators::default(),
            enum_values: None,
            const_value: None,
            bool_true: true,
            bool_false: false,
        }
    }

    pub fn never(location: Location) -> Self {
        Self {
            location,
            metadata: Metadata::default(),
            reference: None,
            types: TypeSet::default(),
            object: None,
            array: None,
            number: None,
            string: None,
            combinators: Combinators::default(),
            enum_values: None,
            const_value: None,
            bool_true: false,
            bool_false: true,
        }
    }
}
