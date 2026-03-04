use crate::decode::{get, Decode, DecodeBearer, DecodeError};
use crate::encode::{Encode, EncodeBearer, EncodeError};
use crate::r#impl::ArrayDecodingGuard;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::{ManuallyDrop, MaybeUninit};

macro_rules! impl_heap_ptr {
    ($type:ident) => {
        impl<T> Encode for $type<T>
        where
            T: Encode + ?Sized,
        {
            fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
                T::encode(bearer, &*src)
            }

            fn size_of(&self) -> usize {
                (**self).size_of()
            }
        }

        const _: () = {
            /// Panic-safety guard for decoding heap pointer for [`Decode`]
            /// implementation of heap pointer type
            ///
            /// # Drop safety
            ///
            /// Owner must call `mem::forget` when underlying value is initialized to
            /// avoid double dropping
            struct HeapPtrDecodingGuard<T> {
                ptr: *mut MaybeUninit<T>,
            }

            impl<T> HeapPtrDecodingGuard<T> {
                /// Create a new [`HeapPtrDecodingGuard`].
                ///
                /// # Safety
                ///
                /// - owner must call `mem::forget` when underlying value is initialized to
                ///   avoid double dropping
                unsafe fn new(ptr: *mut MaybeUninit<T>) -> Self {
                    Self { ptr }
                }
            }

            impl<T> Drop for HeapPtrDecodingGuard<T> {
                #[cold]
                fn drop(&mut self) {
                    drop(unsafe { $type::from_raw(self.ptr) });
                }
            }

            unsafe impl<T> Decode for $type<T>
            where
                T: Decode,
            {
                fn decode(
                    bearer: &mut impl DecodeBearer,
                    dst: &mut MaybeUninit<Self>,
                ) -> Result<(), DecodeError> {
                    let value = $type::<T>::new_uninit();
                    let ptr = $type::into_raw(value) as *mut _;
                    let guard = unsafe {
                        // Safety: guard forgot after `T::decode` call finished
                        HeapPtrDecodingGuard::new(ptr)
                    };

                    T::decode(bearer, unsafe {
                        // Safety: dereferenced pointer is derived from valid
                        // instance of $type
                        &mut *ptr
                    })?;
                    core::mem::drop(guard);

                    dst.write(unsafe {
                        // Safety:
                        // - `ptr` points to valid instance of $type constructed above.
                        // - `Decode::decode` guarantees to initialize the slot provided
                        $type::from_raw(ptr).assume_init()
                    });
                    Ok(())
                }
            }
        };
    };
}

impl_heap_ptr!(Box);
impl_heap_ptr!(Arc);
impl_heap_ptr!(Rc);

