//! Turning schema text into legal, collision-free Rust identifiers.

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::diagnostics::Location;
use crate::error::{Error, Result};

/// Keywords that cannot be used as a field name even as a raw identifier.
const NON_RAW_KEYWORDS: [&str; 4] = ["self", "Self", "super", "crate"];

/// Rust identifiers cannot begin with a digit, but schema enumerations
/// routinely do (`3dNodeIndexDocument`, `3DObject`). Spell the leading digit
/// out so such values still produce a legal, readable name.
pub(crate) fn spell_leading_digit(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return name.to_string();
    };
    let word = match first {
        '0' => "Zero",
        '1' => "One",
        '2' => "Two",
        '3' => "Three",
        '4' => "Four",
        '5' => "Five",
        '6' => "Six",
        '7' => "Seven",
        '8' => "Eight",
        '9' => "Nine",
        _ => return name.to_string(),
    };
    format!("{word}_{}", chars.as_str())
}

/// A type or variant name derived from arbitrary text.
pub(crate) fn upper_camel(text: &str) -> String {
    spell_leading_digit(text).to_upper_camel_case()
}

/// The Rust field name for a JSON property, and whether it differs from what
/// serde's `rename_all = "camelCase"` would infer from it (which makes an
/// explicit `rename` necessary).
pub(crate) fn field_name(json_name: &str) -> Result<String> {
    let snake = json_name.to_snake_case();
    let name = if NON_RAW_KEYWORDS.contains(&snake.as_str()) {
        format!("{snake}_")
    } else if is_keyword(&snake) {
        format!("r#{snake}")
    } else {
        snake
    };
    check_ident(&name, "a field name", json_name)?;
    Ok(name)
}

/// Whether `name` is a Rust keyword (strict, reserved or edition-gated).
fn is_keyword(name: &str) -> bool {
    syn::parse_str::<syn::Ident>(name).is_err()
        && syn::parse_str::<syn::Ident>(&format!("r#{name}")).is_ok()
}

/// Verifies `name` is a legal identifier (raw identifiers included).
pub(crate) fn check_ident(name: &str, context: &str, source: &str) -> Result<()> {
    if syn::parse_str::<syn::Ident>(name).is_ok() {
        Ok(())
    } else {
        Err(Error::InvalidIdentifier {
            name: name.to_string(),
            context: format!("{context} derived from `{source}`"),
        })
    }
}

/// The file's name without its `.schema.json` (or `.json`) extension.
pub(crate) fn file_stem(location: &Location) -> String {
    let name = location
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".schema.json")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(&name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_are_snake_case_and_escape_keywords() {
        assert_eq!(field_name("nodeCount").unwrap(), "node_count");
        assert_eq!(field_name("type").unwrap(), "r#type");
        assert_eq!(field_name("match").unwrap(), "r#match");
    }

    #[test]
    fn keywords_that_cannot_be_raw_get_a_suffix() {
        for (json, rust) in [("self", "self_"), ("crate", "crate_"), ("super", "super_")] {
            assert_eq!(field_name(json).unwrap(), rust);
        }
    }

    #[test]
    fn unrepresentable_names_are_errors_not_panics() {
        assert!(field_name("").is_err());
        assert!(field_name("---").is_err());
    }

    #[test]
    fn leading_digits_are_spelled_out() {
        assert_eq!(
            upper_camel("3dNodeIndexDocument"),
            "ThreeDNodeIndexDocument"
        );
        assert_eq!(upper_camel("ADD"), "Add");
    }

    #[test]
    fn stems_drop_schema_extensions() {
        let location = Location::new("dir/mesh.primitive.schema.json");
        assert_eq!(file_stem(&location), "mesh.primitive");
        assert_eq!(file_stem(&Location::new("a.json")), "a");
    }
}
