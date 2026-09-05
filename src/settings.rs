//! How schema primitives are mapped onto Rust types.
//!
//! JSON Schema describes value *domains*, not machine representations: it has
//! one `number`, one `integer`, and one `string`, with no notion of width or
//! ownership. Every consumer therefore has to state how those domains should
//! be spelled in Rust, and the answer depends on the consumer's runtime model
//! rather than on anything in the schema.
//!
//! These choices are expressed as an enumeration per decision rather than as
//! free-form strings, so an unsupported value is a compile error at the call
//! site instead of a silent fallback to the default. Settings are supplied
//! programmatically through [`GenerationPolicy::settings`] — see the module
//! documentation on [`crate::policy`] for why they live there and not in the
//! data-driven [`Config`].
//!
//! [`GenerationPolicy::settings`]: crate::policy::GenerationPolicy::settings
//! [`Config`]: crate::config::Config

use crate::types::RustType;

/// The Rust type that schema `number` properties map to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FloatWidth {
    /// Single precision. Appropriate when the schema's values are
    /// semantically 32-bit — vertex positions, colour factors, and so on —
    /// which would otherwise require a per-property override on every one.
    Single,
    /// Double precision, matching JSON's own number model.
    #[default]
    Double,
}

impl FloatWidth {
    pub fn rust_type(self) -> RustType {
        match self {
            FloatWidth::Single => RustType::F32,
            FloatWidth::Double => RustType::F64,
        }
    }
}

/// The width that unbounded schema `integer` properties map to.
///
/// This is only the *default*. Explicit `minimum`/`maximum` constraints in the
/// schema are the more specific statement about a value and still win, so a
/// property that provably needs 64 bits keeps them. The unsigned/signed choice
/// is likewise driven by the schema, not by this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegerWidth {
    /// 32 bits. Appropriate when the schema's integers are array indices,
    /// counts, or byte offsets, which would otherwise pay 64 bits each.
    Bits32,
    /// 64 bits, the widest value JSON can round-trip exactly.
    #[default]
    Bits64,
}

impl IntegerWidth {
    pub fn unsigned(self) -> RustType {
        match self {
            IntegerWidth::Bits32 => RustType::U32,
            IntegerWidth::Bits64 => RustType::U64,
        }
    }

    pub fn signed(self) -> RustType {
        match self {
            IntegerWidth::Bits32 => RustType::I32,
            IntegerWidth::Bits64 => RustType::I64,
        }
    }
}

/// The Rust type that schema `string` properties map to.
///
/// Map *keys* are unaffected by this choice — they stay `String` so that
/// serde's map key handling and `Borrow<str>` lookups continue to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StringRepr {
    /// A growable `String`.
    #[default]
    Owned,
    /// An immutable `Box<str>`. Parsed schema data is read-mostly, so a
    /// `String`'s spare capacity is dead weight: `Box<str>` allocates exactly
    /// the bytes needed, and an `Option<Box<str>>` is sixteen bytes inline
    /// against a `String`'s twenty-four.
    Boxed,
}

impl StringRepr {
    pub fn rust_type(self) -> RustType {
        match self {
            StringRepr::Owned => RustType::String,
            StringRepr::Boxed => RustType::BoxedStr,
        }
    }
}

/// How schema objects with homogeneous values ("map" shapes) are represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapRepr {
    /// A `std::collections::HashMap`.
    #[default]
    HashMap,
    /// A sorted `Box<[(K, V)]>`, substantially smaller both inline and on the
    /// heap for the small, read-mostly maps that schemas typically describe.
    /// Lookups become a binary search rather than a hash.
    ///
    /// Not usable for `#[serde(flatten)]` fields, which need a real map
    /// deserializer.
    SortedSlice,
}

impl MapRepr {
    pub fn rust_type(self, key: RustType, value: RustType) -> RustType {
        match self {
            MapRepr::HashMap => RustType::Map(Box::new(key), Box::new(value)),
            MapRepr::SortedSlice => RustType::SortedMap(Box::new(key), Box::new(value)),
        }
    }
}

/// The complete set of primitive-mapping decisions for one generation run.
///
/// Every field defaults to the most conservative choice, so a consumer that
/// has no opinion can use [`TypeSettings::default`] and get types that mirror
/// JSON's own model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeSettings {
    pub number: FloatWidth,
    pub integer: IntegerWidth,
    pub string: StringRepr,
    pub map: MapRepr,

    /// Generate a Rust enum for properties that list a closed set of `const`
    /// values alongside an open primitive fallback branch.
    ///
    /// Schemas commonly write `anyOf: [{const: 5121}, ..., {type: "integer"}]`
    /// to keep a property extensible. By default that fallback wins and the
    /// property becomes a bare primitive, forcing every consumer to re-derive
    /// the value mapping by hand. Enable this to model the closed variants
    /// instead. Values outside the set then become deserialization errors, so
    /// it suits a schema whose extensibility is nominal.
    pub infer_open_enums: bool,
}

impl TypeSettings {
    /// Settings that mirror JSON's own value model: `f64`, 64-bit integers,
    /// `String`, and `HashMap`.
    pub fn json_like() -> Self {
        Self::default()
    }

    /// Settings tuned for a compact in-memory representation of read-mostly
    /// data: boxed strings and sorted-slice maps, keeping JSON's numeric
    /// widths so no value is silently narrowed.
    ///
    /// Numeric width is deliberately left alone because narrowing it is only
    /// sound when the consumer knows its schema's value ranges; the storage
    /// choices here are lossless for any schema.
    pub fn compact() -> Self {
        Self {
            string: StringRepr::Boxed,
            map: MapRepr::SortedSlice,
            ..Self::default()
        }
    }

    pub fn with_number(mut self, number: FloatWidth) -> Self {
        self.number = number;
        self
    }

    pub fn with_integer(mut self, integer: IntegerWidth) -> Self {
        self.integer = integer;
        self
    }

    pub fn with_string(mut self, string: StringRepr) -> Self {
        self.string = string;
        self
    }

    pub fn with_map(mut self, map: MapRepr) -> Self {
        self.map = map;
        self
    }

    pub fn inferring_open_enums(mut self) -> Self {
        self.infer_open_enums = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_mirror_the_json_value_model() {
        let settings = TypeSettings::default();
        assert_eq!(settings.number.rust_type(), RustType::F64);
        assert_eq!(settings.integer.unsigned(), RustType::U64);
        assert_eq!(settings.string.rust_type(), RustType::String);
        assert!(matches!(
            settings.map.rust_type(RustType::String, RustType::Value),
            RustType::Map(_, _)
        ));
        assert!(!settings.infer_open_enums);
    }

    #[test]
    fn compact_changes_storage_without_narrowing_values() {
        let settings = TypeSettings::compact();
        assert_eq!(settings.string.rust_type(), RustType::BoxedStr);
        assert!(matches!(
            settings.map.rust_type(RustType::String, RustType::Value),
            RustType::SortedMap(_, _)
        ));
        // A narrowing change would be lossy, so `compact` must not make it.
        assert_eq!(settings.number.rust_type(), RustType::F64);
        assert_eq!(settings.integer.unsigned(), RustType::U64);
    }

    #[test]
    fn builders_compose() {
        let settings = TypeSettings::compact()
            .with_number(FloatWidth::Single)
            .with_integer(IntegerWidth::Bits32)
            .inferring_open_enums();
        assert_eq!(settings.number.rust_type(), RustType::F32);
        assert_eq!(settings.integer.signed(), RustType::I32);
        assert_eq!(settings.string.rust_type(), RustType::BoxedStr);
        assert!(settings.infer_open_enums);
    }
}
