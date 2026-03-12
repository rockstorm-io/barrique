use crate::frame::FrameError;
use crate::region::{AllocOrd, Push, RegionBuffer, RegionError, Seed, REGION_SIZE};

use alloc::vec::Vec;
use core::mem::MaybeUninit;

/// Runtime error for [`Writer`] trait implementation
///
/// If `std` feature enabled, [`crate::decode::WriteError::Io`] variant included, as
/// analogue to [`std::io::Result`]
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("Requested buffer out of bounds")]
    OutOfBounds,
    #[cfg(feature = "std")]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result of [`Writer`] implementation methods
pub type WriteResult<T> = Result<T, WriteError>;

/// Trait for writing bytes into destination within context of the library.
///
/// # Allocation semantics
///
/// If used only within serializing context, caller will request **up to** 64 KiB
/// per call, but commitment of bytes written can vary depending on data
pub trait Writer {
    /// Allocate exactly `n` possibly uninitialized bytes caller will write to.
    ///
    /// # Advancement semantics
    ///
    /// [`Writer::allocate`] call must not advance the inner cursor, [`Writer::commit`]
    /// is the only method to advance a cursor.
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]>;

    /// Commit `n` written bytes and advance the cursor.
    ///
    /// # Safety
    ///
    /// - advancing inner cursor by `n` must not overflow current capacity
    /// - `..n` bytes allocated from current cursor must be uninitialized.
    unsafe fn commit(&mut self, n: usize);
}

// Writer trait implementations for common types

impl<T> Writer for &mut T
where
    T: Writer,
{
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        (**self).allocate(n)
    }

    unsafe fn commit(&mut self, n: usize) {
        unsafe { (**self).commit(n); }
    }
}

impl Writer for &mut [u8] {
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        let slice = self.get_mut(n..).ok_or(WriteError::OutOfBounds)?;

        Ok(unsafe {
            // Safety: `Self` assumed to be initialized, to cast to `MaybeUninit` is allowed
            core::slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len())
        })
    }

    #[inline]
    unsafe fn commit(&mut self, n: usize) {
        *self = unsafe {
            // Safety: caller guarantees `n` to not overflow
            core::mem::take(self).get_unchecked_mut(n..)
        }
    }
}

impl Writer for &mut [MaybeUninit<u8>] {
    #[inline]
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        self.get_mut(n..).ok_or(WriteError::OutOfBounds)
    }

    #[inline]
    unsafe fn commit(&mut self, n: usize) {
        *self = unsafe {
            // Safety: caller guarantees `n` to not overflow
            core::mem::take(self).get_unchecked_mut(n..)
        }
    }
}

impl Writer for Vec<u8> {
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        self.reserve(n);

        let slice = self.spare_capacity_mut();
        Ok(&mut slice[..n])
    }

    #[inline]
    unsafe fn commit(&mut self, n: usize) {
        unsafe {
            // Safety: caller guarantees bytes to be initialized and the
            // length to not overflow
            self.set_len(self.len() + n);
        }
    }
}

/// A helper function to write arbitrary initialized data into uninitialized
/// slice usually returned by [`Writer::allocate`] method.
///
/// # Panics
///
/// Function will panic if `src.len() > dst.len()`
#[inline]
pub(crate) fn write_to_uninit(src: &[u8], dst: &mut [MaybeUninit<u8>]) {
    if src.len() > dst.len() {
        panic!("Attempt to write with overflow")
    }
    unsafe {
        // Safety:
        // - &[u8] assumed to be initialized, so cast to possibly uninitialized
        //   slice is safe.
        // - mutable reference are exclusive, so ranges can not overlap.
        // - source checked to fit within the destination length.
        core::ptr::copy_nonoverlapping(src.as_ptr().cast(), dst.as_mut_ptr(), src.len());
    }
}

mod private {
    pub trait Sealed {}

    // Similar to `DecodeBearer`, user-defined implementations are forbidden
    impl<W: super::Writer> Sealed for super::StreamEncoder<W> {}
}

/// Bearer of an encoding pipeline.
///
/// In context of this crate, bearers are interfaces whose primary purpose is
/// to translate requested operations on in-memory bytes into a format that
/// can be flushed to or read from a source or destination, in compliance
/// with the specification.
///
/// In simpler terms, bearer will collect bytes requested to write until
/// region buffer is full or explicit flush performed, compress the data and
/// flush resulting region into generic destination. However, such description
/// is only applicable for [`EncodeBearer`] since [`DecodeBearer`] has
/// quite opposite behavior
pub trait EncodeBearer: private::Sealed {
    /// Write `bytes` into the region buffer of this bearer.
    ///
    /// # Request bulks
    ///
    /// It is strongly recommended to not request writes of more than `256` bytes
    /// per write, because the [`StreamEncoder`] implementation does not pack the
    /// data. For example, a request of 32 KiB data to write in case of half-full
    /// buffer will trigger a state switch, degrading performance and leaving
    /// a large span of unused capacity.
    ///
    /// Packing is not implemented as it would add additional runtime overhead
    /// and implementation complexity, especially for [`Decode`] implementations
    ///
    /// [`Decode`]: crate::decode::Decode
    fn write(&mut self, bytes: &[u8]) -> Result<(), RegionError>;
}

