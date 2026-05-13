//! A semi-safe wrapper for LZ4 C library using `lz4-sys` bindings crate
//!
//! ## Streamed operations
//!
//! Currently, there is no safe implementation of streamed compression/decompression
//! because LZ4 `_continue` functions require previous data (source in case of compress
//! pass, destination in case of decompress pass) to be present at the same address of
//! memory until 64 KiB window has been streamed or stream freed, explicit lifetime
//! elision of which wasn't succeeded

use core::mem::MaybeUninit;
use core::ptr::NonNull;
use lz4_sys::{
    c_int, LZ4_compress_continue, LZ4_createStream, LZ4_createStreamDecode,
    LZ4_decompress_safe_continue, LZ4_freeStream, LZ4_freeStreamDecode,
};

/// Wrapper around `LZ4_stream_t` which also serves as an interface for
/// streamed compress function
pub struct CompressStream {
    #[cfg(not(miri))]
    stream: NonNull<lz4_sys::LZ4StreamEncode>,
}

impl CompressStream {
    /// Creates new [`CompressStream`] via `LZ4_createStream` initializer call.
    ///
    /// This method will panic if initializer returned `NULL` pointer
    #[inline]
    pub fn new() -> Self {
        #[cfg(not(miri))]
        {
            let raw_ptr = unsafe {
                // Safety: FFI
                LZ4_createStream()
            };
            let stream = NonNull::new(raw_ptr).expect("LZ4_createStream returned NULL pointer");

            Self { stream }
        }
        #[cfg(miri)]
        {
            Self {}
        }
    }

    /// Compresses the source to the potentially uninitialized destination buffer via
    /// `LZ4_compress_continue`, keeping the context of this encoding stream,
    /// which can improve compression ratios for smaller passes.
    ///
    /// Returned `Some` is an amount of bytes written into `dst`, `None` returned in case
    /// of unsuccessful compression or insufficient destination buffer length.
    ///
    /// # Safety
    ///
    /// - the previous 64 KiB of streamed `src` data *must* remain present, unmodified,
    ///   at same address in memory.
    #[inline]
    pub unsafe fn compress(&self, src: &[u8], dst: &mut [MaybeUninit<u8>]) -> Option<usize> {
        #[cfg(not(miri))]
        {
            let result = unsafe {
                // Safety:
                // - `MaybeUninit<T>` has the same layout as T, so cast is allowed, and
                //   the pointer provided to C FFI is used for reads exclusively.
                // - caller guarantees `src` to live long enough.
                LZ4_compress_continue(
                    self.stream.as_ptr(),
                    src.as_ptr().cast(),
                    dst.as_mut_ptr().cast(),
                    src.len() as c_int,
                )
            };
            (result > 0).then_some(result as usize)
        }
        #[cfg(miri)]
        {
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast(), src.len());
            }
            Some(src.len())
        }
    }
}

#[cfg(not(miri))]
impl Drop for CompressStream {
    fn drop(&mut self) {
        unsafe {
            // Safety: FFI
            LZ4_freeStream(self.stream.as_ptr());
        }
    }
}

/// `LZ4_compressBound` translation
pub const fn compress_bound(size: usize) -> usize {
    if size > 0x7E000000 {
        0
    } else {
        size + (size / 255) + 16
    }
}

/// Wrapper around `LZ4_streamDecode_t` which also serves as an interface for
/// streamed decompress function
pub struct DecompressStream {
    #[cfg(not(miri))]
    stream: NonNull<lz4_sys::LZ4StreamDecode>,
}

impl DecompressStream {
    /// Creates new [`DecompressStream`] via `LZ4_createStreamDecode` initializer call.
    ///
    /// This method will panic if initializer returned `NULL` pointer
    #[inline]
    pub fn new() -> Self {
        #[cfg(not(miri))]
        {
            let raw_ptr = unsafe {
                // Safety: FFI
                LZ4_createStreamDecode()
            };
            let stream = NonNull::new(raw_ptr).expect("LZ4_createStreamDecode returned NULL pointer");

            Self { stream }
        }
        #[cfg(miri)]
        {
            Self {}
        }
    }

    /// Decompresses the source into the potentially uninitialized destination buffer using
    /// `LZ4_decompress_safe_continue`, keeping the context of this decoding stream,
    /// which can improve compression ratios for smaller passes.
    ///
    /// Returned `Some` is an amount of bytes written into `dst`, `None` returned in case
    /// of unsuccessful compression.
    ///
    /// ## Safety
    ///
    /// - the last 64 KiB of previously decoded data *must* remain available and unmodified
    ///   at the memory position where they were previously decoded
    #[inline]
    pub unsafe fn decompress(&self, src: &[u8], dst: &mut [MaybeUninit<u8>]) -> Option<usize> {
        #[cfg(not(miri))]
        {
            let decompressed = unsafe {
                // Safety:
                // - `MaybeUninit<T>` has the same layout as T, so cast is allowed, and
                //   a pointer passed to C FFI is used for reads exclusively.
                // - caller guarantees `src` to live long enough.
                // - `max_output_size` equals to `dst` length.
                LZ4_decompress_safe_continue(
                    self.stream.as_ptr(),
                    src.as_ptr().cast(),
                    dst.as_mut_ptr().cast(),
                    src.len() as c_int,
                    dst.len() as c_int,
                )
            };
            (decompressed > 0).then_some(decompressed as usize)
        }
        #[cfg(miri)]
        {
            unsafe {
                copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast(), src.len());
            }
            Some(src.len())
        }
    }
}

#[cfg(not(miri))]
impl Drop for DecompressStream {
    fn drop(&mut self) {
        unsafe {
            // Safety: FFI
            LZ4_freeStreamDecode(self.stream.as_ptr());
        }
    }
}
