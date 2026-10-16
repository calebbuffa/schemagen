/// A compact, immutable map stored as a sorted `Box<[(K, V)]>`.
///
/// Schema "map" shapes are typically small and read-only once parsed, where a
/// hash map costs far more inline and heap space than a contiguous sorted
/// slice. Sixteen bytes inline rather than a hash map's forty-eight, with no
/// spare capacity and no per-lookup hashing; lookup is a binary search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortedMap<K, V>(Box<[(K, V)]>);

impl<K, V> Default for SortedMap<K, V> {
    fn default() -> Self {
        Self(Box::default())
    }
}

impl<K: Ord, V> SortedMap<K, V> {
    /// Builds a map from `entries`, sorting them and keeping the first value
    /// for any duplicated key.
    pub fn from_vec(mut entries: Vec<(K, V)>) -> Self {
        // The sort is stable, so the first of equal keys is the earliest.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|later, earlier| later.0 == earlier.0);
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
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;
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
