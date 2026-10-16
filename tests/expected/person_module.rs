//!Test module.
#![allow(missing_docs)]
#![allow(dead_code)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::approx_constant)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::empty_docs)]
use serde::{Deserialize, Serialize};
///Current mood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mood {
    #[serde(rename = "happy")]
    Happy,
    #[serde(rename = "sad")]
    Sad,
    #[serde(rename = "meh")]
    Meh,
}
impl Default for Mood {
    fn default() -> Self {
        Self::Happy
    }
}
///A postal address.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    ///City name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    ///Street line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    ///Extension-specific data.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub extensions: std::collections::HashMap<String, serde_json::Value>,
    ///Application-specific data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}
///A test person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    ///Age in years.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age: Option<u32>,
    ///Primary residence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<Address>,
    ///Current mood.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<Mood>,
    ///Full name.
    pub name: String,
    ///Arbitrary tags.
    #[serde(default = "default_person_tags")]
    pub tags: Vec<String>,
    ///Schema version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    ///Origin URI when decoded from a file.
    #[serde(skip)]
    pub source_uri: Option<String>,
    ///Extension-specific data.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub extensions: std::collections::HashMap<String, serde_json::Value>,
    ///Application-specific data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}
impl Default for Person {
    fn default() -> Self {
        Self {
            age: Default::default(),
            home: Default::default(),
            mood: Default::default(),
            name: Default::default(),
            tags: default_person_tags(),
            version: Default::default(),
            source_uri: Default::default(),
            extensions: Default::default(),
            extras: Default::default(),
        }
    }
}
fn default_person_tags() -> Vec<String> {
    vec![]
}
