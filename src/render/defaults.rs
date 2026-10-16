//! Rust expressions for schema-declared default values.

use proc_macro2::{Literal, TokenStream};
use quote::quote;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::RustType;

fn mismatch(value: &Value, ty: &RustType) -> Error {
    Error::render(format!("default {value} cannot be a `{ty}`"))
}

fn elements(value: &Value, inner: &RustType, ty: &RustType) -> Result<Vec<TokenStream>> {
    value
        .as_array()
        .ok_or_else(|| mismatch(value, ty))?
        .iter()
        .map(|element| default_expression(element, inner))
        .collect()
}

/// A Rust expression of type `ty` that evaluates to the JSON `value`.
///
/// Scalars and containers are built directly. Types the generator cannot see
/// into (generated structs and enums, maps, `serde_json::Value`) are built by
/// deserializing the JSON, which is exactly what reading the same value from a
/// document would do.
pub(crate) fn default_expression(value: &Value, ty: &RustType) -> Result<TokenStream> {
    match ty {
        RustType::Option(inner) => {
            let inner = default_expression(value, inner)?;
            Ok(quote!(Some(#inner)))
        }
        RustType::Boxed(inner) => {
            let inner = default_expression(value, inner)?;
            Ok(quote!(Box::new(#inner)))
        }
        RustType::Vec(inner) => {
            let items = elements(value, inner, ty)?;
            Ok(quote!(vec![#(#items),*]))
        }
        RustType::Array(inner, length) => {
            let items = elements(value, inner, ty)?;
            if items.len() != *length {
                return Err(mismatch(value, ty));
            }
            Ok(quote!([#(#items),*]))
        }
        RustType::BoxedSlice(inner) => {
            let items = elements(value, inner, ty)?;
            Ok(quote!(Box::new([#(#items),*])))
        }
        RustType::InlineVec(inner, _) => {
            let items = elements(value, inner, ty)?;
            Ok(quote!(InlineVec::from_vec(vec![#(#items),*])))
        }
        RustType::Map(_, _) | RustType::SortedMap(_, _) | RustType::Named(_) | RustType::Value => {
            let json = serde_json::to_string(value).map_err(|e| Error::render(e.to_string()))?;
            Ok(quote!(
                serde_json::from_str(#json).expect("a schema default must match its type")
            ))
        }
        RustType::Bool => value
            .as_bool()
            .map(|value| quote!(#value))
            .ok_or_else(|| mismatch(value, ty)),
        RustType::String => text(value, ty).map(|text| quote!(#text.to_string())),
        RustType::BoxedStr => text(value, ty).map(|text| quote!(#text.into())),
        RustType::F32 | RustType::F64 => {
            let number = value.as_f64().ok_or_else(|| mismatch(value, ty))?;
            let literal = Literal::f64_unsuffixed(number);
            Ok(quote!(#literal))
        }
        integer => {
            let (low, high) = bounds(integer).ok_or_else(|| mismatch(value, ty))?;
            let number = value
                .as_i64()
                .map(i128::from)
                .or_else(|| value.as_u64().map(i128::from))
                .filter(|number| (low..=high).contains(number))
                .ok_or_else(|| mismatch(value, ty))?;
            let literal = Literal::i128_unsuffixed(number);
            Ok(quote!(#literal))
        }
    }
}

fn text<'a>(value: &'a Value, ty: &RustType) -> Result<&'a str> {
    value.as_str().ok_or_else(|| mismatch(value, ty))
}

/// The inclusive range of an integer type, or `None` for other types.
fn bounds(ty: &RustType) -> Option<(i128, i128)> {
    Some(match ty {
        RustType::I8 => (i8::MIN.into(), i8::MAX.into()),
        RustType::I16 => (i16::MIN.into(), i16::MAX.into()),
        RustType::I32 => (i32::MIN.into(), i32::MAX.into()),
        RustType::I64 => (i64::MIN.into(), i64::MAX.into()),
        RustType::U8 => (0, u8::MAX.into()),
        RustType::U16 => (0, u16::MAX.into()),
        RustType::U32 => (0, u32::MAX.into()),
        RustType::U64 => (0, u64::MAX.into()),
        RustType::Usize => (0, usize::MAX as i128),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn render(value: Value, ty: RustType) -> String {
        default_expression(&value, &ty).unwrap().to_string()
    }

    #[test]
    fn scalars_use_the_target_type() {
        assert_eq!(render(json!(4), RustType::U32), "4");
        assert_eq!(render(json!(1), RustType::F64), "1.0");
        assert_eq!(render(json!(0.5), RustType::F32), "0.5");
        assert_eq!(render(json!(true), RustType::Bool), "true");
        assert_eq!(render(json!("a"), RustType::String), "\"a\" . to_string ()");
        assert_eq!(render(json!("a"), RustType::BoxedStr), "\"a\" . into ()");
    }

    #[test]
    fn containers_follow_their_representation() {
        let items = |ty: fn(Box<RustType>) -> RustType| ty(Box::new(RustType::U8));
        assert_eq!(render(json!([1, 2]), items(RustType::Vec)), "vec ! [1 , 2]");
        assert_eq!(
            render(json!([1]), items(RustType::BoxedSlice)),
            "Box :: new ([1])"
        );
        assert_eq!(
            render(json!([1]), RustType::Array(Box::new(RustType::U8), 1)),
            "[1]"
        );
        assert_eq!(
            render(json!([1]), RustType::InlineVec(Box::new(RustType::U8), 2)),
            "InlineVec :: from_vec (vec ! [1])"
        );
        assert_eq!(
            render(json!(3), RustType::Option(Box::new(RustType::U8))),
            "Some (3)"
        );
    }

    #[test]
    fn values_that_do_not_fit_the_type_are_errors() {
        for (value, ty) in [
            (json!(256), RustType::U8),
            (json!(-1), RustType::U32),
            (json!(1.5), RustType::I32),
            (json!("x"), RustType::Bool),
            (json!(1), RustType::String),
            (json!([1, 2]), RustType::Array(Box::new(RustType::U8), 3)),
            (json!(1), RustType::Vec(Box::new(RustType::U8))),
        ] {
            assert!(default_expression(&value, &ty).is_err(), "{value} as {ty}");
        }
    }

    #[test]
    fn opaque_types_are_deserialized_from_json() {
        let expression = render(json!({"a": 1}), RustType::Named("Thing".into()));
        assert!(expression.contains("from_str"), "{expression}");
    }
}
