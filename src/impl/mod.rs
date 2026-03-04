use crate::decode::{get, Decode, DecodeBearer, DecodeError};
use crate::encode::{Encode, EncodeBearer, EncodeError};

use core::marker::PhantomData;
use core::mem::MaybeUninit;

mod alloc;
mod seq;
mod tuple;

impl<T> Encode for &T
where
    T: Encode,
{
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        T::encode(bearer, src)
    }

    fn size_of(&self) -> usize {
        (*self).size_of()
    }
}

macro_rules! impl_int {
    ($type:ty) => {
        impl Encode for $type {
            #[inline]
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                bearer.write(&src.to_le_bytes()).map_err(|e| e.into())
            }

            #[inline]
            fn size_of(&self) -> usize {
                size_of::<$type>()
            }
        }

        unsafe impl Decode for $type {
            fn decode(
                bearer: &mut impl DecodeBearer,
                dst: &mut MaybeUninit<Self>,
            ) -> Result<(), DecodeError> {
                let bytes = unsafe {
                    bearer
                        .read(size_of::<$type>())?
                        .try_into()
                        // Safety: `DecodeBearer::read()` guarantees to return slice with
                        // exact length as requested
                        .unwrap_unchecked()
                };
                dst.write(<$type>::from_le_bytes(bytes));
                Ok(())
            }
        }
    };
    ($type:ty as $as:ty) => {
        impl Encode for $type {
            #[inline]
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                bearer
                    .write(&(*src as $as).to_le_bytes())
                    .map_err(|e| e.into())
            }

            #[inline]
            fn size_of(&self) -> usize {
                size_of::<$as>()
            }
        }

        unsafe impl Decode for $type {
            fn decode(
                bearer: &mut impl DecodeBearer,
                dst: &mut MaybeUninit<Self>,
            ) -> Result<(), DecodeError> {
                let bytes = unsafe {
                    bearer
                        .read(size_of::<$as>())?
                        .try_into()
                        // Safety: `DecodeBearer::read()` guarantees to return slice with
                        // exact length as requested
                        .unwrap_unchecked()
                };
                dst.write(<$as>::from_le_bytes(bytes) as $type);
                Ok(())
            }
        }
    };
}

impl_int!(u8);
impl_int!(u16);
impl_int!(u32);
impl_int!(u64);
impl_int!(u128);
impl_int!(i8);
impl_int!(i16);
impl_int!(i32);
impl_int!(i64);
impl_int!(i128);
impl_int!(f32);
impl_int!(f64);
impl_int!(usize as u64);
impl_int!(isize as i64);

impl Encode for bool {
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        u8::encode(bearer, &(*src as u8))
    }

    #[inline]
    fn size_of(&self) -> usize {
        size_of::<u8>()
    }
}

unsafe impl Decode for bool {
    #[inline]
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let bytes = bearer.read(size_of::<u8>())?;
        match unsafe {
            // Safety: `StreamDecoder::read` guarantees to return
            // slice with exact length requested
            bytes.get_unchecked(0)
        } {
            0 => dst.write(false),
            1 => dst.write(true),
            _ => return Err(DecodeError::InvalidPattern),
        };

        Ok(())
    }
}

impl Encode for char {
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        let mut bytes = [0u8; 4];
        let str = src.encode_utf8(&mut bytes);

        bearer.write(str.as_bytes()).map_err(|e| e.into())
    }

    #[inline]
    fn size_of(&self) -> usize {
        self.len_utf8()
    }
}

/// Decode bytes into a UTF-8 character.
///
/// # Safety
///
/// - `leading` must be valid first UTF-8 sequence byte for given length
///   of continuous bytes.
///
/// Violating this requirement will not trigger language UB, instead,
/// core library level UB is produced
unsafe fn decode_char_from_bytes(leading: u8, remaining: &[u8]) -> Option<char> {
    let get = |i: usize| {
        if remaining[i] & 0xC0 == 0x80 {
            return Some((remaining[i] & 0x3F) as u32);
        }
        None
    };

    let codepoint = match remaining.len() {
        1 => (leading as u32 & 0x1F) << 6 | get(0)?,
        2 => (leading as u32 & 0x0F) << 12 | get(0)? << 6 | get(1)?,
        3 => (leading as u32 & 0x07) << 18 | get(0)? << 12 | get(1)? << 6 | get(2)?,
        _ => unreachable!(),
    };

    Some(unsafe {
        // Safety: UTF-8 validated in the `get` closure, `leading` checked to
        // guaranteed by the caller to be valid
        char::from_u32_unchecked(codepoint)
    })
}

unsafe impl Decode for char {
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let leading = unsafe {
            // Safety: `DecodeBearer::read()` guarantees to return slice with
            // exact length as requested
            *bearer.read(1)?.get_unchecked(0)
        };

        let len = match leading {
            0x0..=0x7F => {
                let char = unsafe {
                    // Safety: match expression checked byte to be within ASCII range
                    char::from_u32_unchecked(leading as u32)
                };
                dst.write(char);
                return Ok(());
            }
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            _ => return Err(DecodeError::InvalidPattern),
        };

        let char = unsafe {
            // Safety: `leading` has valid bit pattern for length of remaining bytes
            decode_char_from_bytes(leading, bearer.read(len)?).ok_or(DecodeError::InvalidPattern)?
        };
        dst.write(char);
        Ok(())
    }
}

impl<T> Encode for Option<T>
where
    T: Encode,
{
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        u8::encode(bearer, &(src.is_some() as u8))?;
        match src {
            Some(v) => T::encode(bearer, v),
            None => Ok(()),
        }
    }

    #[inline]
    fn size_of(&self) -> usize {
        1 + self.as_ref().map_or(0, |v| v.size_of())
    }
}