macro_rules! impl_heap_array_decode {
    ($type:ident) => {
        const _: () = {
            /// Panic-safety guard for decoding heap-allocated array for [`Decode`]
            /// implementation of heap pointer type
            ///
            /// # Drop safety
            ///
            /// Owner must call `mem::forget` when underlying array is fully initialized to
            /// avoid double dropping
            struct HeapPointerDecodingGuard<T> {
                array_guard: ManuallyDrop<ArrayDecodingGuard<T>>,
                alloc_ptr: *mut [MaybeUninit<T>],
            }

            impl<T> HeapPointerDecodingGuard<T> {
                /// Create a new [`HeapPointerDecodingGuard`].
                ///
                /// # Safety
                ///
                /// - `ptr` must point to valid `$type` holding `[MaybeUninit<T>]`
                ///
                /// - owner must call `mem::forget` when underlying array is fully initialized to
                ///   avoid double dropping
                unsafe fn new(ptr: *mut [MaybeUninit<T>]) -> Self {
                    Self {
                        array_guard: ManuallyDrop::new(unsafe {
                            // Safety: caller guarantees to call `mem::forget` when array is
                            // fully initialized
                            ArrayDecodingGuard::new(
                                // Safety: constructor requires assigned `ptr` to point
                                // to valid `$type`
                                (*ptr).as_mut_ptr(),
                            )
                        }),
                        alloc_ptr: ptr,
                    }
                }

                /// Get a mutable reference to last slot of inner array of this pointer.
                fn slot(&mut self) -> &mut MaybeUninit<T> {
                    self.array_guard.slot()
                }

                /// Increment inner length by one.
                ///
                /// # Safety
                ///
                /// - incrementing the length must not overflow underlying array or isize::MAX
                unsafe fn add(&mut self) {
                    unsafe {
                        // Safety: caller guarantees to comply with `ArrayDecodingGuard::add()`
                        // requirements
                        self.array_guard.add()
                    }
                }
            }

            impl<T> Drop for HeapPointerDecodingGuard<T> {
                #[cold]
                fn drop(&mut self) {
                    unsafe {
                        ManuallyDrop::drop(&mut self.array_guard);
                        drop($type::from_raw(self.alloc_ptr))
                    }
                }
            }

            unsafe impl<T> Decode for $type<[T]>
            where
                T: Decode,
            {
                fn decode(
                    bearer: &mut impl DecodeBearer,
                    dst: &mut MaybeUninit<Self>,
                ) -> Result<(), DecodeError> {
                    let len = get::<u32>(bearer)? as usize;
                    // This check is necessary since `write` method of `guard` requires
                    // length cursor to not exceed `isize::MAX` for pointer addition
                    if len > isize::MAX as usize {
                        return Err(DecodeError::Other("Length exceeds isize::MAX"));
                    }

                    let slice = $type::<[T]>::new_uninit_slice(len);
                    let slice_ptr = $type::into_raw(slice) as *mut [MaybeUninit<T>];
                    let mut guard = unsafe {
                        // Safety: `buf_ptr` is valid for `$type::from_raw`
                        HeapPointerDecodingGuard::new(slice_ptr)
                    };

                    for _ in 0..len {
                        let slot = guard.slot();
                        T::decode(bearer, slot)?;
                        unsafe {
                            // Safety: we're iterating up to `len`, which is length
                            // of the underlying `$type` of `guard`
                            guard.add();
                        }
                    }

                    core::mem::forget(guard);
                    unsafe {
                        // Safety: `Decode::decode` guarantees to initialize the slot provided
                        dst.write($type::from_raw(slice_ptr).assume_init());
                    }
                    Ok(())
                }
            }
        };
    };
}

impl_heap_array_decode!(Box);
impl_heap_array_decode!(Arc);
impl_heap_array_decode!(Rc);

impl<T> Encode for Vec<T>
where
    T: Encode,
{
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        <[T]>::encode(bearer, src)
    }

    fn size_of(&self) -> usize {
        self.len() * size_of::<T>()
    }
}

unsafe impl<T> Decode for Vec<T>
where
    T: Decode,
{
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let len = get::<u32>(bearer)? as usize;
        // `Vec` constructor will panic if `len` exceeds isize::MAX
        if len > isize::MAX as usize {
            return Err(DecodeError::Other("Length exceeds isize::MAX"));
        }

        let mut vec = Vec::<T>::with_capacity(len);
        let mut ptr = vec.as_mut_ptr().cast();

        for slot in 0..len {
            T::decode(bearer, unsafe {
                // Safety: `ptr` points to valid span of memory within
                // the bounds of `vec` capacity
                &mut *ptr
            })?;
            unsafe {
                // Safety: `ptr` incremented strictly up to `len`, which
                // equal to capacity of `vec`.
                // Note: unlike heap pointers and arrays, Vec already has a
                // drop-guard behavior we achieve using `set_len`
                ptr = ptr.add(1);
                vec.set_len(slot + 1);
            }
        }

        dst.write(vec);
        Ok(())
    }
}

impl<T> Encode for VecDeque<T>
where
    T: Encode,
{
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        u32::encode(bearer, &(src.len() as u32))?;

        let mut write_slice_plain = |slice: &[T]| -> Result<(), EncodeError> {
            for elem in slice {
                T::encode(bearer, elem)?;
            }
            Ok(())
        };

        let (front, back) = src.as_slices();
        write_slice_plain(front)?;
        write_slice_plain(back)?;

        Ok(())
    }

    fn size_of(&self) -> usize {
        self.len() * size_of::<T>()
    }
}

unsafe impl<T> Decode for VecDeque<T>
where
    T: Decode,
{
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let vec = get::<Vec<T>>(bearer)?;
        dst.write(vec.into());
        Ok(())
    }
}

impl Encode for String {
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError> {
        str::encode(bearer, src.as_str())
    }

    fn size_of(&self) -> usize {
        self.len()
    }
}

unsafe impl Decode for String {
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError> {
        let bytes = get(bearer)?;
        dst.write(String::from_utf8(bytes).map_err(|_| DecodeError::InvalidPattern)?);

        Ok(())
    }
}
