/// Whether a collection-typed field should be omitted from output.
///
/// A schema property that is absent deserializes to an empty collection, so
/// writing it back out as `[]` or `{}` would not round-trip.
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
/// A schema that declares an array or object and does not require it is
/// routinely satisfied by producers writing an explicit `null` rather than
/// omitting the key. `#[serde(default)]` only covers the omitted case, so
/// without this a conforming document is rejected.
fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
