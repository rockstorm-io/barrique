use crate::decode::Decode;
use crate::decode::DecodeBearer;
use crate::decode::DecodeError;
use crate::encode::{Encode, EncodeBearer, EncodeError};
use crate::tuple_drop_guard;

use core::mem::MaybeUninit;

macro_rules! impl_tuple {
    ($($idx:tt $type:tt),+) => {
        impl<$($type,)+> Encode for ($($type,)+)
        where
            $($type: Encode,)+
        {
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                $(
                    <$type as Encode>::encode(bearer, &src.$idx)?;
                )*
                Ok(())
            }

            fn size_of(&self) -> usize {
                $(
                    self.$idx.size_of() +
                )+ 0
            }
        }

        const _: () = {
            struct TupleDecodingGuard<$($type,)+> {
                ptr: *mut ($($type,)+),
                count: u8
            }

            impl<$($type,)+> Drop for TupleDecodingGuard<$($type,)+> {
                #[cold]
                fn drop(&mut self) {
                    tuple_drop_guard!($($idx,)+);
                }
            }

            unsafe impl<$($type,)+> Decode for ($($type,)+)
            where
                $($type: Decode,)+
            {
                fn decode(bearer: &mut impl DecodeBearer, dst: &mut MaybeUninit<Self>) -> Result<(), DecodeError> {
                    let mut guard = TupleDecodingGuard {
                        ptr: dst.as_mut_ptr(),
                        count: 0
                    };
                    $(
                        $type::decode(bearer, unsafe {
                            &mut *(&raw mut (*dst.as_mut_ptr()).$idx).cast()
                        })?;
                        guard.count += 1;
                    )*
                    core::mem::forget(guard);
                    Ok(())
                }
            }
        };
    }
}

impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E);
impl_tuple!(0 A, 1 B, 2 C, 3 D);
impl_tuple!(0 A, 1 B, 2 C);
impl_tuple!(0 A, 1 B);
