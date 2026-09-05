//! Rust source rendering using `syn`, `quote`, and `prettyplease`.

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse_str;

use crate::config::Config;
use crate::policy::GenerationPolicy;
use crate::types::{RustType, StructDef};

pub fn render_module<P: GenerationPolicy>(
    module_doc: &str,
    structs: &[StructDef],
    config: &Config,
    policy: &P,
) -> Result<String, String> {
    let support = support_definitions(structs);
    let body = render_body(structs, config, policy)?;
    let doc = syn::LitStr::new(module_doc, proc_macro2::Span::call_site());
    let attrs = module_attributes();
    render_tokens(quote! {
        #![doc = #doc]
        #attrs
        use serde::{Deserialize, Serialize};
        #support
        #body
    })
}

/// Renders several named modules into one file, emitting the shared support
/// code (serde imports, collection helpers, deserializers) exactly once at the
/// top level rather than repeating it inside every module.
///
/// A schema family split across profiles produces one module per profile, and
/// each one otherwise carries its own private copy of identical helper code.
/// Each module re-exports the shared items via `use super::*`, so generated
/// paths are unchanged.
pub fn render_modules<P: GenerationPolicy>(
    file_doc: &str,
    modules: &[(&str, &[StructDef], &P)],
    config: &Config,
) -> Result<String, String> {
    let all: Vec<StructDef> = modules
        .iter()
        .flat_map(|(_, structs, _)| structs.iter().cloned())
        .collect();
    let support = support_definitions(&all);
    let rendered: Vec<TokenStream> = modules
        .iter()
        .map(|(name, structs, policy)| {
            let body = render_body(structs, config, *policy)?;
            let ident = syn::Ident::new(name, proc_macro2::Span::call_site());
            Ok::<_, String>(quote! {
                pub mod #ident {
                    use super::*;
                    #body
                }
            })
        })
        .collect::<Result<_, _>>()?;
    let doc = syn::LitStr::new(file_doc, proc_macro2::Span::call_site());
    let attrs = module_attributes();
    render_tokens(quote! {
        #![doc = #doc]
        #attrs
        use serde::{Deserialize, Serialize};
        #support
        #(#rendered)*
    })
}

/// The lint and documentation attributes every generated file carries.
fn module_attributes() -> TokenStream {
    quote! {
        #![allow(missing_docs)]
        // Generated modules describe an entire schema; consumers typically use
        // only part of it.
        #![allow(dead_code)]
        // Schema-declared defaults are rarely the type's `Default::default()`,
        // so these impls are written out rather than derived.
        #![allow(clippy::derivable_impls)]
        // Schema literals are reproduced verbatim; rewriting them as
        // `std::f64::consts` or collapsing them into ranges would obscure the
        // fact that they came from the schema.
        #![allow(clippy::approx_constant)]
        #![allow(clippy::excessive_precision)]
        // Schema descriptions are sometimes empty strings.
        #![allow(clippy::empty_docs)]
    }
}

/// The helper types and functions that generated fields refer to, emitted only
/// when some field actually uses them.
fn support_definitions(structs: &[StructDef]) -> TokenStream {
    let sorted_map = if uses_sorted_map(structs) {
        sorted_map_definition()
    } else {
        TokenStream::new()
    };
    let inline_vec = if uses_inline_vec(structs) {
        inline_vec_definition()
    } else {
        TokenStream::new()
    };
    let boxed_slice = if uses_boxed_slice(structs) {
        boxed_slice_definition()
    } else {
        TokenStream::new()
    };
    let empty_collection = if structs.iter().any(|definition| {
        definition
            .fields
            .iter()
            .any(|field| !field.required && field.default.is_none() && omits_when_empty(&field.ty))
    }) {
        empty_collection_definition()
    } else {
        TokenStream::new()
    };
    quote! {
        #empty_collection
        #boxed_slice
        #sorted_map
        #inline_vec
    }
}

