use crate::decode::{ReadError, Reader};
use crate::encode::{write_to_uninit, Encode, WriteError, Writer};
use crate::lz4::{compress_bound, CompressStream, DecompressStream};

use core::slice::SliceIndex;
use core::num::NonZeroU64;

use alloc::vec::Vec;

use twox_hash::XxHash64;

/// Maximum size of region body.
pub(crate) const REGION_SIZE: usize = 64 * 1024 - 1;

/// Returns the maximum possible size of an encoded region stream
/// containing a value sized in `size` bytes.
///
/// This does not account for possible early state switches in
/// non-last regions because that directly depends
/// on [`Encode`] implementation
pub const fn max_encoded_size(size: usize) -> usize {
    (size / REGION_SIZE) * compress_bound(REGION_SIZE)
        + compress_bound(size - (size / REGION_SIZE) * REGION_SIZE)
        + size.div_ceil(REGION_SIZE) * HEADER_SIZE
}

/// Seed of a region hash, represented in 8 bytes.
///
/// # Examples
///
/// ```
/// use barrique::encode::StreamEncoder;
/// use barrique::region::Seed;
///
/// let seed = Seed::new(0);
/// let _ = StreamEncoder::new(vec![], Some(seed), Default::default());
/// ```
#[derive(Default, Copy, Clone, Debug, PartialEq)]
#[repr(transparent)]
pub struct Seed {
    inner: u64,
}

// Public methods

impl Seed {
    /// Constructs a new [`Seed`]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: seed,
        }
    }
}

// Private methods

impl Seed {
    /// Produces a hash of `bytes` using this [`Seed`]
    pub(crate) fn hash(&self, bytes: &[u8]) -> u64 {
        XxHash64::oneshot(self.inner, bytes)
    }
}

impl From<u64> for Seed {
    fn from(seed: u64) -> Self {
        Self::new(seed)
    }
}

impl From<NonZeroU64> for Seed {
    fn from(seed: NonZeroU64) -> Self {
        Self::new(seed.get())
    }
}

/// A hint of capacity required for bearer to stream a pipeline.
///
/// Bearers are inherently pairs of buffers holding some amount of
/// bytes and operating pointwise on given state of these buffers
/// and the size of these buffers is limited to 64 KiB. This enum
/// is essentially capacity value which will be used to allocate
/// a bearer.
///
/// # Example
///
/// An example of hinting size of a value which will be encoded:
///
/// ```
/// use barrique::region::{Size, Seed};
/// use barrique::encode::StreamEncoder;
///
/// let value = String::from("Hello, world!");
/// let ord = Size::Auto(&value);
///
/// // `Size::Auto` usually applied to encode bearers only
/// let mut encoder = StreamEncoder::new(vec![], Seed::new(0), ord);
///
/// // Now, if we call value's implementation, bearer will exactly
/// // fit encoded bytes without any remaining capacity:
/// // ... <String as Encode>::encode(&mut encoder, &value);
/// ```
///
/// # Default value
///
/// Default value of [`Size`] is 4 KiB
pub enum Size<T = ()>
where
    T: Encode,
{
    /// An explicitly specified capacity in bytes
    Manual(isize),
    /// Capacity derived from `<T as Encode>::size_of` method call
    Auto(T),
    /// Hint to allocate the maximum capacity possible
    Full,
}

impl Default for Size {
    fn default() -> Self {
        Size::Manual(4 * 1024)
    }
}

impl Size {
    /// `Size::<()>::Full`
    #[inline]
    pub fn full() -> Self {
        Size::Full
    }

    /// `Size::<()>::Manual`
    #[inline]
    pub fn manual(count: usize) -> Self {
        Size::Manual(count as isize)
    }
}

impl<T: Encode> Size<T> {
    /// Returns capacity hinted in bytes
    #[inline]
    pub(crate) fn cap(&self) -> usize {
        match self {
            Self::Manual(cap) => (*cap).max(0) as usize,
            Self::Auto(hint) => hint.size_of(),
            Self::Full => REGION_SIZE * 2,
        }
    }
}

