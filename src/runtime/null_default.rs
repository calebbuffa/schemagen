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