/// Renders the type definitions for one module, without support code.
fn render_body<P: GenerationPolicy>(
    structs: &[StructDef],
    config: &Config,
    policy: &P,
) -> Result<TokenStream, String> {
    let definitions: Vec<TokenStream> = structs
        .iter()
        .filter(|definition| definition.is_object || definition.alias.is_some())
        .map(|definition| {
            let rendered = render_definition(definition)?;
            let items = policy.struct_items(definition);
            Ok::<_, String>(quote!(#rendered #(#items)*))
        })
        .collect::<Result<_, _>>()?;
    let mut custom_types = config
        .custom_types
        .iter()
        .map(|(name, definition)| render_custom_type(name, definition))
        .collect::<Result<Vec<_>, _>>()?;
    custom_types.sort_by_key(|definition| definition.to_string());
    let policy_definitions = policy.additional_definitions();
    let mut enum_definitions = structs
        .iter()
        .flat_map(|definition| definition.enums.iter())
        .map(render_schema_enum)
        .collect::<Result<Vec<_>, _>>()?;
    enum_definitions.sort_by_key(|definition| definition.to_string());
    let mut union_definitions = structs
        .iter()
        .flat_map(|definition| definition.unions.iter())
        .map(render_union)
        .collect::<Result<Vec<_>, _>>()?;
    union_definitions.sort_by_key(|definition| definition.to_string());
    Ok(quote! {
        #(#custom_types)*
        #(#policy_definitions)*
        #(#enum_definitions)*
        #(#union_definitions)*
        #(#definitions)*
    })
}

fn render_tokens(tokens: TokenStream) -> Result<String, String> {
    let token_text = tokens.to_string();
    let file: syn::File = syn::parse2(tokens).map_err(|error| {
        format!(
            "generated Rust is invalid: {error}; output starts: {}",
            &token_text[..token_text.len().min(1000)]
        )
    })?;
    Ok(prettyplease::unparse(&file))
}

fn uses_sorted_map(structs: &[StructDef]) -> bool {
    type_predicate(structs, &|ty| matches!(ty, RustType::SortedMap(_, _)))
}

fn uses_inline_vec(structs: &[StructDef]) -> bool {
    type_predicate(structs, &|ty| matches!(ty, RustType::InlineVec(_, _)))
}

/// Whether a field of this type represents absence as emptiness, and so should
/// be omitted from serialized output when empty rather than written as `[]`.
fn omits_when_empty(ty: &RustType) -> bool {
    matches!(
        ty,
        RustType::Vec(_)
            | RustType::BoxedSlice(_)
            | RustType::InlineVec(_, _)
            | RustType::Map(_, _)
            | RustType::SortedMap(_, _)
    )
}

fn uses_boxed_slice(structs: &[StructDef]) -> bool {
    type_predicate(structs, &|ty| matches!(ty, RustType::BoxedSlice(_)))
}

/// The `boxed_slice` deserializer emitted alongside generated definitions.
///
/// Deriving `Box<[T]>` directly costs two allocations for most lengths: serde
/// fills a growing `Vec`, then `into_boxed_slice` reallocates to drop the spare
/// capacity. Collecting into a stack buffer first makes the length known before
/// anything is allocated, so the single allocation is already exact.
fn boxed_slice_definition() -> TokenStream {
    quote!(
        /// How many elements are collected on the stack before falling back to
        /// a heap buffer. Sized so that schema-typical short arrays are covered
        /// while keeping the stack frame small.
        const BOXED_SLICE_SCRATCH: usize = 32;

        /// Deserializes a sequence into an exactly-sized `Box<[T]>`.
        ///
        /// Used via `#[serde(deserialize_with = ...)]` on `Box<[T]>` fields.
        fn deserialize_boxed_slice<'de, D, T>(deserializer: D) -> Result<Box<[T]>, D::Error>
        where
            D: serde::Deserializer<'de>,
            T: Deserialize<'de>,
        {
            /// Owns the initialized prefix of the scratch buffer, so that an
            /// error partway through the sequence still drops what was read.
            struct Scratch<T> {
                buffer: [std::mem::MaybeUninit<T>; BOXED_SLICE_SCRATCH],
                len: usize,
            }

            impl<T> Scratch<T> {
                fn as_mut_slice(&mut self) -> &mut [T] {
                    // SAFETY: the leading `len` slots are initialized, and
                    // `MaybeUninit<T>` shares `T`'s layout.
                    unsafe {
                        std::slice::from_raw_parts_mut(
                            self.buffer.as_mut_ptr().cast::<T>(),
                            self.len,
                        )
                    }
                }

                /// Moves the collected elements out, leaving the scratch empty.
                fn drain_into(&mut self, sink: &mut Vec<T>) {
                    // SAFETY: the leading `len` slots are initialized. `len` is
                    // cleared first, so a panic in `push` cannot drop them twice.
                    let len = std::mem::replace(&mut self.len, 0);
                    for slot in self.buffer.iter_mut().take(len) {
                        sink.push(unsafe { slot.assume_init_read() });
                    }
                }
            }

            impl<T> Drop for Scratch<T> {
                fn drop(&mut self) {
                    // SAFETY: the leading `len` slots are initialized, and
                    // `drop` runs at most once per value.
                    unsafe { std::ptr::drop_in_place(self.as_mut_slice()) };
                }
            }

            struct SliceVisitor<T>(std::marker::PhantomData<T>);

            impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for SliceVisitor<T> {
                type Value = Box<[T]>;

                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a sequence")
                }

                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut access: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut scratch = Scratch::<T> {
                        buffer: [const { std::mem::MaybeUninit::uninit() }; BOXED_SLICE_SCRATCH],
                        len: 0,
                    };
                    while scratch.len < BOXED_SLICE_SCRATCH {
                        let Some(element) = access.next_element()? else {
                            // The length is known before allocating, so this
                            // allocation is already exactly sized.
                            let mut collected = Vec::with_capacity(scratch.len);
                            scratch.drain_into(&mut collected);
                            return Ok(collected.into_boxed_slice());
                        };
                        scratch.buffer[scratch.len].write(element);
                        scratch.len += 1;
                    }
                    // Longer than the scratch buffer: fall back to growing a
                    // `Vec`, which still shrinks once at the end.
                    let mut collected = Vec::with_capacity(BOXED_SLICE_SCRATCH * 2);
                    scratch.drain_into(&mut collected);
                    while let Some(element) = access.next_element()? {
                        collected.push(element);
                    }
                    Ok(collected.into_boxed_slice())
                }
            }

            /// Accepts `null` for an absent sequence.
            ///
            /// A schema that declares an array and does not require it is
            /// routinely satisfied by producers writing an explicit `null`
            /// rather than omitting the key, so a deserializer that only
            /// accepts a sequence rejects conforming documents.
            struct NullableSlice<T>(std::marker::PhantomData<T>);

            impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for NullableSlice<T> {
                type Value = Box<[T]>;

                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a sequence or null")
                }

                fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                    Ok(Box::default())
                }

                fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                    Ok(Box::default())
                }

                fn visit_some<D: serde::Deserializer<'de>>(
                    self,
                    deserializer: D,
                ) -> Result<Self::Value, D::Error> {
                    deserializer.deserialize_seq(SliceVisitor(std::marker::PhantomData))
                }
            }

            deserializer.deserialize_option(NullableSlice(std::marker::PhantomData))
        }
    )
}

