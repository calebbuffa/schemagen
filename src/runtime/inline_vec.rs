/// A sequence storing up to `N` elements inline, spilling to the heap only when
/// longer.
///
/// Schema arrays are often short and fixed-shape, and a heap allocation per
/// value dominates both footprint and parse time once the array count scales
/// with document size. Deserialization fills the inline buffer directly, so
/// short values cost no allocation at all. A `Box<[T]>` by contrast costs two
/// for short input: one to grow a `Vec` and another to shrink it to fit.
///
/// The inline buffer and the spill pointer are mutually exclusive and share
/// storage, so the inline capacity costs only the bytes by which it exceeds a
/// slice pointer (16 bytes), rather than being added on top of one.
///
/// This trades struct size for allocation count, so it only pays off when
/// nearly every value of the containing type carries the field. When the field
/// is frequently absent, prefer `Box<[T]>`.
///
/// `N` must not exceed 255, which is checked when a value is built.
pub struct InlineVec<T, const N: usize>(InlineVecRepr<T, N>);

/// Storage for [`InlineVec`].
///
/// Private: the `Inline` variant carries the invariant that its leading `len`
/// slots are initialized, which safe code must not be able to violate by
/// constructing the variant directly.
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
    const CAPACITY_FITS_LENGTH: () = assert!(
        N <= u8::MAX as usize,
        "InlineVec capacity must not exceed 255"
    );

    /// Returns an empty value.
    pub fn new() -> Self {
        let () = Self::CAPACITY_FITS_LENGTH;
        Self(InlineVecRepr::Inline {
            buffer: [const { std::mem::MaybeUninit::uninit() }; N],
            len: 0,
        })
    }

    /// Returns the elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        match &self.0 {
            InlineVecRepr::Spilled(spilled) => spilled,
            // SAFETY: the leading `len` slots are initialized by construction,
            // and `MaybeUninit<T>` shares `T`'s layout.
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
                std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<T>(), *len as usize)
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

    /// Builds a value from `elements`, storing them inline when short enough.
    /// When spilling, the `Vec`'s allocation is reused.
    pub fn from_vec(elements: Vec<T>) -> Self {
        let () = Self::CAPACITY_FITS_LENGTH;
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
            // SAFETY: the leading `len` slots are initialized, and `drop` runs
            // at most once per value.
            unsafe {
                std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
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
        Self::new()
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
                // Fill the inline buffer first, so that sequences short enough
                // to fit -- the common case -- never allocate. Kept as a live
                // `InlineVec` throughout so that an error partway through
                // still drops the elements read so far.
                let mut result = InlineVec::<T, N>::new();
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
                    // SAFETY: slots below `len` are initialized. `len` is
                    // cleared below so they are not dropped twice.
                    elements.push(unsafe { slot.assume_init_read() });
                }
                *len = 0;
                elements.push(overflow);
                while let Some(element) = access.next_element()? {
                    elements.push(element);
                }
                Ok(InlineVec(InlineVecRepr::Spilled(elements.into_boxed_slice())))
            }
        }
        d.deserialize_seq(Visitor(std::marker::PhantomData))
    }
}
