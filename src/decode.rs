use crate::region::{AllocOrd, Pull, RegionBuffer, RegionError, Seed};
use crate::frame::FrameError;

use core::mem::MaybeUninit;

/// Decode `T` from the [`DecodeBearer`] provided.
///
/// [`Decode`] trait implied in-place initialization, which requires to pass
/// destination slot implementation will write to. Such approach avoids
/// copying in some cases, but may introduce boilerplate for slot
/// declaration. This function does this exact setup and
/// calls `::decode` on the `T` provided
#[inline]
pub fn get<T>(bearer: &mut impl DecodeBearer) -> Result<T, DecodeError>
where
    T: Decode,
{
    let mut slot = MaybeUninit::<T>::uninit();
    T::decode(bearer, &mut slot)?;

    Ok(unsafe {
        // Safety: `Decode::decode` guarantees to initialize
        // the given destination
        slot.assume_init()
    })
}

/// Runtime error for [`Reader`] trait implementation
///
/// If `std` feature enabled, [`ReadError::Io`] variant included, as
/// analogue to [`std::io::Result`]
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("Requested length out of bounds")]
    OutOfBounds,
    #[cfg(feature = "std")]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result of read methods of the [`Reader`] trait
pub type ReadResult<T> = Result<T, ReadError>;

/// Trait for reading bytes from a source within context of the crate.
///
/// # Advancement semantics
///
/// - `read_borrow` and `read_borrow_const` never advance a reader.
/// - `advance` is the only method to advance a reader
pub trait Reader {
    /// Read exactly `n` bytes
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]>;

    /// Advance the reader by `n` bytes
    fn advance(&mut self, n: usize);

    /// Read exactly constant `N` bytes.
    fn read_borrow_const<const N: usize>(&self) -> ReadResult<[u8; N]> {
        let mut buf = [0u8; N];
        buf.copy_from_slice(self.read_borrow(N)?);

        Ok(buf)
    }
}

// Reader trait implementations for common types

impl Reader for &[u8] {
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]> {
        self.get(..n).ok_or(ReadError::OutOfBounds)
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        *self = self
            .get(n..)
            .expect("Attempt to advance a Reader with overflow")
    }
}

impl Reader for &mut [u8] {
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]> {
        self.get(..n).ok_or(ReadError::OutOfBounds)
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        *self = core::mem::take(self)
            .get_mut(n..)
            .expect("Attempt to advance a Reader with overflow")
    }
}

mod private {
    pub trait Sealed {}

    // Bearers are the main and the only way to work with data in
    // specification format, so allowing user-defined traits is
    // obviously unsuitable
    impl<R: super::Reader> Sealed for super::StreamDecoder<R> {}
}

/// Bearer of a decoding pipeline.
///
/// In context of this crate, bearers are interfaces whose primary purpose is
/// to translate requested operations on in-memory bytes into a format that
/// can be flushed to or read from a source or destination, in compliance
/// with the specification.
///
/// In simpler terms, bearer will select targeted chunk, verify the metadata,
/// decompress and give raw bytes to the caller, which is responsible for
/// interpreting them in [`Decode`] implementation. However, such description
/// is only applicable for [`DecodeBearer`] since [`EncodeBearer`] has
/// quite the opposite behavior
pub trait DecodeBearer: private::Sealed {
    /// Request exactly `n` raw bytes.
    ///
    /// # Implementation considerations
    ///
    /// The only implementation caller can receive is [`StreamDecoder`],
    /// which guarantees slice in `Ok` result to have `n` length
    /// exactly.
    ///
    /// Read of `n` bytes advances the bearer's internal position in source
    /// by `n`, making prior bytes inaccessible.
    fn read(&mut self, n: usize) -> Result<&[u8], RegionError>;
}

/// The one and only [`DecodeBearer`] implementation operating on
/// region streams.
///
/// # Example
///
/// ```
/// use std::mem::MaybeUninit;
///
/// let src = std::fs::read("stream.bin").unwrap();
/// let mut bearer = StreamDecoder::new(&src, 0.into(), Default::default());
///
/// let mut string = MaybeUninit::uninit();
/// <String as Decode>::decode(&mut bearer, &mut string).unwrap();
///
/// // Safety: `Decode` is an unsafe trait and requires implementation to
/// // initialize the value properly
/// let string = unsafe { string.assume_init() };
/// ```
///
/// # Panic
///
/// The [`DecodeBearer::read`] implementation will panic if inner
/// destination [`Reader`] implementation of this decoder returned
/// incorrect result
///
/// # The trait
///
/// The [`DecodeBearer`] trait is essentially a generic wrapper for this
/// struct, providing a cleaner interface for implementing type without
/// trait bounds inclusion and allowing to extend features in
/// the future without breaking API changes
pub struct StreamDecoder<R>
where
    R: Reader,
{
    region_buffer: RegionBuffer,
    authority: Pull<R>,
}