/// The `is_empty_collection` support predicate emitted alongside generated
/// definitions, used as a `skip_serializing_if` for collection-typed fields.
fn empty_collection_definition() -> TokenStream {
    quote!(
        /// Whether a collection-typed field should be omitted from output.
        ///
        /// A schema property that is absent deserializes to an empty
        /// collection, so writing it back out as `[]` or `{}` would not
        /// round-trip.
        fn is_empty_collection<T: EmptyCollection>(value: &T) -> bool {
            value.is_empty_collection()
        }

        /// Implemented by the collection types that generated fields use.
        trait EmptyCollection {
            /// Whether this collection has no entries.
            fn is_empty_collection(&self) -> bool;
        }

        impl<T> EmptyCollection for Vec<T> {
            fn is_empty_collection(&self) -> bool {
                self.is_empty()
            }
        }

        impl<T> EmptyCollection for Box<[T]> {
            fn is_empty_collection(&self) -> bool {
                self.is_empty()
            }
        }

        impl<K, V> EmptyCollection for std::collections::HashMap<K, V> {
            fn is_empty_collection(&self) -> bool {
                self.is_empty()
            }
        }

        /// Deserializes a collection, treating `null` as absent.
        ///
        /// A schema that declares an array or object and does not require it
        /// is routinely satisfied by producers writing an explicit `null`
        /// rather than omitting the key. `#[serde(default)]` only covers the
        /// omitted case, so without this a conforming document is rejected.
        fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
        where
            D: serde::Deserializer<'de>,
            T: Default + Deserialize<'de>,
        {
            Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
        }
    )
}

/// Whether any field's type, at any nesting depth, satisfies `predicate`.
fn type_predicate(structs: &[StructDef], predicate: &dyn Fn(&RustType) -> bool) -> bool {
    fn walk(ty: &RustType, predicate: &dyn Fn(&RustType) -> bool) -> bool {
        if predicate(ty) {
            return true;
        }
        match ty {
            RustType::Option(inner)
            | RustType::Vec(inner)
            | RustType::Boxed(inner)
            | RustType::BoxedSlice(inner)
            | RustType::InlineVec(inner, _)
            | RustType::Array(inner, _) => walk(inner, predicate),
            RustType::Map(key, value) | RustType::SortedMap(key, value) => {
                walk(key, predicate) || walk(value, predicate)
            }
            _ => false,
        }
    }
    structs.iter().any(|definition| {
        definition
            .fields
            .iter()
            .any(|field| walk(&field.ty, predicate))
    })
}

