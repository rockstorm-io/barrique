use crate::region::{Pull, RegionBuffer, RegionError};
use crate::frame::FrameError;

use core::mem::MaybeUninit;

/// Decode `T` from the [`DecodeBearer`] provided.
///
/// [`Decode`] trait implied in-place initialization, which requires to pass
/// destination slot implementation will write to. Such approach avoids
/// copying in some cases, but may introduce boilerplate for slot
/// declaration. This function does this exact setup and
/// calls `::decode` on the `T` provided.
///
/// # Example
///
/// ```rust, no_run
/// use barrique::decode::{StreamDecoderBuilder, get};
///
/// fn main() {
///     let src = vec![];
///     let mut bearer = StreamDecoderBuilder::new(0).build(src.as_slice())
///         .expect("Err is expected since `src` is empty");
///
///     let _ = get::<String>(&mut bearer);
/// }
/// ```
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

/// A trait defining a read interface.
///
/// This trait fulfills the role of [`io::Read`] for `no_std` environments
/// and provides an interface more suitable for this crate's implementation
/// of the serialization pipeline
pub trait Reader {
    /// Read exactly `n` bytes and do not advance the reader
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]>;

    /// Advance the reader by `n` bytes
    fn advance(&mut self, n: usize);

    /// Read exactly constant `N` bytes and do not advance the reader
    fn read_borrow_const<const N: usize>(&self) -> ReadResult<[u8; N]> {
        let mut buf = [0u8; N];
        buf.copy_from_slice(self.read_borrow(N)?);

        Ok(buf)
    }
}

// Reader trait implementations for common types

impl Reader for &[u8] {
    #[inline]
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
    #[inline]
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
///
/// [`EncodeBearer`]: crate::encode::EncodeBearer
pub trait DecodeBearer: private::Sealed {
    /// Request exactly `n` raw bytes.
    ///
    /// # Implementation considerations
    ///
    /// The only implementation caller can receive right now is [`StreamDecoder`],
    /// which guarantees slice in `Ok` result to have `n` length
    /// exactly.
    ///
    /// Read of `n` bytes advances the bearer's internal position in source
    /// by `n`, making prior bytes inaccessible.
    fn read(&mut self, n: usize) -> Result<&[u8], RegionError>;
}

/// A [`StreamDecoder`] builder.
/// 
/// See example in [`StreamDecoder`] documentation
pub struct StreamDecoderBuilder {
    region_buffer: RegionBuffer,
    seed: Option<u64>,
}

impl StreamDecoderBuilder {
    /// Creates a new [`StreamDecoderBuilder`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use barrique::decode::StreamDecoderBuilder;
    /// use barrique::encode::{StreamEncoder, StreamEncoderBuilder};
    ///
    /// let src = std::fs::read("functional_bros.bin").unwrap();
    /// let bearer = StreamDecoderBuilder::new(0).build(src.as_slice())
    ///     .expect("Failed to read the first region");
    ///
    /// // Now we can call an `Decode` implementation with this bearer ...
    /// ```
    ///
    /// # Allocation semantics
    ///
    /// Each time this constructor is called, it allocates a region buffer with
    /// initial capacity equal to `size`. Such semantics may add (miserable) runtime
    /// overhead, but results in a considerable memory usage decrease for smaller streams.
    ///
    /// If you have access to previously created decoder and your goal is only to
    /// switch the source, consider using [`StreamDecoderBuilder::from_raw`] to
    /// reuse already existing allocation
    #[inline]
    pub fn new(size: usize) -> Self {
        Self {
            region_buffer: RegionBuffer::new(size),
            seed: None,
        }
    }

    /// Creates a new [`StreamDecoderBuilder`] without allocating a new region buffer
    /// and instead reusing already existing one.
    ///
    /// See example in [`StreamDecoder::into_raw`] documentation
    #[inline]
    pub fn from_raw(buffer: RegionBuffer) -> Self {
        Self {
            region_buffer: buffer,
            seed: None,
        }
    }

    /// Specifies a `seed` which will be used to create region hashes
    #[inline]
    pub fn seed(mut self, seed: u64) -> StreamDecoderBuilder {
        self.seed = Some(seed);
        self
    }

