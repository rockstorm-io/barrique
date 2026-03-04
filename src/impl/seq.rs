use crate::decode::{get, Decode, DecodeBearer, DecodeError};
use crate::encode::{Encode, EncodeBearer, EncodeError};

use alloc::collections::{BTreeMap, BTreeSet, LinkedList};
use core::mem::MaybeUninit;

macro_rules! impl_map {
    ($type:ident<K: $($key_constraints:ident)|*, V: $($value_constraints:ident)*>, $constructor:expr) => {
        impl<K, V> Encode for $type<K, V>
        where
            K: $($key_constraints +)* Encode,
            V: $($value_constraints +)* Encode,
        {
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                u32::encode(bearer, &(src.len() as u32))?;
                for (key, value) in src.iter() {
                    K::encode(bearer, key)?;
                    V::encode(bearer, value)?;
                }

                Ok(())
            }

            fn size_of(&self) -> usize {
                self.iter().map(|(k, v)| k.size_of() + v.size_of()).sum()
            }
        }

        unsafe impl<K, V> Decode for $type<K, V>
        where
            K: $($key_constraints +)* Decode,
            V: $($value_constraints +)* Decode,
        {
            fn decode(bearer: &mut impl DecodeBearer, dst: &mut MaybeUninit<Self>) -> Result<(), DecodeError> {
                let len = get::<u32>(bearer)? as usize;

                let mut result = $constructor(len);
                for _ in 0..len {
                    let key = get(bearer)?;
                    let value = get(bearer)?;
                    result.insert(key, value);
                }

                dst.write(result);
                Ok(())
            }
        }
    };
}

impl_map!(BTreeMap<K: Ord, V:>, |_| BTreeMap::new());

macro_rules! impl_seq {
    ($type:ident<T: $($constraints:ident)|*>, $insert:ident, $constructor:expr) => {
        impl<T> Encode for $type<T>
        where
            T: $($constraints +)* Encode,
        {
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                u32::encode(bearer, &(src.len() as u32))?;
                for value in src.iter() {
                    T::encode(bearer, value)?;
                }

                Ok(())
            }

            fn size_of(&self) -> usize {
                self.iter().map(Encode::size_of).sum()
            }
        }

        unsafe impl<T> Decode for $type<T>
        where
            T: $($constraints +)* Decode,
        {
            fn decode(bearer: &mut impl DecodeBearer, dst: &mut MaybeUninit<Self>) -> Result<(), DecodeError> {
                let len = get::<u32>(bearer)? as usize;
                let mut result = $constructor(len);

                for _ in 0..len {
                    result.$insert(get(bearer)?);
                }

                dst.write(result);
                Ok(())
            }
        }
    };
}

impl_seq!(LinkedList<T: >, push_back, |_| LinkedList::new());
impl_seq!(BTreeSet<T: Ord>, insert, |_| BTreeSet::new());

#[cfg(feature = "std")]
mod featured {
    use super::*;

    use std::collections::{HashMap, HashSet};
    use std::hash::Hash;

    impl_map!(HashMap<K: Hash | Eq, V: Eq>, HashMap::with_capacity);
    impl_seq!(HashSet<T: Hash | Eq>, insert, HashSet::with_capacity);
}