/// The `InlineVec` support type emitted alongside generated definitions.
///
/// Schema arrays are often short and fixed-shape, and a heap allocation per
/// value dominates both footprint and parse time once the array count scales
/// with document size. Storing the common lengths inline removes that
/// allocation entirely.
///
/// This trades struct size for allocation count, so it only pays off when
/// nearly every value of the containing type carries the field. When the field
/// is frequently absent, the inline capacity is paid by every value and earns
/// nothing; prefer `Box<[T]>`, whose deserializer already avoids the
/// grow-then-shrink reallocation.
fn inline_vec_definition() -> TokenStream {
    quote!(
        /// A sequence storing up to `N` elements inline, spilling to the heap
        /// only when longer.
        ///
        /// Deserialization fills the inline buffer directly, so short values
        /// cost no allocation at all. A `Box<[T]>` by contrast costs two for
        /// short input: one to grow a `Vec` and another to shrink it to fit.
        ///
        /// The inline buffer and the spill pointer are mutually exclusive and
        /// share storage, so the inline capacity costs only the bytes by which
        /// it exceeds a pointer, rather than being added on top of one.
        pub struct InlineVec<T, const N: usize>(InlineVecRepr<T, N>);

        /// Storage for [`InlineVec`].
        ///
        /// Private: the `Inline` variant carries the invariant that its leading
        /// `len` slots are initialized, which safe code must not be able to
        /// violate by constructing the variant directly.
        enum InlineVecRepr<T, const N: usize> {
            /// The leading `len` slots of `buffer` are initialized.
            Inline {
                buffer: [std::mem::MaybeUninit<T>; N],
                len: u8,
            },
            /// Storage for sequences longer than `N`.
            Spilled(Box<[T]>),
        }

        impl<T, const N: usize> InlineVec<T, N> {
            /// Returns the elements as a slice.
            pub fn as_slice(&self) -> &[T] {
                match &self.0 {
                    InlineVecRepr::Spilled(spilled) => spilled,
                    // SAFETY: the leading `len` slots are initialized by
                    // construction, and `MaybeUninit<T>` shares `T`'s layout.
                    InlineVecRepr::Inline { buffer, len } => unsafe {
                        std::slice::from_raw_parts(buffer.as_ptr().cast::<T>(), *len as usize)
                    },
                }
            }

            /// Returns the elements as a mutable slice.
            pub fn as_mut_slice(&mut self) -> &mut [T] {
                match &mut self.0 {
                    InlineVecRepr::Spilled(spilled) => spilled,
                    // SAFETY: as in `as_slice`.
                    InlineVecRepr::Inline { buffer, len } => unsafe {
                        std::slice::from_raw_parts_mut(
                            buffer.as_mut_ptr().cast::<T>(),
                            *len as usize,
                        )
                    },
                }
            }

            /// Returns the number of elements.
            pub fn len(&self) -> usize {
                self.as_slice().len()
            }

            /// Returns whether the sequence is empty.
            pub fn is_empty(&self) -> bool {
                self.len() == 0
            }

            /// Iterates over the elements.
            pub fn iter(&self) -> std::slice::Iter<'_, T> {
                self.as_slice().iter()
            }

            /// Builds a value from `elements`, storing them inline when short
            /// enough. When spilling, the `Vec`'s allocation is reused.
            pub fn from_vec(elements: Vec<T>) -> Self {
                if elements.len() > N {
                    return Self(InlineVecRepr::Spilled(elements.into_boxed_slice()));
                }
                let mut buffer = [const { std::mem::MaybeUninit::uninit() }; N];
                let mut len = 0u8;
                for element in elements {
                    buffer[len as usize].write(element);
                    len += 1;
                }
                Self(InlineVecRepr::Inline { buffer, len })
            }
        }

        impl<T, const N: usize> Drop for InlineVecRepr<T, N> {
            fn drop(&mut self) {
                if let Self::Inline { buffer, len } = self {
                    // SAFETY: the leading `len` slots are initialized, and
                    // `drop` runs at most once per value.
                    unsafe {
                        std::ptr::drop_in_place(std::slice::from_raw_parts_mut(
                            buffer.as_mut_ptr().cast::<T>(),
                            *len as usize,
                        ));
                    }
                }
            }
        }

        impl<T: Clone, const N: usize> Clone for InlineVec<T, N> {
            fn clone(&self) -> Self {
                Self::from_vec(self.as_slice().to_vec())
            }
        }

        impl<T, const N: usize> Default for InlineVec<T, N> {
            fn default() -> Self {
                Self(InlineVecRepr::Inline {
                    buffer: [const { std::mem::MaybeUninit::uninit() }; N],
                    len: 0,
                })
            }
        }

        impl<T, const N: usize> std::ops::Deref for InlineVec<T, N> {
            type Target = [T];
            fn deref(&self) -> &[T] {
                self.as_slice()
            }
        }

        impl<T, const N: usize> std::ops::DerefMut for InlineVec<T, N> {
            fn deref_mut(&mut self) -> &mut [T] {
                self.as_mut_slice()
            }
        }

        impl<T: std::fmt::Debug, const N: usize> std::fmt::Debug for InlineVec<T, N> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.as_slice().fmt(f)
            }
        }

        impl<T: PartialEq, const N: usize> PartialEq for InlineVec<T, N> {
            fn eq(&self, other: &Self) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl<T: Eq, const N: usize> Eq for InlineVec<T, N> {}

        impl<T, const N: usize> FromIterator<T> for InlineVec<T, N> {
            fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
                Self::from_vec(iter.into_iter().collect())
            }
        }

        impl<T, const N: usize> From<Vec<T>> for InlineVec<T, N> {
            fn from(elements: Vec<T>) -> Self {
                Self::from_vec(elements)
            }
        }

        impl<'a, T, const N: usize> IntoIterator for &'a InlineVec<T, N> {
            type Item = &'a T;
            type IntoIter = std::slice::Iter<'a, T>;
            fn into_iter(self) -> Self::IntoIter {
                self.iter()
            }
        }

        impl<T: Serialize, const N: usize> Serialize for InlineVec<T, N> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.as_slice().serialize(s)
            }
        }

        impl<T, const N: usize> EmptyCollection for InlineVec<T, N> {
            fn is_empty_collection(&self) -> bool {
                self.is_empty()
            }
        }

        impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for InlineVec<T, N> {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Visitor<T, const N: usize>(std::marker::PhantomData<T>);
                impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Visitor<T, N> {
                    type Value = InlineVec<T, N>;

                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a sequence")
                    }

                    fn visit_seq<A: serde::de::SeqAccess<'de>>(
                        self,
                        mut access: A,
                    ) -> Result<Self::Value, A::Error> {
                        // Fill the inline buffer first, so that sequences short
                        // enough to fit -- the common case -- never allocate.
                        // Kept as a live `InlineVec` throughout so that an error
                        // partway through still drops the elements read so far.
                        let mut result = InlineVec::<T, N>::default();
                        let InlineVecRepr::Inline { buffer, len } = &mut result.0 else {
                            unreachable!("just constructed as inline")
                        };
                        while (*len as usize) < N {
                            let Some(element) = access.next_element()? else {
                                return Ok(result);
                            };
                            buffer[*len as usize].write(element);
                            *len += 1;
                        }
                        let Some(overflow) = access.next_element()? else {
                            return Ok(result);
                        };
                        let mut elements = Vec::with_capacity(N * 2);
                        for slot in buffer.iter_mut().take(*len as usize) {
                            // SAFETY: slots below `len` are initialized. `len`
                            // is cleared below so they are not dropped twice.
                            elements.push(unsafe { slot.assume_init_read() });
                        }
                        *len = 0;
                        elements.push(overflow);
                        while let Some(element) = access.next_element()? {
                            elements.push(element);
                        }
                        Ok(InlineVec(InlineVecRepr::Spilled(
                            elements.into_boxed_slice(),
                        )))
                    }
                }
                d.deserialize_seq(Visitor(std::marker::PhantomData))
            }
        }
    )
}