// Public methods
impl<R> StreamDecoder<R>
where
    R: Reader,
{
    /// Construct a new [`StreamDecoder`] with provided source and [`Seed`].
    ///
    /// # Example
    ///
    /// ```
    /// use barrique::decode::{AllocOrd, StreamDecoder};
    ///
    /// let src = vec![];
    /// let mut bearer = StreamDecoder::new(&src, 0.into(), Default::default());
    ///
    /// // In this example read of a first region will always fail
    /// // since we've passed an empty slice
    /// assert_eq!(bearer, Err(_));
    /// ```
    ///
    /// # Allocation semantics
    ///
    /// Each [`StreamDecoder`] constructor call allocates a region buffer with
    /// initial capacity equal to result of [`AllocOrd`] provided. Such semantics
    /// may add (miserable) runtime overhead, but results in a considerable
    /// memory usage decrease for smaller streams.
    ///
    /// If you have access to previously created decoder and your goal is only to
    /// switch the source, consider using [`StreamDecoder::relocate`] method,
    /// which does not reallocate internal buffer
    pub fn new(src: R, seed: Seed, ord: AllocOrd) -> Result<Self, RegionError> {
        let mut bearer = Self {
            region_buffer: RegionBuffer::new(ord.cap()),
            authority: Pull::new(src, seed),
        };
        bearer.region_buffer.pass(&mut bearer.authority)?;

        Ok(bearer)
    }

    /// Replace the source of this decoder with a new one.
    ///
    /// Unlike [`StreamDecoder::relocate_with_seed`], region hash remains
    /// unchanged, making this method useful in cases of operating on
    /// the same format.
    ///
    /// # Example
    ///
    /// ```
    /// use barrique::decode::{AllocOrd, StreamDecoder};
    ///
    /// let src = std::fs::read("serialized_1.bin").unwrap();
    /// let mut bearer = StreamDecoder::new(&src, 0.into(), Default::default())
    ///     .expect("Failed to initialize a bearer");
    ///
    /// // Doing some work on `src` contents ...
    ///
    /// let mut src = std::fs::read("serialized_2.bin").unwrap();
    /// bearer.relocate(&mut src)
    ///     .expect("Failed to relocate the bearer");
    ///
    /// // Now we can process contents of the new `src` ...
    /// ```
    pub fn relocate(&mut self, src: R) -> Result<(), RegionError> {
        self.relocate_with_seed(src, self.authority.seed())
    }

    /// Replace the source and the seed of this decoder with new values.
    ///
    /// This method differs from the main constructor in a way that it doesn't
    /// reconstruct the inner region buffer, avoiding reallocation.
    pub fn relocate_with_seed(&mut self, src: R, seed: Seed) -> Result<(), RegionError> {
        self.authority = Pull::new(src, seed);
        self.region_buffer.pass(&mut self.authority)
    }
}

impl<R> DecodeBearer for StreamDecoder<R>
where
    R: Reader,
{
    #[inline]
    fn read(&mut self, n: usize) -> Result<&[u8], RegionError> {
        if self.region_buffer.remaining_len() < n {
            self.region_buffer.swap();
            self.region_buffer.pass(&mut self.authority)?;
        }

        // If read failed after a state switch, we assume request
        // as invalid for the bytes streamed
        self.region_buffer.read(n).ok_or(RegionError::OutOfBounds)
    }
}

/// The [`Decode`] implementation runtime error
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Frame decoding error: \"{0}\"")]
    FrameError(#[from] FrameError),
    #[error("Decode bearer error: \"{0}\"")]
    BearerError(#[from] RegionError),
    #[error("Invalid pattern")]
    InvalidPattern,
    #[error("{0}")]
    Other(&'static str),
}

/// A trait for interpreting serialized bytes into instances of the
/// implementing type.
///
/// # Safety
///
/// Implementation of a `decode` method must properly initialize given `dst`
pub unsafe trait Decode
where
    Self: Sized,
{
    /// Construct `Self` from raw serialized bytes.
    ///
    /// An implementation request bytes to interpret via [`DecodeBearer::read`]
    /// method, which in case of correct schema will return the same byte pattern
    /// as [`Encode`] implementation of this type or any other type with similar
    /// serialized structure will produce.
    ///
    /// # Implementation requirement
    ///
    /// This trait marked as unsafe because of one requirement implementation must
    /// guarantee: `dst` must be properly initialized in case of `Ok`
    fn decode(
        bearer: &mut impl DecodeBearer,
        dst: &mut MaybeUninit<Self>,
    ) -> Result<(), DecodeError>;
}
