/// How many elements are collected on the stack before falling back to a heap
/// buffer. Sized so that schema-typical short arrays are covered while keeping
/// the stack frame small.
const BOXED_SLICE_SCRATCH: usize = 32;

/// Deserializes a sequence into an exactly-sized `Box<[T]>`.
///
/// Used via `#[serde(deserialize_with = ...)]` on `Box<[T]>` fields.
///
/// Deriving `Box<[T]>` directly costs two allocations for most lengths: serde
/// fills a growing `Vec`, then `into_boxed_slice` reallocates to drop the spare
/// capacity. Collecting into a stack buffer first makes the length known before
/// anything is allocated, so the single allocation is already exact.
fn deserialize_boxed_slice<'de, D, T>(deserializer: D) -> Result<Box<[T]>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    /// Owns the initialized prefix of the scratch buffer, so that an error
    /// partway through the sequence still drops what was read.
    struct Scratch<T> {
        buffer: [std::mem::MaybeUninit<T>; BOXED_SLICE_SCRATCH],
        len: usize,
    }

    impl<T> Scratch<T> {
        fn as_mut_slice(&mut self) -> &mut [T] {
            // SAFETY: the leading `len` slots are initialized, and
            // `MaybeUninit<T>` shares `T`'s layout.
            unsafe { std::slice::from_raw_parts_mut(self.buffer.as_mut_ptr().cast::<T>(), self.len) }
        }

        /// Moves the collected elements out, leaving the scratch empty.
        fn drain_into(&mut self, sink: &mut Vec<T>) {
            // SAFETY: the leading `len` slots are initialized. `len` is cleared
            // first, so a panic in `push` cannot drop them twice.
            let len = std::mem::replace(&mut self.len, 0);
            for slot in self.buffer.iter_mut().take(len) {
                sink.push(unsafe { slot.assume_init_read() });
            }
        }
    }

    impl<T> Drop for Scratch<T> {
        fn drop(&mut self) {
            // SAFETY: the leading `len` slots are initialized, and `drop` runs
            // at most once per value.
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
            // Longer than the scratch buffer: fall back to growing a `Vec`,
            // which still shrinks once at the end.
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
    /// A schema that declares an array and does not require it is routinely
    /// satisfied by producers writing an explicit `null` rather than omitting
    /// the key, so a deserializer that only accepts a sequence rejects
    /// conforming documents.
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