    /// Constructs a new [`StreamDecoder`] with specified parameters and `reader`
    /// source.
    ///
    /// See example in [`StreamDecoderBuilder::new`].
    ///
    /// # Seed
    ///
    /// If a seed wasn't specified using [`StreamDecoderBuilder::seed`], hash computation
    /// will be disabled and hash fields will be compared with zeroed hash.
    ///
    /// # Error
    ///
    /// This method will return an error if decoding the first region has failed
    pub fn build<R>(self, reader: R) -> Result<StreamDecoder<R>, RegionError>
    where
        R: Reader,
    {
        let mut decoder = StreamDecoder {
            region_buffer: self.region_buffer,
            switch: Pull::new(reader, self.seed)
        };
        decoder.region_buffer.pass(&mut decoder.switch)?;

        Ok(decoder)
    }
}

/// The one and only [`DecodeBearer`] implementation operating on
/// region streams.
///
/// # Example
///
/// ```rust, no_run
/// use barrique::decode::{StreamDecoder, Decode, get, StreamDecoderBuilder};
/// use core::mem::MaybeUninit;
///
/// let src = std::fs::read("question.bin").unwrap();
/// let mut bearer = StreamDecoderBuilder::new(4 * 1024)
///     .seed(0)
///     .build(src.as_slice())
///     .unwrap();
///
/// assert_eq!(
///     get::<String>(&mut bearer).unwrap(),
///     "Are you gonna walk into a test chamber?".to_string()
/// );
/// ```
///
/// # Panic
///
/// The [`DecodeBearer::read`] implementation will panic if inner
/// destination [`Reader`] implementation of this decoder returned
/// incorrect result.
///
/// # The trait
///
/// The [`DecodeBearer`] trait is essentially a generic wrapper for this
/// struct, providing a simpler interface for implementing type without
/// trait bounds inclusion and allowing to extend features in
/// the future without breaking API changes
pub struct StreamDecoder<R>
where
    R: Reader,
{
    region_buffer: RegionBuffer,
    switch: Pull<R>,
}

// Public methods
impl<R> StreamDecoder<R>
where
    R: Reader,
{
    /// Deconstructs this [`StreamDecoder`] into a [`RegionBuffer`] for later reuse.
    ///
    /// # Example
    ///
    /// Decoding two files:
    ///
    /// ```rust,no_run
    /// use barrique::decode::{StreamDecoderBuilder, get};
    /// 
    /// let mut src = std::fs::read("seashell.bin").unwrap();
    /// let mut bearer = StreamDecoderBuilder::new(0).build(src.as_slice()).unwrap();
    /// 
    /// assert_eq!(
    ///     get::<String>(&mut bearer).unwrap(),
    ///     "She sells seashells by the seashore".to_string()
    /// );
    /// let raw = bearer.into_raw();
    /// 
    /// let mut src = std::fs::read("ascii_seashell.bin").unwrap();
    /// let mut bearer = StreamDecoderBuilder::new(0).build(src.as_slice()).unwrap();
    /// 
    /// assert_eq!(
    ///     get::<u64>(&mut bearer).unwrap(),
    ///     115104101108108
    /// );
    /// ```
    #[inline]
    pub fn into_raw(self) -> RegionBuffer {
        self.region_buffer
    }
}

impl<R> StreamDecoder<R>
where
    R: Reader,
{
    /// Creates a new [`StreamDecoder`].
    ///
    /// Not exposed publicly in favor of [`StreamDecoderBuilder::build`]
    pub(crate) fn new(src: R, size: usize, seed: Option<u64>) -> Result<Self, RegionError> {
        let mut decoder = Self {
            region_buffer: RegionBuffer::new(size),
            switch: Pull::new(src, seed)
        };
        decoder.region_buffer.pass(&mut decoder.switch)?;

        Ok(decoder)
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
            self.region_buffer.pass(&mut self.switch)?;
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
/// This trait is recommended to be implemented using `#[derive(Decode)]` macro.
/// 
/// # Example
///
/// Implementing using `Decode` macro (requires `derive` feature):
/// 
/// ```
/// use barrique::Decode;
///
/// #[derive(Decode)]
/// enum Days {
///     OldDays(u64),
///     NewDays
/// }
/// ```
///
/// # Safety
///
/// Implementation of a `decode` method must initialize `dst`.
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