/// The `SortedMap` support type emitted alongside generated definitions.
///
/// Schema "map" shapes are typically small and read-only once parsed, where a
/// hash map costs far more inline and heap space than a contiguous sorted
/// slice. Entries are kept sorted so lookup is a binary search.
fn sorted_map_definition() -> TokenStream {
    quote!(
        /// A compact, immutable map stored as a sorted `Box<[(K, V)]>`.
        ///
        /// Sixteen bytes inline rather than a hash map's forty-eight, with no
        /// spare capacity and no per-lookup hashing.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct SortedMap<K, V>(Box<[(K, V)]>);

        impl<K, V> Default for SortedMap<K, V> {
            fn default() -> Self {
                Self(Box::default())
            }
        }

        impl<K: Ord, V> SortedMap<K, V> {
            /// Builds a map from `entries`, sorting them and keeping the first
            /// value for any duplicated key.
            pub fn from_vec(mut entries: Vec<(K, V)>) -> Self {
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                entries.dedup_by(|a, b| a.0 == b.0);
                Self(entries.into_boxed_slice())
            }

            /// Returns the value for `key`, if present.
            pub fn get<Q>(&self, key: &Q) -> Option<&V>
            where
                K: std::borrow::Borrow<Q>,
                Q: Ord + ?Sized,
            {
                self.0
                    .binary_search_by(|(k, _)| k.borrow().cmp(key))
                    .ok()
                    .map(|index| &self.0[index].1)
            }

            /// Returns whether `key` is present.
            pub fn contains_key<Q>(&self, key: &Q) -> bool
            where
                K: std::borrow::Borrow<Q>,
                Q: Ord + ?Sized,
            {
                self.get(key).is_some()
            }
        }

        impl<K, V, Q> std::ops::Index<&Q> for SortedMap<K, V>
        where
            K: Ord + std::borrow::Borrow<Q>,
            Q: Ord + ?Sized,
        {
            type Output = V;

            fn index(&self, key: &Q) -> &V {
                self.get(key).expect("no entry found for key")
            }
        }

        impl<K, V> SortedMap<K, V> {
            /// Returns the number of entries.
            pub fn len(&self) -> usize {
                self.0.len()
            }

            /// Returns whether the map is empty.
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            /// Iterates over entries in key order.
            pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
                self.0.iter().map(|(key, value)| (key, value))
            }

            /// Iterates over keys in order.
            pub fn keys(&self) -> impl Iterator<Item = &K> {
                self.0.iter().map(|(key, _)| key)
            }

            /// Iterates over values in key order.
            pub fn values(&self) -> impl Iterator<Item = &V> {
                self.0.iter().map(|(_, value)| value)
            }
        }

        impl<K, V> EmptyCollection for SortedMap<K, V> {
            fn is_empty_collection(&self) -> bool {
                self.is_empty()
            }
        }

        impl<'a, K, V> IntoIterator for &'a SortedMap<K, V> {
            type Item = (&'a K, &'a V);
            type IntoIter =
                std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;
            fn into_iter(self) -> Self::IntoIter {
                self.0.iter().map(|(key, value)| (key, value))
            }
        }

        impl<K: Ord, V> FromIterator<(K, V)> for SortedMap<K, V> {
            fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
                Self::from_vec(iter.into_iter().collect())
            }
        }

        impl<K: Serialize, V: Serialize> Serialize for SortedMap<K, V> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                let mut map = s.serialize_map(Some(self.0.len()))?;
                for (key, value) in self.0.iter() {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }

        impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Deserialize<'de> for SortedMap<K, V> {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Visitor<K, V>(std::marker::PhantomData<(K, V)>);
                impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> serde::de::Visitor<'de>
                    for Visitor<K, V>
                {
                    type Value = SortedMap<K, V>;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a map")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(
                        self,
                        mut access: A,
                    ) -> Result<Self::Value, A::Error> {
                        let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(4));
                        while let Some(entry) = access.next_entry()? {
                            entries.push(entry);
                        }
                        Ok(SortedMap::from_vec(entries))
                    }
                }
                d.deserialize_map(Visitor(std::marker::PhantomData))
            }
        }
    )
}

fn render_union(definition: &crate::types::UnionDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let variant_name = parse_str::<syn::Ident>(&variant.name)
                .map_err(|e| format!("union {} variant {}: {e}", definition.name, variant.name))?;
            let ty = parse_str::<syn::Type>(&variant.ty.to_string()).map_err(|e| e.to_string())?;
            Ok::<_, String>(quote!(#variant_name(#ty)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let default_variant = definition
        .variants
        .first()
        .ok_or_else(|| format!("union {} has no variants", definition.name))?;
    let default_name = parse_str::<syn::Ident>(&default_variant.name).map_err(|e| e.to_string())?;
    Ok(quote!(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(untagged)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default_name(Default::default()) }
        }
    ))
}