/// An error type returned by region processing related methods
#[derive(Debug, thiserror::Error)]
pub enum RegionError {
    #[error("Failed to read data into a region buffer")]
    ReadFailure(#[from] ReadError),
    #[error("Failed to allocate capacity to write a region buffer")]
    WriteFailure(#[from] WriteError),
    #[error("Invalid region size hint")]
    InvalidSizeHint,
    #[error("Malformed region")]
    MalformedRegion,
    #[error("Hash is not valid for contiguous region")]
    InvalidHash,
    #[error("Requested region operation is out of bounds of current capacity")]
    OutOfBounds,
}

/// Two uninitialized region buffers, used for region streaming.
///
/// LZ4 streaming compression implemented using `_continue` functions, which
/// require to keep data referred in the stream alive. Only one additional region
/// stored since LZ4 window size is 64 KiB, which is the same as region size.
///
/// The `previous` is used only in an even switch, meaning it will be used
/// after first 64 KiB span streamed, which is the reason of why it is
/// wrapped into [`Option`], escaping additional 64 KiB allocation for
/// smaller passes
struct DoubleBuffer {
    curr: Vec<u8>,
    prev: Option<Vec<u8>>,
}

impl DoubleBuffer {
    /// Constructs a new [`DoubleBuffer`] capable of streaming `cap` bytes
    fn new(cap: usize) -> Self {
        let curr = Vec::with_capacity(cap.min(REGION_SIZE));
        let prev =
            (cap > REGION_SIZE).then(|| Vec::with_capacity((cap - REGION_SIZE).min(REGION_SIZE)));

        Self { curr, prev }
    }

    /// Swaps current and previous buffers
    fn swap(&mut self) {
        let prev = self.prev.get_or_insert_with(|| {
            // This branch is called only in case of incorrect `Size` hint,
            // so the allocation is small
            let predict = if self.curr.len() > 4 * 1024 /* 4 Kib */ { self.curr.len() / 4 } else { 256 };
            Vec::with_capacity(predict)
        });
        core::mem::swap(&mut self.curr, prev);
    }

    /// Invokes `pass` method of the given `authority` with access to current buffer.
    ///
    /// Access to contents of [`DoubleBuffer`] achieved via [`StateSwitch`] only, implementations
    /// of which are limited to [`Push`] and [`Pull`]
    fn authorize_pass<S: StateSwitch>(&mut self, authority: &mut S) -> Result<(), RegionError> {
        authority.pass(&mut self.curr)
    }

    /// Returns the length of the current buffer
    fn len(&self) -> usize {
        self.curr.len()
    }

    /// Returns requested range or index of current buffer
    fn get<I>(&self, idx: I) -> Option<&I::Output>
    where
        I: SliceIndex<[u8]>,
    {
        self.curr.get(idx)
    }

    /// Extends current buffer with `src` bytes.
    fn extend(&mut self, src: &[u8]) {
        self.curr.extend_from_slice(src);
    }
}

/// A buffer for operations on region stream, containing a [`DoubleBuffer`]
/// and a cursor for tracking current position
pub(crate) struct RegionBuffer {
    buffer: DoubleBuffer,
    cursor: usize,
}

impl RegionBuffer {
    /// Constructs an empty region buffer capable of streaming `cap` bytes
    pub fn new(cap: usize) -> Self {
        Self {
            buffer: DoubleBuffer::new(cap),
            cursor: 0,
        }
    }

    /// Returns remaining capacity of this region buffer
    pub fn remaining_cap(&self) -> usize {
        REGION_SIZE - self.buffer.len()
    }

    /// Returns remaining length of this region buffer
    pub fn remaining_len(&self) -> usize {
        self.buffer.len() - self.cursor
    }

    /// Calls a pass implementation for provided [`StateSwitch`], performing
    /// region state switch on the current region buffer.
    ///
    /// Cursor will be reset to `0` after a successful pass
    pub fn pass<S: StateSwitch>(&mut self, authority: &mut S) -> Result<(), RegionError> {
        self.buffer.authorize_pass(authority)?;
        self.cursor = 0;

        Ok(())
    }

    /// Swaps the current buffer with previous
    pub fn swap(&mut self) {
        self.buffer.swap();
    }

    /// Reads `n` bytes of this region buffer from the current cursor position
    pub fn read(&mut self, n: usize) -> Option<&[u8]> {
        let bytes = self.buffer.get(self.cursor..self.cursor + n)?;
        self.cursor = self.cursor.saturating_add(n);

        Some(bytes)
    }

    /// Writes `src` bytes into this region buffer.
    ///
    /// Inner buffer will be reallocated if capacity is insufficient
    /// to hold `src` more bytes.
    pub fn write(&mut self, src: &[u8]) {
        self.buffer.extend(src)
    }
}

/// Byte size of region header
const HEADER_SIZE: usize = 12;

/// In-memory representation of region header.
///
/// Byte format of region header stated in specification declared as following:
///
/// ```non-rust
/// RegionHeader:
/// [CompressedSize; 2][AllocSize; 2][RegionHash; 8]
/// ```
///
/// `CompressedSize` and `AllocSize` are 2-byte unsigned integers indicating compressed
/// contents and raw contents lengths accordingly. `RegionHash` is a result of XXHASH64
/// hash function performed on raw (decompressed) contents of this region
#[derive(Debug)]
struct RegionHeader {
    compressed_size: u16,
    alloc_size: u16,
    hash: u64,
}

impl RegionHeader {
    /// Constructs a new [`RegionHeader`] from serialized header bytes.
    ///
    /// This method will panic if `bytes` length is less than [`HEADER_SIZE`]
    fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() < HEADER_SIZE {
            panic!("Attempt to create a header from slice with length less than HEADER_SIZE");
        }

        let compressed_size = u16::from_le_bytes(bytes[..2].try_into().unwrap());
        let alloc_size = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
        let hash = u64::from_le_bytes(bytes[4..12].try_into().unwrap());

        Self { compressed_size, alloc_size, hash }
    }

    /// Serializes this [`RegionHeader`] into bytes
    fn into_bytes(self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];

        buf[..2].clone_from_slice(&self.compressed_size.to_le_bytes());
        buf[2..4].clone_from_slice(&self.alloc_size.to_le_bytes());
        buf[4..12].copy_from_slice(&self.hash.to_le_bytes());

        buf
    }
}

