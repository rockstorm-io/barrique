use crate::frame::FrameError;
use crate::region::{Push, RegionBuffer, RegionError, REGION_SIZE};

use core::mem::MaybeUninit;
use core::error::Error;

use alloc::vec::Vec;

/// Runtime IO error for [`Writer::write()`] method.
///
/// If `std` feature enabled, [`crate::decode::WriteError::Io`] variant included, as
/// an alternative to [`std::io::Result`]
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{0}")]
    Custom(Box<dyn Error>),
    #[cfg(feature = "std")]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A trait defining an incremental write interface.
///
/// This trait serves not only as an alternative to [`std::io::Write`] for
/// `no_std`: LZ4 compression used in region encoding requires two separate
/// source and destination buffers, therefore we need an additional
/// allocation besides the region buffer. [`Writer::write_mut()`] allows
/// encoder to write directly into destination and [`Writer::flush`]
/// serves as a controllable flush.
///
/// The writes are incremental because implementations of this trait are also
/// responsible for tracking amount of written bytes. [`Writer::write_mut()`]
/// must not increment the cursor
pub trait Writer {
    /// Get a mutable slice of exactly `n` possibly uninitialized
    /// bytes the caller will write to without advancing the cursor.
    ///
    /// `None` will be returned if `n` is out of bounds
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]>;

    /// Assume `n` bytes starting from the cursor position as initialized (written)
    /// and advance the cursor by `n`.
    ///
    /// # Safety
    ///
    /// - advancing inner cursor by `n` must not overflow current capacity.
    ///
    /// - a range of `n` bytes staring from the cursor position must be initialized
    unsafe fn assume_init(&mut self, n: usize);

    /// Flush this `Writer` into the actual destination.
    ///
    /// Implementations of this trait don't have a condition to guarantee data
    /// reaching its destination unless explicit flush was called
    fn flush(&mut self) -> Result<(), WriteError> {
        Ok(())
    }
}

// Writer trait implementations for common types

impl<T> Writer for &mut T
where
    T: Writer,
{
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
        (**self).write_mut(n)
    }

    unsafe fn assume_init(&mut self, n: usize) {
        unsafe { (**self).assume_init(n) }
    }
}

impl Writer for &mut [u8] {
    #[inline]
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
        let slice = self.get_mut(n..)?;

        Some(unsafe {
            // Safety: `Self` assumed to be initialized, to cast to `MaybeUninit` is allowed
            core::slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len())
        })
    }

    #[inline]
    unsafe fn assume_init(&mut self, n: usize) {
        *self = unsafe {
            // Safety: caller guarantees `n` to not overflow
            core::mem::take(self).get_unchecked_mut(n..)
        }
    }
}

impl Writer for &mut [MaybeUninit<u8>] {
    #[inline]
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
        self.get_mut(n..)
    }

    #[inline]
    unsafe fn assume_init(&mut self, n: usize) {
        *self = unsafe {
            // Safety: caller guarantees `n` to not overflow
            core::mem::take(self).get_unchecked_mut(n..)
        }
    }
}

impl Writer for Vec<u8> {
    #[inline]
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
        self.reserve(n);