fn render_definition(definition: &StructDef) -> Result<TokenStream, String> {
    if definition.is_object {
        return render_struct(definition);
    }
    let name = parse_str::<syn::Ident>(&definition.name)
        .map_err(|e| format!("enum {}: {e}", definition.name))?;
    let ty = parse_str::<syn::Type>(&definition.alias.as_ref().unwrap().to_string())
        .map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|doc| {
            let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #literal])
        })
        .unwrap_or_default();
    Ok(quote!(#docs pub type #name = #ty;))
}

fn render_schema_enum(definition: &crate::types::EnumDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|doc| {
            let lit = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #lit])
        })
        .unwrap_or_default();
    if definition.open {
        return render_open_enum(definition, &name, &docs);
    }
    let variants = definition
        .variants
        .iter()
        .map(|variant| {
            let name = parse_str::<syn::Ident>(&variant.name)
                .map_err(|e| format!("enum {} variant {}: {e}", definition.name, variant.name))?;
            match &variant.value {
                serde_json::Value::String(value) => {
                    let value = syn::LitStr::new(value, proc_macro2::Span::call_site());
                    Ok::<_, String>(quote!(#[serde(rename = #value)] #name))
                }
                serde_json::Value::Number(value) => {
                    let value = value.as_i64().ok_or_else(|| {
                        format!("enum {} variant {name} is not an integer", definition.name)
                    })?;
                    let value = proc_macro2::Literal::i64_unsuffixed(value);
                    Ok(quote!(#name = #value))
                }
                _ => Err("schema enum value must be a string or number".to_string()),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let default_variant = definition
        .variants
        .first()
        .ok_or_else(|| format!("enum {} has no variants", definition.name))?;
    let default_name = parse_str::<syn::Ident>(&default_variant.name).map_err(|e| e.to_string())?;

    // Numeric enums need a repr and an integer-based serde bridge; deriving
    // Serialize/Deserialize directly would produce variant *names* in the JSON.
    let (repr, serde_derive, serde_impl) = if definition.numeric {
        let arms_to = definition
            .variants
            .iter()
            .map(|variant| {
                let ident = parse_str::<syn::Ident>(&variant.name).unwrap();
                quote!(Self::#ident => Self::#ident as i64)
            })
            .collect::<Vec<_>>();
        let arms_from = definition
            .variants
            .iter()
            .map(|variant| {
                let ident = parse_str::<syn::Ident>(&variant.name).unwrap();
                quote!(v if v == Self::#ident as i64 => Ok(Self::#ident))
            })
            .collect::<Vec<_>>();
        let expected = format!("a valid {} value", definition.name);
        (
            quote!(#[repr(i64)]),
            quote!(),
            quote!(
                impl #name {
                    /// Returns the underlying schema value.
                    pub fn value(self) -> i64 {
                        match self { #(#arms_to),* }
                    }
                    /// Converts a raw schema value into this enum.
                    pub fn from_value(value: i64) -> Result<Self, i64> {
                        match value { #(#arms_from),*, other => Err(other) }
                    }
                }
                impl Serialize for #name {
                    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                        s.serialize_i64(self.value())
                    }
                }
                impl<'de> Deserialize<'de> for #name {
                    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                        let value = i64::deserialize(d)?;
                        Self::from_value(value).map_err(|value| {
                            serde::de::Error::invalid_value(
                                serde::de::Unexpected::Signed(value),
                                &#expected,
                            )
                        })
                    }
                }
            ),
        )
    } else {
        (quote!(), quote!(Serialize, Deserialize,), quote!())
    };

    Ok(quote!(
        #docs
        #repr
        #[derive(Debug, Clone, Copy, PartialEq, Eq, #serde_derive)]
        pub enum #name { #(#variants),* }
        impl Default for #name {
            fn default() -> Self { Self::#default_name }
        }
        #serde_impl
    ))
}

/// Renders an open value set.
///
/// The schema lists known values but explicitly permits others, so a closed
/// Rust enum would reject valid documents. Two shapes are used:
///
/// - Numeric sets become a transparent newtype over `i64` with associated
///   constants. This is the same size as the raw primitive and makes serde a
///   pure passthrough with no match table.
/// - String sets become an enum whose known variants are fieldless and whose
///   `Other` variant boxes the unrecognised string. Known values then cost no
///   allocation at all, which a `Cow`-based newtype could not achieve.
///
/// Either way unrecognised values round-trip losslessly.
fn render_open_enum(
    definition: &crate::types::EnumDef,
    name: &syn::Ident,
    docs: &TokenStream,
) -> Result<TokenStream, String> {
    let open_note = quote!(
        ///
        /// The schema permits values beyond those listed, so unrecognised
        /// values are preserved rather than rejected.
    );
    if !definition.numeric {
        return render_open_string_enum(definition, name, docs, &open_note);
    }

    let constants = definition
        .variants
        .iter()
        .map(|variant| {
            let const_name = parse_str::<syn::Ident>(&to_shouty_snake_case(&variant.name))
                .map_err(|e| format!("enum {} variant {}: {e}", definition.name, variant.name))?;
            let doc = syn::LitStr::new(
                &format!("Schema value `{}`.", variant.value),
                proc_macro2::Span::call_site(),
            );
            let value = variant.value.as_i64().ok_or_else(|| {
                format!(
                    "enum {} variant {const_name} is not an integer",
                    definition.name
                )
            })?;
            let value = proc_macro2::Literal::i64_unsuffixed(value);
            Ok::<_, String>(quote!(#[doc = #doc] pub const #const_name: Self = Self(#value);))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let default_variant = definition
        .variants
        .first()
        .ok_or_else(|| format!("enum {} has no variants", definition.name))?;
    let default_const = parse_str::<syn::Ident>(&to_shouty_snake_case(&default_variant.name))
        .map_err(|e| e.to_string())?;

    let known_constants = definition
        .variants
        .iter()
        .map(|variant| parse_str::<syn::Ident>(&to_shouty_snake_case(&variant.name)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(quote!(
        #docs
        #open_note
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize
        )]
        #[serde(transparent)]
        pub struct #name(pub i64);

        impl #name {
            #(#constants)*

            /// Returns true when the value is one of the schema's named constants.
            pub fn is_known(&self) -> bool {
                [#(Self::#known_constants),*].iter().any(|known| known == self)
            }
        }

        impl Default for #name {
            fn default() -> Self { Self::#default_const }
        }
    ))
}

/// Renders an open *string* value set as an enum with a boxed `Other` variant.
///
/// Known values are fieldless, so the common case allocates nothing and the
/// type stays 16 bytes. A `Cow`-based newtype would be 24 bytes and force an
/// allocation for every value parsed from a document.
fn render_open_string_enum(
    definition: &crate::types::EnumDef,
    name: &syn::Ident,
    docs: &TokenStream,
    open_note: &TokenStream,
) -> Result<TokenStream, String> {
    let mut variant_idents = Vec::new();
    let mut variant_values = Vec::new();
    for variant in &definition.variants {
        variant_idents.push(
            parse_str::<syn::Ident>(&variant.name)
                .map_err(|e| format!("enum {} variant {}: {e}", definition.name, variant.name))?,
        );
        let value = variant
            .value
            .as_str()
            .ok_or_else(|| format!("enum {} variant value must be a string", definition.name))?;
        variant_values.push(syn::LitStr::new(value, proc_macro2::Span::call_site()));
    }

    let default_ident = variant_idents
        .first()
        .ok_or_else(|| format!("enum {} has no variants", definition.name))?
        .clone();

    Ok(quote!(
        #docs
        #open_note
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum #name {
            #(
                #[doc = #variant_values]
                #variant_idents,
            )*
            /// A value not listed in the schema, preserved verbatim.
            ///
            /// The payload is boxed so the enum stays pointer-sized (16 bytes)
            /// rather than paying the full `String` width in every value.
            Other(Box<String>),
        }

        impl #name {
            /// Returns the value's schema representation.
            pub fn as_str(&self) -> &str {
                match self {
                    #(Self::#variant_idents => #variant_values,)*
                    Self::Other(value) => value,
                }
            }

            /// Returns true when the value is one of the schema's named variants.
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Other(_))
            }
        }

        impl From<&str> for #name {
            fn from(value: &str) -> Self {
                match value {
                    #(#variant_values => Self::#variant_idents,)*
                    other => Self::Other(Box::new(other.to_owned())),
                }
            }
        }

        impl Default for #name {
            fn default() -> Self { Self::#default_ident }
        }

        impl Serialize for #name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for #name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl serde::de::Visitor<'_> for Visitor {
                    type Value = #name;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a string")
                    }
                    // Borrowed input avoids allocating for known values.
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<#name, E> {
                        Ok(#name::from(value))
                    }
                    // Owned input reuses the allocation for unknown values.
                    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<#name, E> {
                        Ok(match value.as_str() {
                            #(#variant_values => #name::#variant_idents,)*
                            _ => #name::Other(Box::new(value)),
                        })
                    }
                }
                d.deserialize_str(Visitor)
            }
        }
    ))
}