/// The one and only [`EncodeBearer`] implementation operating on
/// region streams.
///
/// Flushes are performed automatically when the region buffer is full,
/// but the final flush must be invoked by a caller explicitly.
///
/// # Example
///
/// Encoding a [`String`]:
///
/// ```
/// use barrique::encode::{StreamEncoder, Encode};
/// use barrique::region::{max_encoded_size, AllocOrd};
///
/// let value = String::from("That's a barrique");
/// let mut dst = Vec::with_capacity(max_encoded_size(value.len()));
///
/// let mut bearer = StreamEncoder::new(&mut dst, 0.into(), AllocOrd::Auto(&value));
/// <String as Encode>::encode(&mut bearer, &value).unwrap();
///
/// // Final flush performed by the caller
/// bearer.flush().unwrap();
/// ```
///
/// # Panic
///
/// The [`EncodeBearer::write`] implementation will panic if inner
/// destination [`Writer`] implementation of this encoder returned
/// incorrect result
///
/// # The trait
///
/// The [`EncodeBearer`] trait is essentially a generic wrapper for this
/// struct, providing a cleaner interface for implementing type without
/// trait bounds inclusion and allowing to extend features in
/// the future without breaking API changes
pub struct StreamEncoder<W>
where
    W: Writer,
{
    region_buffer: RegionBuffer,
    authority: Push<W>,
}

impl<W> StreamEncoder<W>
where
    W: Writer,
{
    /// Constructs a new [`StreamEncoder`] with provided destination and [`Seed`].
    ///
    /// # Example
    ///
    /// ```
    /// use barrique::encode::StreamEncoder;
    /// use barrique::region::Seed;
    ///
    /// let mut dst = vec![];
    /// let bearer = StreamEncoder::new(&mut dst, Seed::empty(), Default::default());
    ///
    /// // Now we can call an `Encode` implementation on this bearer ...
    /// ```
    ///
    /// Note on the example: hash will be always zero because [`Seed::empty`] is
    /// an explicit market that the hash computation must be disabled.
    ///
    /// # Allocation semantics
    ///
    /// Each [`StreamEncoder`] constructor call allocates a region buffer with
    /// initial capacity equal to result of [`AllocOrd`] provided. Such semantics
    /// may add (miserable) runtime overhead, but results in a considerable
    /// memory usage decrease for smaller streams.
    ///
    /// If you have access to previously created decoder and your goal is only to
    /// switch the source, consider using [`StreamEncoder::relocate`] method,
    /// which does not reallocate internal buffer
    pub fn new<E>(dst: W, seed: Seed, ord: AllocOrd<E>) -> Self
    where
        E: Encode
    {
        Self {
            region_buffer: RegionBuffer::new(ord.cap()),
            authority: Push::new(dst, seed),
        }
    }

    /// Flushes the region buffer and replaces the destination of this encoder
    /// with the new one, returning the previous destination
    ///
    /// Unlike [`StreamEncoder::relocate_with_seed`], region hash remains
    /// unchanged, making this method useful in cases of operating on
    /// the same format.
    ///
    /// # Example
    ///
    /// ```rust, no_run
    /// use barrique::encode::StreamEncoder;
    /// use barrique::region::Seed;
    ///
    /// let mut dst = vec![];
    /// let mut bearer = StreamEncoder::new(&mut dst, 0.into(), Default::default());
    ///
    /// // Encoding a first value ...
    ///
    /// let mut new_dst = vec![];
    /// let old_dst = bearer.relocate(&mut new_dst).expect("Failed to relocate the bearer");
    /// std::fs::write("serialized_1.bin", &old_dst).unwrap();
    /// 
    /// // Encoding a second value ...
    /// 
    /// std::fs::write("serialized_2.bin", new_dst).unwrap();
    /// ```
    pub fn relocate(&mut self, src: W) -> Result<W, RegionError> {
        self.relocate_with_seed(src, self.authority.seed())
    }

    /// Flushes current content of the region buffer and replaces the destination
    /// and seed of this encoder with the new values.
    ///
    /// This method differs from the main constructor in a way that it doesn't
    /// reconstruct the inner region buffer, avoiding reallocation.
    pub fn relocate_with_seed(&mut self, src: W, seed: Seed) -> Result<W, RegionError> {
        self.region_buffer.pass(&mut self.authority)?;
        let old = core::mem::replace(&mut self.authority, Push::new(src, seed));
        
        Ok(old.into_destination())
    }

    /// Flushes the current data in the region buffer
    pub fn flush(&mut self) -> Result<(), RegionError> {
        self.region_buffer.pass(&mut self.authority)?;
        Ok(())
    }
}

impl<W> EncodeBearer for StreamEncoder<W>
where
    W: Writer,
{
    fn write(&mut self, bytes: &[u8]) -> Result<(), RegionError> {
        if bytes.len() > REGION_SIZE {
            // Serving request greater than `REGION_SIZE` is not possible
            return Err(RegionError::OutOfBounds);
        }

        if self.region_buffer.remaining_cap() < bytes.len() {
            self.region_buffer.pass(&mut self.authority)?;
            self.region_buffer.swap();
        }

        unsafe {
            // Safety: `bytes` can not overlap with internal buffer allocation
            // since no interface provided to access it
            self.region_buffer.write_nonoverlapping(bytes);
        }

        Ok(())
    }
}

/// The [`Encode`] implementation runtime error
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("Frame encoding error: \"{0}\"")]
    FrameError(#[from] FrameError),
    #[error("Encode bearer error: \"{0}\"")]
    BearerError(#[from] RegionError),
    #[error("{0}")]
    Other(&'static str),
}

/// A trait defining serialization of implementing type into a byte representation
/// for continuous storage.
pub trait Encode {
    /// Serialize `Self`, by requesting `bearer` to write encoded bytes.
    ///
    /// An implementation serializes `Self` by interpreting it into a byte format and requesting
    /// [`EncodeBearer`] to write format slice via [`EncodeBearer::write`] method
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError>;

    /// Get a possible or actual size of serialized format of `Self` in bytes.
    fn size_of(&self) -> usize;
}