mod private {
    use super::*;

    pub trait Sealed {}

    impl<W: Writer> Sealed for Push<W> {}
    impl<R: Reader> Sealed for Pull<R> {}
}

/// A trait defining an implementation of region state switch fiduciary to get access
/// to region buffer contents.
///
/// The region switch is a pass of current state of a region buffer
/// which main point is to prepare the buffer for streaming next
/// region of bytes.
///
/// # Pull switch
///
/// The [`Pull`] implementation is a switch of read pipeline: new
/// region is read, decompressed and copied into the given
/// region buffer.
///
/// # Push switch
///
/// The [`Push`] implementation is a switch of write pipeline: current
/// contents of the region buffer compressed, structured into
/// a region format and flushed into internal destination
pub(crate) trait StateSwitch: private::Sealed {
    /// Perform a state switch on the `buf` passed
    fn pass(&mut self, buf: &mut Vec<u8>) -> Result<(), RegionError>;
}

/// A [`StateSwitch`] implementation for [`StreamDecoder`] operations
///
/// [`StreamDecoder`]: crate::decode::StreamDecoder
pub(crate) struct Pull<R>
where
    R: Reader,
{
    stream: DecompressStream,
    seed: Option<Seed>,
    source: R,
}

impl<R> Pull<R>
where
    R: Reader,
{
    /// Constructs a new [`Pull`] authority with `src` source [`Reader`] and `seed`
    pub fn new(src: R, seed: Option<Seed>) -> Self {
        Self {
            stream: DecompressStream::new(),
            source: src,
            seed,
        }
    }

    /// Returns contained reader, consuming `self`
    pub fn into_inner(self) -> R {
        self.source
    }

    /// Returns contained region hash seed
    pub const fn seed(&self) -> Option<Seed> {
        self.seed
    }
}