/// Converts a PascalCase or SCREAMING_SNAKE name into SCREAMING_SNAKE_CASE.
fn to_shouty_snake_case(name: &str) -> String {
    use heck::ToShoutySnakeCase;
    name.to_shouty_snake_case()
}

fn render_struct(definition: &StructDef) -> Result<TokenStream, String> {
    let name = parse_str::<syn::Ident>(&definition.name).map_err(|e| e.to_string())?;
    let docs = definition
        .description
        .as_deref()
        .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
    let docs = docs
        .map(|value| quote!(#[doc = #value]))
        .unwrap_or_default();
    let mut default_functions = Vec::new();
    let fields: Vec<TokenStream> = definition
        .fields
        .iter()
        .map(|field| {
            let field_name =
                parse_str::<syn::Ident>(&field.rust_name).map_err(|e| e.to_string())?;
            let type_source = field.ty.to_string();
            let ty = parse_str::<syn::Type>(&type_source)
                .map_err(|e| format!("{} ({type_source}): {e}", field.json_name))?;
            let json_name = syn::LitStr::new(&field.json_name, proc_macro2::Span::call_site());
            let field_doc = field
                .description
                .as_deref()
                .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
            let rename = if field.rust_name != field.json_name {
                quote!(rename = #json_name,)
            } else {
                quote!()
            };
            let default = if let Some(default) = field.default.as_ref() {
                let function_name = syn::Ident::new(
                    &format!(
                        "default_{}_{}",
                        definition.name.to_snake_case(),
                        field.rust_name.trim_start_matches("r#").to_snake_case()
                    ),
                    proc_macro2::Span::call_site(),
                );
                let expression = default_expression(default, &field.ty)?;
                let return_type = ty.clone();
                default_functions.push(quote!(fn #function_name() -> #return_type { #expression }));
                let function_name_literal =
                    syn::LitStr::new(&function_name.to_string(), proc_macro2::Span::call_site());
                quote!(default = #function_name_literal,)
            } else if field.required {
                quote!()
            } else if field.ty.to_string().starts_with("Option<") {
                quote!(default, skip_serializing_if = "Option::is_none",)
            } else if omits_when_empty(&field.ty) {
                // An empty collection means the property was absent, so writing
                // it back out as `[]` or `{}` would not round-trip.
                let predicate =
                    syn::LitStr::new("is_empty_collection", proc_macro2::Span::call_site());
                quote!(default, skip_serializing_if = #predicate,)
            } else {
                quote!(default,)
            };
            let skip = field
                .skip_serializing_if
                .as_deref()
                .map(|value| syn::LitStr::new(value, proc_macro2::Span::call_site()));
            let skip = skip
                .map(|value| quote!(skip_serializing_if = #value,))
                .unwrap_or_default();
            let flatten = if field.flatten {
                quote!(flatten,)
            } else {
                Default::default()
            };
            // Deriving `Box<[T]>` allocates twice: once to grow a `Vec` and
            // again to shrink it to fit. The helper sizes its allocation
            // exactly, so it is always at least as cheap. Other optional
            // collections take the `null`-tolerant helper for the reason
            // given on `deserialize_null_default`.
            let deserialize_with = if matches!(field.ty, RustType::BoxedSlice(_)) {
                let helper =
                    syn::LitStr::new("deserialize_boxed_slice", proc_macro2::Span::call_site());
                quote!(deserialize_with = #helper,)
            } else if !field.required && field.default.is_none() && omits_when_empty(&field.ty) {
                let helper =
                    syn::LitStr::new("deserialize_null_default", proc_macro2::Span::call_site());
                quote!(deserialize_with = #helper,)
            } else {
                quote!()
            };
            let serde = if field.flatten {
                quote!(#[serde(default, #flatten)])
            } else if field.required
                && field.default.is_none()
                && field.rust_name == field.json_name
                && deserialize_with.is_empty()
            {
                quote!()
            } else {
                quote!(#[serde(#rename #default #skip #flatten #deserialize_with)])
            };
            let docs = field_doc
                .map(|value| quote!(#[doc = #value]))
                .unwrap_or_default();
            Ok(quote! {
                #docs
                #serde
                pub #field_name: #ty,
            })
        })
        .collect::<Result<_, String>>()?;
    let extra_fields = definition
        .extra_fields
        .iter()
        .map(|field| {
            let name = parse_str::<syn::Ident>(&field.name).map_err(|e| e.to_string())?;
            let ty = parse_str::<syn::Type>(&field.rust_type).map_err(|e| e.to_string())?;
            let docs = field
                .description
                .as_deref()
                .map(|doc| {
                    let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
                    quote!(#[doc = #literal])
                })
                .unwrap_or_default();
            let serde = if field.skip_serde {
                quote!(#[serde(skip)])
            } else {
                quote!(#[serde(default)])
            };
            Ok::<TokenStream, String>(quote!(#docs #serde pub #name: #ty,))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let extension_fields = if definition.extensions {
        quote!(#[doc = "Extension-specific data."] #[serde(default)] pub extensions: std::collections::HashMap<String, serde_json::Value>, #[doc = "Application-specific data."] pub extras: Option<serde_json::Value>,)
    } else {
        quote!()
    };
    Ok(quote! {
        #docs
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct #name { #(#fields)* #(#extra_fields)* #extension_fields }
        #(#default_functions)*
    })
}

fn default_expression(value: &serde_json::Value, ty: &RustType) -> Result<TokenStream, String> {
    if let RustType::Option(inner) = ty {
        let expression = default_expression(value, inner)?;
        return Ok(quote!(Some(#expression)));
    }
    if let RustType::Boxed(inner) = ty {
        let expression = default_expression(value, inner)?;
        return Ok(quote!(Box::new(#expression)));
    }
    if let RustType::Array(inner, _) | RustType::Vec(inner) = ty {
        let expressions = value
            .as_array()
            .ok_or_else(|| "array default must be a JSON array".to_string())?
            .iter()
            .map(|value| default_expression(value, inner))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote!([#(#expressions),*]));
    }
    if let RustType::BoxedSlice(inner) = ty {
        let expressions = value
            .as_array()
            .ok_or_else(|| "array default must be a JSON array".to_string())?
            .iter()
            .map(|value| default_expression(value, inner))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote!(Box::new([#(#expressions),*])));
    }
    if matches!(ty, RustType::Named(_)) {
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        let literal = syn::LitStr::new(&json, proc_macro2::Span::call_site());
        return Ok(quote!(serde_json::from_str(#literal).expect("valid generated JSON default")));
    }
    if matches!(ty, RustType::Value) {
        if let Some(string) = value.as_str() {
            let literal = syn::LitStr::new(string, proc_macro2::Span::call_site());
            return Ok(quote!(serde_json::Value::String(#literal.to_owned())));
        }
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        let literal = syn::LitStr::new(&json, proc_macro2::Span::call_site());
        return Ok(quote!(serde_json::from_str(#literal).expect("valid generated JSON default")));
    }
    if let Some(value) = value.as_str() {
        let literal = syn::LitStr::new(value, proc_macro2::Span::call_site());
        if matches!(ty, RustType::String) {
            return Ok(quote!(#literal.to_string()));
        }
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_bool() {
        return Ok(quote!(#value));
    }
    if let Some(value) = value.as_i64() {
        if matches!(ty, RustType::F64 | RustType::F32) {
            let literal = syn::LitFloat::new(&format!("{value}.0"), proc_macro2::Span::call_site());
            return Ok(quote!(#literal));
        }
        let literal = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_u64() {
        if matches!(ty, RustType::F64 | RustType::F32) {
            let literal = syn::LitFloat::new(&format!("{value}.0"), proc_macro2::Span::call_site());
            return Ok(quote!(#literal));
        }
        let literal = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(value) = value.as_f64() {
        let literal = syn::LitFloat::new(&format!("{value:?}"), proc_macro2::Span::call_site());
        return Ok(quote!(#literal));
    }
    if let Some(values) = value.as_array() {
        let expressions = values
            .iter()
            .map(|value| default_expression(value, ty))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(quote!([#(#expressions),*]));
    }
    Ok(quote!(Default::default()))
}

fn render_custom_type(
    type_name: &str,
    config: &crate::config::CustomTypeConfig,
) -> Result<TokenStream, String> {
    if config.kind != "enum" {
        return Err(format!("unsupported custom type kind: {}", config.kind));
    }
    let name =
        parse_str::<syn::Ident>(&type_name.to_upper_camel_case()).map_err(|e| e.to_string())?;
    let variants = config
        .variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let variant_name = variant.to_upper_camel_case();
            let variant_ident =
                parse_str::<syn::Ident>(&variant_name).map_err(|e| e.to_string())?;
            if config.numeric {
                let value = config
                    .numeric_values
                    .get(variant)
                    .copied()
                    .unwrap_or(index as u32);
                let value = syn::LitInt::new(&value.to_string(), proc_macro2::Span::call_site());
                Ok(quote!(#variant_ident = #value))
            } else {
                let value = syn::LitStr::new(variant, proc_macro2::Span::call_site());
                Ok(quote!(#[serde(rename = #value)] #variant_ident))
            }
        })
        .collect::<Result<Vec<TokenStream>, String>>()?;
    let default_variant = config
        .variants
        .first()
        .map(|variant| parse_str::<syn::Ident>(&variant.to_upper_camel_case()))
        .transpose()
        .map_err(|error| error.to_string())?;
    let default_impl = default_variant
        .map(|variant| quote!(impl Default for #name { fn default() -> Self { Self::#variant } }))
        .unwrap_or_default();
    let docs = config
        .doc
        .as_deref()
        .map(|doc| {
            let literal = syn::LitStr::new(doc, proc_macro2::Span::call_site());
            quote!(#[doc = #literal])
        })
        .unwrap_or_default();
    let derives = if config.numeric {
        quote!(#[repr(u32)] #[derive(Debug, Clone, Copy, PartialEq, Eq, serde_repr::Serialize_repr, serde_repr::Deserialize_repr)])
    } else {
        quote!(#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)])
    };
    Ok(quote!(#docs #derives pub enum #name { #(#variants),* } #default_impl))
}