unsafe impl<T> Decode for Option<T>
where
    T: Decode,
{
    #[inline]
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        match unsafe {
            // Safety: `DecodeBearer::read()` guarantees to return slice with
            // exact length as requested
            bearer.read(1)?.get_unchecked(0)
        } {
            0 => dst.write(None),
            1 => dst.write(Some(get(bearer)?)),
            _ => return Err(DecodeError::InvalidPattern),
        };
        Ok(())
    }
}

impl<T, E> Encode for Result<T, E>
where
    T: Encode,
    E: Encode,
{
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        u8::encode(bearer, &(src.is_ok() as u8))?;
        match src {
            Ok(v) => T::encode(bearer, v),
            Err(e) => E::encode(bearer, e),
        }
    }

    #[inline]
    fn size_of(&self) -> usize {
        match self {
            Ok(v) => v.size_of(),
            Err(e) => e.size_of(),
        }
    }
}

unsafe impl<T, E> Decode for Result<T, E>
where
    T: Decode,
    E: Decode,
{
    #[inline]
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        match unsafe {
            // Safety: `DecodeBearer::read()` guarantees to return slice with
            // exact length as requested
            bearer.read(1)?.get_unchecked(0)
        } {
            0 => dst.write(Err(get(bearer)?)),
            1 => dst.write(Ok(get(bearer)?)),
            _ => return Err(DecodeError::InvalidPattern),
        };
        Ok(())
    }
}

impl<T> Encode for PhantomData<T> {
    #[inline]
    fn encode(_: &mut impl EncodeBearer, _: &Self) -> Result<(), EncodeError> {
        Ok(())
    }

    #[inline]
    fn size_of(&self) -> usize {
        0
    }
}

unsafe impl<T> Decode for PhantomData<T> {
    #[inline]
    fn decode(_: &mut impl DecodeBearer, _: &mut MaybeUninit<Self>) -> Result<(), DecodeError> {
        Ok(())
    }
}

impl Encode for () {
    #[inline]
    fn encode(_: &mut impl EncodeBearer, _: &Self) -> Result<(), EncodeError> {
        Ok(())
    }

    #[inline]
    fn size_of(&self) -> usize {
        0
    }
}

unsafe impl Decode for () {
    #[inline]
    fn decode(_: &mut impl DecodeBearer, _: &mut MaybeUninit<Self>) -> Result<(), DecodeError> {
        Ok(())
    }
}

impl<T> Encode for [T]
where
    T: Encode,
{
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        u32::encode(bearer, &(src.len() as u32))?;
        for item in src {
            T::encode(bearer, item)?;
        }
        Ok(())
    }

    #[inline]
    fn size_of(&self) -> usize {
        self.iter().map(|v| v.size_of()).sum::<usize>() + size_of::<u32>()
    }
}

impl<T, const N: usize> Encode for [T; N]
where
    T: Encode,
{
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        for item in src {
            T::encode(bearer, item)?;
        }
        Ok(())
    }

    #[inline]
    fn size_of(&self) -> usize {
        self.iter().map(|v| v.size_of()).sum()
    }
}

/// Panic-safety guard for [`Decode`] implementation of unsized array types.
///
/// # Drop safety
///
/// Owner must call `mem::forget` when underlying array is fully initialized to
/// avoid double dropping
pub(super) struct ArrayDecodingGuard<T> {
    ptr: *mut MaybeUninit<T>,
    len: usize,
}

impl<T> ArrayDecodingGuard<T> {
    /// Create a new [`ArrayDecodingGuard`].
    ///
    /// # Safety
    ///
    /// - `ptr` must be properly aligned and point to valid memory.
    ///
    /// - owner must call `mem::forget` when underlying array is fully initialized to
    ///   avoid double dropping
    pub unsafe fn new(ptr: *mut MaybeUninit<T>) -> Self {
        Self { ptr, len: 0 }
    }

    /// Get a mutable reference to last slot of this array.
    pub fn slot(&mut self) -> &mut MaybeUninit<T> {
        unsafe {
            // Safety:
            // - `len` can be incremented only via `::add()` method, which requires caller
            //   to guarantee increment not to overflow underlying array.
            // - constructor method requires `ptr` assigned to be properly aligned
            &mut *self.ptr.add(self.len)
        }
    }

    /// Increment inner length by one.
    ///
    /// # Safety
    ///
    /// - incrementing the length must not overflow underlying array or `isize::MAX`
    pub unsafe fn add(&mut self) {
        self.len = unsafe {
            // Safety: caller guarantees inner length to not overflow underlying array
            self.len.unchecked_add(1)
        }
    }
}

impl<T> Drop for ArrayDecodingGuard<T> {
    #[cold]
    fn drop(&mut self) {
        unsafe {
            // Safety: owner guarantees to call `mem::forget`
            core::ptr::drop_in_place(core::ptr::slice_from_raw_parts_mut(self.ptr, self.len))
        }
    }
}

unsafe impl<T, const N: usize> Decode for [T; N]
where
    T: Decode,
{
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let mut guard = unsafe {
            // Safety: passed pointer is valid and pointing to `array` initialized above
            ArrayDecodingGuard::new(dst.as_mut_ptr().cast::<MaybeUninit<T>>())
        };

        for _ in 0..N {
            let slot = guard.slot();
            T::decode(bearer, slot)?;
            unsafe {
                // Safety: write will not overflow since we're
                // iterating strictly up to `N`
                guard.add()
            }
        }

        core::mem::forget(guard);
        Ok(())
    }
}

impl Encode for str {
    #[inline]
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        <[u8]>::encode(bearer, src.as_bytes())
    }

    #[inline]
    fn size_of(&self) -> usize {
        self.len()
    }
}