impl<R> StateSwitch for Pull<R>
where
    R: Reader,
{
    /// Reads next region from the [`Reader`] source, decompresses the body to the region buffer and
    /// verifies a hash of resulting data. Buffer swap must be performed *before* the pass.
    ///
    /// This method will panic if [`Reader`] implementation returned invalid result
    fn pass(&mut self, buf: &mut Vec<u8>) -> Result<(), RegionError> {
        let header = RegionHeader::from_bytes(self.source.read_borrow(HEADER_SIZE)?);

        let size = HEADER_SIZE + header.compressed_size as usize;
        let bytes = self.source.read_borrow(size).map_err(|e| match e {
            ReadError::OutOfBounds => RegionError::InvalidSizeHint,
            #[cfg_attr(not(feature = "std"), allow(unreachable_patterns))]
            _ => e.into(),
        })?;

        // `Reader` is not an unsafe trait, so check of incorrect implementation required. For
        // region header, constructor will panic in case of slice with length less
        // than requested `HEADER_SIZE`
        assert_eq!(
            bytes.len(),
            size,
            "Mismatch between Reader implementation result and actual length requested"
        );

        unsafe {
            // Safety:
            // - length set to `0` so no elements need to be initialized.
            // - `0` is always less or equal to capacity
            buf.set_len(0);
        }

        buf.reserve(header.alloc_size as usize);
        unsafe {
            // Safety:
            // - decompressed data stored in internal buffer inside `RegionBuffer`, semantics
            //   of which guarantees region buffer to live long enough.
            // - extremely unlikely that single memory allocation exceeds `c_int::MAX`
            let init = self
                .stream
                .decompress(&bytes[HEADER_SIZE..], buf.spare_capacity_mut())
                .ok_or(RegionError::MalformedRegion)?;

            // Safety: `..init` bytes are initialized by `decompress` call
            // and within bounds of `buf` capacity since it was given a
            // slice from `spare_capacity_mut`
            buf.set_len(init);
        }

        let hash = self.seed.map(|seed| seed.hash(buf)).unwrap_or(0);
        if hash != header.hash {
            return Err(RegionError::InvalidHash);
        }

        self.source.advance(size);
        Ok(())
    }
}

/// A [`StateSwitch`] implementation for [`WriteBearer`] operations
///
/// [`WriteBearer`]: crate::encode::WriteBearer
pub(crate) struct Push<W>
where
    W: Writer,
{
    stream: CompressStream,
    destination: W,
    seed: Seed,
}

impl<W> Push<W>
where
    W: Writer,
{
    /// Creates a new [`Push`] with `dst` [`Writer`] destination and `seed`
    pub fn new(dst: W, seed: Seed) -> Self {
        Self {
            stream: CompressStream::new(),
            destination: dst,
            seed,
        }
    }

    /// Returns contained writer, consuming `self`
    pub fn into_inner(self) -> W {
        self.destination
    }

    /// Returns contained region hash seed
    pub const fn seed(&self) -> Seed {
        self.seed
    }
}

impl<W> StateSwitch for Push<W>
where
    W: Writer,
{
    /// Serializes initialized part of `buf` and flushed it into internal [`Writer`].
    /// Buffer swap must be performed *after* a pass.
    ///
    /// This method will panic in case of invalid result of [`Writer`] implementation.
    fn pass(&mut self, buf: &mut Vec<u8>) -> Result<(), RegionError> {
        // Situation when region buffer appears empty (e.g. unnecessary flush) is not dangerous,
        // but since LZ4 wrapper treats 0 as an error, this check is performed
        // to improve error clarity
        if buf.is_empty() {
            return Ok(());
        }

        let size = compress_bound(buf.len()) + HEADER_SIZE;
        let arena = self.destination.allocate(size)?;

        // Similarly to `Pull` implementation, we check for incorrect implementation
        assert_eq!(
            arena.len(),
            size,
            "Mismatch between Writer implementation result and actual length requested"
        );

        let compressed = unsafe {
            // Safety:
            // - uncompressed data stored in internal buffer inside `RegionBuffer`, semantics
            //   of which guarantees region buffer to live long enough.
            // - extremely unlikely that single memory allocation exceeds `c_int::MAX`
            self.stream
                .compress(buf, &mut arena[HEADER_SIZE..])
                .ok_or(RegionError::MalformedRegion)?
        };

        let header = RegionHeader {
            compressed_size: compressed as u16,
            alloc_size: buf.len() as u16,
            hash: self.seed.hash(buf),
        };
        write_to_uninit(header.into_bytes().as_slice(), arena);

        unsafe {
            // Safety:
            // - length set to `0` so no elements need to be initialized.
            // - `0` is always less or equal to capacity.
            // Note: this is necessary because `extend_nonoverlapping` method extends
            // from the length, which is, unlike the cursor, not reset by the
            // region buffer
            buf.set_len(0);

            // Safety:
            // - `compress` call initialized `HEADER_SIZE..compressed` range,
            //   header range initialized by `write_to_uninit` above.
            // - commitment of `n` can not overflow since `compressed` can't be
            //   greater than `compress_bound`, which is the size of requested
            //   allocation without header bytes
            self.destination.commit(HEADER_SIZE + compressed);
        }
        Ok(())
    }
}
