//! The runtime support code that generated modules embed.
//!
//! Generated types refer to a few helpers - a compact map, an inline vector,
//! deserializers that tolerate `null`. They are written as ordinary Rust
//! under `src/runtime/`, so they are formatted, linted and unit-tested like
//! any other code, and are spliced into the output with `include_str!`. Each
//! is emitted only when something in the output uses it.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::ToTokens;

use crate::error::Result;

/// One embeddable helper. Variants are in emission order, so a helper
/// always follows the ones it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Helper {
    /// `EmptyCollection`, `is_empty_collection` and `deserialize_null_default`.
    EmptyCollection,
    /// `deserialize_boxed_slice`.
    BoxedSlice,
    /// `SortedMap`.
    SortedMap,
    /// `InlineVec`.
    InlineVec,
}

impl Helper {
    fn source(self) -> &'static str {
        match self {
            Helper::EmptyCollection => include_str!("../runtime/empty_collection.rs"),
            Helper::BoxedSlice => include_str!("../runtime/boxed_slice.rs"),
            Helper::SortedMap => include_str!("../runtime/sorted_map.rs"),
            Helper::InlineVec => include_str!("../runtime/inline_vec.rs"),
        }
    }

    /// The helpers this one's source refers to.
    fn requires(self) -> &'static [Helper] {
        match self {
            Helper::SortedMap | Helper::InlineVec => &[Helper::EmptyCollection],
            Helper::EmptyCollection | Helper::BoxedSlice => &[],
        }
    }

    fn tokens(self) -> Result<TokenStream> {
        let file = syn::parse_file(self.source())?;
        Ok(file.items.iter().map(ToTokens::to_token_stream).collect())
    }
}

/// Adds the helpers `needed` depends on, transitively.
pub(crate) fn with_dependencies(needed: impl IntoIterator<Item = Helper>) -> BTreeSet<Helper> {
    let mut all = BTreeSet::new();
    let mut pending: Vec<Helper> = needed.into_iter().collect();
    while let Some(helper) = pending.pop() {
        if all.insert(helper) {
            pending.extend_from_slice(helper.requires());
        }
    }
    all
}