        let slice = self.spare_capacity_mut();
        slice.get_mut(..n)
    }

    #[inline]
    unsafe fn assume_init(&mut self, n: usize) {
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
/// 
/// [`DecodeBearer`]: crate::decode::DecodeBearer
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

/// A [`StreamEncoder`] builder.
/// 
/// See example in [`StreamEncoderBuilder::new`]
pub struct StreamEncoderBuilder {
    region_buffer: RegionBuffer,
    seed: Option<u64>,
}

impl StreamEncoderBuilder {
    /// Creates a new [`StreamEncoderBuilder`].
    ///
    /// # Example
    ///
    /// ```
    /// use barrique::encode::{StreamEncoder, StreamEncoderBuilder};
    ///
    /// let mut dst = vec![];
    /// let bearer = StreamEncoderBuilder::new(4 * 1024).build(&mut dst);
    ///
    /// // Now we can call an `Encode` implementation with this bearer ...
    /// ```
    ///
    /// # Allocation semantics
    ///
    /// Each time this constructor is called, it allocates a region buffer with
    /// initial capacity equal to `size`. Such semantics may add (miserable) runtime
    /// overhead, but results in a considerable memory usage decrease for smaller streams.
    ///
    /// If you have access to previously created encoder and your goal is only to
    /// switch the source, consider using [`StreamEncoderBuilder::from_raw`] to
    /// reuse already existing allocation
    pub fn new(size: usize) -> Self {
        Self {
            region_buffer: RegionBuffer::new(size),
            seed: None,
        }
    }

    /// Creates a new [`StreamEncoderBuilder`] without allocating a new region buffer
    /// and instead reusing already existing one.
    ///
    /// See example in [`StreamEncoder::into_raw`] documentation
    pub fn from_raw(buffer: RegionBuffer) -> Self {
        Self {
            region_buffer: buffer,
            seed: None,
        }
    }

    /// Specifies a `seed` which will be used to create region hashes
    pub fn seed(mut self, seed: u64) -> StreamEncoderBuilder {
        self.seed = Some(seed);
        self
    }

    /// Constructs a new [`StreamEncoder`] with specified parameters and `writer`
    /// destination.
    ///
    /// See example in [`StreamEncoderBuilder::new`].
    ///
    /// # Seed
    ///
    /// If a seed wasn't specified using [`StreamEncoderBuilder::seed`], hash computation
    /// will be disabled and hash field in region headers will be zeroed.
    pub fn build<W>(self, writer: W) -> StreamEncoder<W>
    where
        W: Writer,
    {
        StreamEncoder {
            region_buffer: self.region_buffer,
            switch: Push::new(writer, self.seed)
        }
    }
}

/// The one and only [`EncodeBearer`] implementation operating on
/// region streams.
///
/// Flushes are performed automatically when the region buffer is full, but the final
/// flush should be performed by a user explicitly.
///
/// # Example
///
/// Encoding a [`String`] and specifying a seed:
///
/// ```
/// use barrique::encode::{StreamEncoder, Encode, StreamEncoderBuilder};
/// use barrique::region::max_encoded_size;
///
/// let value = String::from("That's a barrique");
/// let size = max_encoded_size(value.len());
///
/// let mut dst = Vec::with_capacity(size);
/// let mut bearer = StreamEncoderBuilder::new(size).seed(0).build(&mut dst);
/// String::encode(&mut bearer, &value).unwrap();
///
/// bearer.flush().unwrap();
/// ```
///
/// # Panic
///
/// The [`EncodeBearer::write`] implementation will panic if inner
/// destination [`Writer`] implementation of this encoder returned
/// incorrect result.
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
    switch: Push<W>,
}

impl<W> StreamEncoder<W>
where
    W: Writer,
{
    /// Deconstructs this [`StreamEncoder`] into a [`RegionBuffer`] for later reuse.
    ///
    /// # Example
    ///
    /// Encoding two files:
    ///
    /// ```rust,no_run
    /// use barrique::encode::StreamEncoderBuilder;
    /// use barrique::encode::Encode;
    /// 
    /// let mut dst = vec![];
    /// let mut bearer = StreamEncoderBuilder::new(0).build(&mut dst);
    ///
    /// // Let's encode a String into the first file
    /// String::encode(&mut bearer, &"She sells seashells by the seashore".into()).unwrap();
    /// let raw = bearer.into_raw();
    ///
    /// std::fs::write("seashell.bin", &dst).unwrap();
    ///
    /// let mut bearer = StreamEncoderBuilder::from_raw(raw).build(&mut dst);
    ///
    /// // ... then an u64 into the second file
    /// u64::encode(&mut bearer, &115104101108108).unwrap();
    /// bearer.flush().unwrap();
    ///
    /// std::fs::write("ascii_seashell.bin", &dst).unwrap();
    /// ```
    pub fn into_raw(self) -> RegionBuffer {
        self.region_buffer
    }

    /// Flushes current state of the region buffer
    pub fn flush(&mut self) -> Result<(), RegionError> {
        self.region_buffer.pass(&mut self.switch)
    }
}

impl<W> StreamEncoder<W>
where
    W: Writer,
{
    /// Creates a new [`StreamEncoder`].
    ///
    /// Not exposed publicly in favor of [`StreamEncoderBuilder::build`]
    pub(crate) fn new(dst: W, size: usize, seed: Option<u64>) -> Self {
        Self {
            region_buffer: RegionBuffer::new(size),
            switch: Push::new(dst, seed),
        }
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
            self.region_buffer.pass(&mut self.switch)?;
            self.region_buffer.swap();
        }
        self.region_buffer.write(bytes);

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
///
/// This trait is recommended to be implemented using `#[derive(Encode)]` procedural macro.
///
/// # Example
///
/// Implementing using `Encode` macro (requires `derive` feature):
///
/// ```rust,no_run
/// use barrique::Encode;
///
/// // Nothing fancy, right?
///
/// #[derive(Encode)]
/// struct Time {
///     old_days: u64,
/// }
/// ```
/// 
/// See documentation on attributes in [`barrique_derive`]
pub trait Encode {
    /// Serialize `Self`, by requesting `bearer` to write encoded bytes.
    ///
    /// An implementation serializes `Self` by interpreting it into a byte format and requesting
    /// [`EncodeBearer`] to write format slice via [`EncodeBearer::write`] method
    fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError>;

    /// Get a possible or an actual size of serialized format of `Self`, in bytes
    fn size_of(&self) -> usize;
}