/// The source of `helpers`, in emission order.
pub(crate) fn render_helpers(helpers: &BTreeSet<Helper>) -> Result<TokenStream> {
    let mut tokens = TokenStream::new();
    for helper in helpers {
        tokens.extend(helper.tokens()?);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependencies_are_pulled_in_and_ordered() {
        let helpers = with_dependencies([Helper::InlineVec]);
        assert_eq!(
            helpers.into_iter().collect::<Vec<_>>(),
            [Helper::EmptyCollection, Helper::InlineVec]
        );
        assert!(with_dependencies([]).is_empty());
    }

    #[test]
    fn every_helper_is_valid_rust_on_its_own_with_its_dependencies() {
        for helper in [
            Helper::EmptyCollection,
            Helper::BoxedSlice,
            Helper::SortedMap,
            Helper::InlineVec,
        ] {
            let helpers = with_dependencies([helper]);
            render_helpers(&helpers).unwrap_or_else(|e| panic!("{helper:?}: {e}"));
        }
    }
}

/// The helpers are compiled here, as real code, so they carry real tests: the
/// `unsafe` in them is only as trustworthy as what exercises it.
#[cfg(test)]
#[allow(dead_code, clippy::all)]
mod runtime_tests {
    use std::cell::Cell;

    use serde::{Deserialize, Serialize};

    include!("../runtime/empty_collection.rs");
    include!("../runtime/boxed_slice.rs");
    include!("../runtime/sorted_map.rs");
    include!("../runtime/inline_vec.rs");

    thread_local! {
        static LIVE: Cell<isize> = const { Cell::new(0) };
    }

    fn live() -> isize {
        LIVE.with(Cell::get)
    }

    /// Counts how many of itself are alive, so a leak or double drop shows up
    /// as a non-zero balance.
    #[derive(Debug)]
    struct Counted(u32);

    impl Counted {
        fn new(value: u32) -> Self {
            LIVE.with(|live| live.set(live.get() + 1));
            Self(value)
        }
    }

    impl Clone for Counted {
        fn clone(&self) -> Self {
            Self::new(self.0)
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            LIVE.with(|live| live.set(live.get() - 1));
        }
    }

    /// Deserializes from a number, and fails on 999.
    impl<'de> Deserialize<'de> for Counted {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            match u32::deserialize(d)? {
                999 => Err(serde::de::Error::custom("rejected")),
                value => Ok(Counted::new(value)),
            }
        }
    }

    #[derive(Deserialize)]
    struct Slice {
        #[serde(deserialize_with = "deserialize_boxed_slice")]
        items: Box<[Counted]>,
    }

    fn slice_json(len: usize, poison: bool) -> String {
        let mut items: Vec<String> = (0..len).map(|n| n.to_string()).collect();
        if poison {
            items.push("999".into());
        }
        format!(r#"{{"items": [{}]}}"#, items.join(","))
    }

    #[test]
    fn boxed_slice_reads_every_length_across_the_scratch_boundary() {
        for len in [0, 1, 31, 32, 33, 100] {
            let slice: Slice = serde_json::from_str(&slice_json(len, false)).unwrap();
            assert_eq!(slice.items.len(), len);
            assert!(
                slice
                    .items
                    .iter()
                    .enumerate()
                    .all(|(i, c)| c.0 as usize == i)
            );
            drop(slice);
            assert_eq!(live(), 0, "len {len}");
        }
    }

    #[test]
    fn boxed_slice_accepts_null_as_empty() {
        let slice: Slice = serde_json::from_str(r#"{"items": null}"#).unwrap();
        assert!(slice.items.is_empty());
    }

    #[test]
    fn boxed_slice_drops_what_it_read_when_an_element_fails() {
        for len in [3, 32, 40] {
            assert!(serde_json::from_str::<Slice>(&slice_json(len, true)).is_err());
            assert_eq!(live(), 0, "len {len}");
        }
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Inline {
        values: InlineVec<u32, 3>,
    }

    #[test]
    fn inline_vec_round_trips_inline_and_spilled() {
        for len in 0..8u32 {
            let values: Vec<u32> = (0..len).collect();
            let json = serde_json::to_string(&Inline {
                values: InlineVec::from_vec(values.clone()),
            })
            .unwrap();
            let back: Inline = serde_json::from_str(&json).unwrap();
            assert_eq!(back.values.as_slice(), values.as_slice(), "len {len}");
        }
    }

    #[test]
    fn inline_vec_clones_and_drops_each_element_exactly_once() {
        for len in [0, 2, 3, 4, 10] {
            let original = InlineVec::<Counted, 3>::from_vec((0..len).map(Counted::new).collect());
            assert_eq!(live(), len as isize);
            let copy = original.clone();
            assert_eq!(live(), 2 * len as isize);
            drop(original);
            assert_eq!(copy.len(), len as usize);
            drop(copy);
            assert_eq!(live(), 0, "len {len}");
        }
    }

    #[test]
    fn inline_vec_drops_what_it_read_when_an_element_fails() {
        #[derive(Deserialize)]
        struct Holder {
            #[allow(dead_code)]
            values: InlineVec<Counted, 3>,
        }
        for len in [1, 3, 4, 10] {
            let json = format!(
                r#"{{"values": [{}, 999]}}"#,
                (0..len)
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            assert!(serde_json::from_str::<Holder>(&json).is_err());
            assert_eq!(live(), 0, "len {len}");
        }
    }

    #[test]
    fn inline_vec_is_no_larger_than_its_storage_plus_a_tag() {
        // The larger of the inline buffer and the spill pointer, plus one
        // aligned word for the discriminant.
        assert!(std::mem::size_of::<InlineVec<u32, 3>>() <= 16 + 8);
        assert!(std::mem::size_of::<InlineVec<u8, 32>>() <= 33 + 7 + 8);
    }

    #[test]
    fn sorted_map_keeps_the_first_of_duplicate_keys_and_looks_up_by_borrow() {
        let map = SortedMap::from_vec(vec![
            ("b".to_string(), 1),
            ("a".to_string(), 2),
            ("b".to_string(), 3),
        ]);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("b"), Some(&1));
        assert_eq!(map["a"], 2);
        assert!(!map.contains_key("c"));
        assert_eq!(map.keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn sorted_map_round_trips_as_a_json_object() {
        let map: SortedMap<String, u32> = serde_json::from_str(r#"{"z": 1, "a": 2}"#).unwrap();
        assert_eq!(serde_json::to_string(&map).unwrap(), r#"{"a":2,"z":1}"#);
        assert!(SortedMap::<String, u32>::default().is_empty_collection());
    }

    #[test]
    fn null_default_turns_null_into_the_default() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default, deserialize_with = "deserialize_null_default")]
            values: Vec<u32>,
        }
        let holder: Holder = serde_json::from_str(r#"{"values": null}"#).unwrap();
        assert!(holder.values.is_empty());
        assert!(is_empty_collection(&holder.values));
    }
}
