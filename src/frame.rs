use crate::region::Seed;
use crate::decode::{get, Decode, DecodeError, ReadError, Reader, StreamDecoder};
use crate::encode::{
    write_to_uninit, Encode, EncodeError, StreamEncoder, WriteError, Writer,
};
use crate::region::Size;

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::fmt::Display;
use core::ops::Deref;

/// A runtime error of [`Frame`] methods
#[derive(thiserror::Error, Debug)]
pub enum FrameError {
    #[error("Malformed label")]
    MalformedLabel,
    #[error("Non-ASCII label contents")]
    NonAsciiLabel,
    #[error("Leading magic number not found at expected position in source")]
    NoMagicNumber,
    #[error("Mismatch between flags found in the metadata and the actual environment")]
    EnvironmentMismatch,
    #[error("Read error: \"{0}\"")]
    ReadError(#[from] ReadError),
    #[error("Write error: \"{0}\"")]
    WriteError(#[from] WriteError),
}

/// Frame magic number
pub const MAGIC_NUM: u32 = 0x96EBCEA9;

/// Get magic number as bytes
const fn magic_num_bytes() -> [u8; 4] {
    MAGIC_NUM.to_le_bytes()
}

/// Verify `bytes` to represent `MAGIC_NUM`
const fn is_magic_num_bytes(bytes: [u8; 4]) -> bool {
    u32::from_le_bytes(bytes) == MAGIC_NUM
}

/// An owned frame metadata label, representing a static `ASCII`-only string.
///
/// This struct is provided since the label is restricted to maximum size
/// of `255` bytes, so storing it in heap-allocated [`String`] would be inefficient.
///
/// Label implements [`Deref<Target = str>`] trait, which allow to use it as any other `str` type
///
/// [`Deref<Target = str>`]: Deref
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Label {
    buf: [u8; 255],
    len: u8,
}

// Public methods
impl Label {
    /// Create a new [`Label`] from `str` string slice.
    ///
    /// `None` returned in case of non-`ASCII` input or length which is
    /// greater than `255`
    pub fn new(str: &str) -> Option<Self> {
        if !str.is_ascii() || str.len() > 255 {
            return None;
        }

        let mut buf = [0u8; 255];
        buf[..str.len()].copy_from_slice(str.as_bytes());

        Some(Self {
            len: str.len() as u8,
            buf,
        })
    }

    /// Extracts a slice containing entire label
    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe {
            // Safety: interface guarantees that `buf` contains only ASCII characters.
            core::str::from_utf8_unchecked(self.as_bytes())
        }
    }

    /// Extracts a byte array representing this label
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            // Safety: `len` can not be out of bounds since it's maximum value
            // is 255, which equal to `buf` size
            self.buf.get_unchecked(0..self.len as usize)
        }
    }

    /// Return length of this label
    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Check if this label is empty
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// Private methods
impl Label {
    /// Deserialize a label from the provided [`DecodeBearer`]
    fn decode_from_reader(src: &mut impl Reader) -> Result<Self, FrameError> {
        let len = src.read_borrow_const::<1>()?[0] as usize;
        src.advance(1);

        let str = unsafe {
            // Safety: `Label::new()` will check the input to be ASCII-only, so
            // non UTF-8 string would be dropped inside this method. This is
            // not a language UB since pull 792
            core::str::from_utf8_unchecked(
                src.read_borrow(len)
                    .map_err(|_| FrameError::MalformedLabel)?,
            )
        };

        let label = Label::new(str).ok_or(FrameError::NonAsciiLabel)?;
        src.advance(len);

        Ok(label)
    }

    /// Serialize this label into the provided [`EncodeBearer`]
    fn encode_into_writer(&self, dst: &mut impl Writer) -> Result<(), FrameError> {
        let len = self.len() + 1; // One more byte for a length marker

        let arena = dst.allocate(self.len() + 1)?;
        assert_eq!(arena.len(), len, "Invalid result of Writer implementation");

        arena[0].write(self.len);
        write_to_uninit(self.as_bytes(), &mut arena[1..]);

        unsafe {
            // Safety: `..len` bytes are initialized
            dst.commit(len);
        }
        Ok(())
    }
}

impl Default for Label {
    fn default() -> Self {
        Self {
            buf: [0u8; 255],
            len: 0,
        }
    }
}

impl Display for Label {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.deref())
    }
}

impl TryFrom<&str> for Label {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(())
    }
}

impl Deref for Label {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

/// In-memory representation of frame metadata header
///
/// # Byte format
///
/// Byte format of a metadata header variable between `2` and `265` bytes, depending
/// on its contents. Structure of the byte format, as specification stated, declared
/// as following:
///
/// ```non-rust
/// FrameMetadata:
/// [FrameDescriptor; 1-9][LabelLength; 1][Label; 0-255]
/// ```
///
/// `LabelLength` is a byte representing contiguous label length, `Label` is an ASCII-only
/// string serving as a small description of file contents or similar applications.
///
/// # Magic number
///
/// In this implementation, magic number itself embedded into [`FrameMetadata`] byte
/// representation, as the metadata header position in a frame expected to be after
/// a leading magic number
#[derive(Debug, Clone, Default)]
struct FrameMetadata {
    frame_descriptor: FrameDescriptor,
    label: Label,
}

impl FrameMetadata {
    /// Create a new [`FrameMetadata`] from the provided [`DecodeBearer`]
    fn decode_from_reader(src: &mut impl Reader) -> Result<Self, FrameError> {
        let leading_bytes = src.read_borrow_const::<4>()?;
        if !is_magic_num_bytes(leading_bytes) {
            return Err(FrameError::NoMagicNumber);
        }
        src.advance(4 /* size_of::<u32>() */);

        // The minimum possible size of region stream equals to 10 bytes, which is
        // single completely empty region with only a header, so we are able to
        // request possibly overkill length (if timestamp is not included) to
        // avoid doubling call to read a descriptor byte and a timestamp
        let frame_descriptor_bytes = src.read_borrow(FrameDescriptor::max_len())?;
        let frame_descriptor = FrameDescriptor::from_bytes(frame_descriptor_bytes)
            .expect("Invalid result of Reader implementation");
        src.advance(frame_descriptor.len());

        let label = Label::decode_from_reader(src)?;

        Ok(Self {
            frame_descriptor,
            label,
        })
    }

    /// Serialize this frame metadata into the provided [`EncodeBearer`]
    fn encode_into_writer(self, dst: &mut impl Writer) -> Result<(), FrameError> {
        let len = 4 /* size_of::<u32>() */ + self.frame_descriptor.len();

        // Since the label handles allocation itself, we're responsible only for writing
        // magic number and a frame descriptor
        let arena = dst.allocate(len)?;
        assert_eq!(arena.len(), len, "Invalid result of Writer implementation");

        write_to_uninit(&magic_num_bytes(), arena);
        self.frame_descriptor
            .into_bytes(&mut arena[4 /* size_of::<u32>() */..])
            .unwrap();

        unsafe {
            // Safety: `..len` bytes are initialized
            dst.commit(len);
        }

        self.label.encode_into_writer(dst)?;

        Ok(())
    }
}

/// Mask of seeded region hash flag
const MASK_SEEDED: u8 = 0b10000000;

/// Mask of timestamp inclusion flag
const MASK_TIMESTAMP: u8 = 0b01000000;

/// The frame descriptor flags representation.
///
/// As specification stated, frame descriptor of frame metadata is
/// a variable-sized byte span which hold specific frame flags or metadata
/// values (e.g. timestamp). At the time of current specification, there are
/// only two variables available — `seeded` flag and a timestamp value.
///
/// # Byte format
///
/// Frame descriptor byte format has size from `1` byte to `9` bytes, representation
/// of which at the time of current specification presented below:
///
/// ```non-rust
/// FrameDescriptor:
/// [Flags; 1][?Timestamp; 8]
/// ```
///
/// `Timestamp` is an optional generic time value limited to 8 bytes, interpretation
/// of which performed by caller. `Flags` is a byte which bit values represent
/// specific frame flags.
///
/// ```non-rust
/// Flags:
/// [Seeded; 7][HasTimestamp; 6][..ReservedSpace; 0..6]
/// ```
#[derive(Debug, Default, Clone)]
struct FrameDescriptor {
    /// Flag indicating that region hash is generated with seed
    seeded: bool,
    /// 8 byte generic timestamp included within descriptor header byte span
    timestamp: Option<u64>,
}

impl FrameDescriptor {
    /// Create a new [`FrameDescriptor`] from the provided source slice.
    ///
    /// `None` returned if `src` slice has insufficient length to deserialize
    /// variables involved, panic occurs in case of empty slice
    fn from_bytes(src: &[u8]) -> Option<Self> {
        let byte = src[0];

        let timestamp = if byte & MASK_TIMESTAMP != 0 {
            Some(u64::from_le_bytes(
                src.get(1..=8 /* size_of::<u64>()*/)?.try_into().unwrap(),
            ))
        } else {
            None
        };

        Some(Self {
            seeded: byte & MASK_SEEDED != 0,
            timestamp,
        })
    }

    /// Serialize this frame descriptor into `dst` buffer.
    ///
    /// `None` returned if the destination buffer provided has insufficient length
    /// to serialize variables involved, panic occurs in case of empty buffer.
    /// `Some` contains amount of bytes written
    fn into_bytes(self, dst: &mut [MaybeUninit<u8>]) -> Option<()> {
        let mut byte = 0u8;

        if self.seeded {
            byte |= MASK_SEEDED;
        }

        if let Some(timestamp) = self.timestamp {
            byte |= MASK_TIMESTAMP;

            // 8 for timestamp, 1 for descriptor byte
            if dst.len() < 8 /* size_of::<u64>() */ + 1 {
                return None;
            }

            write_to_uninit(&timestamp.to_le_bytes(), &mut dst[1..=8]);
        }

        dst.get_mut(0)?.write(byte);
        Some(())
    }

    /// Get a length of this [`FrameDescriptor`] byte representation
    const fn len(&self) -> usize {
        // If expression can be replaced with `map_or` when it gets stable const implementation
        1 + if self.timestamp.is_some() { 8 } else { 0 }
    }

    /// Return the maximum possible length of [`FrameDescriptor`] byte representation
    const fn max_len() -> usize {
        1 /* size_of::<u8>() */ + 8 /* size_of::<u64>() */
    }
}

/// A frame decoding and encoding interface.
///
/// Frames are specification defined wrappers for region streams, declared as
/// a magic number, metadata header and an actual region stream.
///
/// # Usage areas
///
/// The main purpose of frames is to provide metadata fields with force access
/// and lazy loaded streams, which can be useful if given data must be initially
/// verified. Frame descriptor also holds additional flags to verify pipeline
/// environment (e.g. seed) and a leading magic number will prevent reading
/// arbitrary contents
///
/// # Example
///
/// Storing a cat with proc-macro generated implementations:
///
/// ```
/// use barrique::region::{Size, Seed};
/// use barrique::frame::Frame;
/// use barrique::{Decode, Encode};
///
/// use std::time::{SystemTime, UNIX_EPOCH};
///
/// #[derive(Encode, Decode)]
/// struct Cat {
///     hungry: bool,
///     state: StateOfCat,
/// }
///
/// #[derive(Encode, Decode)]
/// enum StateOfCat {
///     Sleeping,
///     Purring {
///         sound_level: u8
///     },
/// }
///
/// let cat = Cat {
///     hungry: false,
///     state: StateOfCat::Purring {
///         sound_level: 1
///     }
/// };
/// let mut dst = vec![];
///
/// let frame = Frame::new(&mut dst, Seed::new(0))
///     .with_label("A cat".try_into().unwrap())
///     .with_timestamp(
///         SystemTime::now()
///             .duration_since(UNIX_EPOCH)
///             .unwrap()
///             .as_secs()
/// );
/// frame.encode(cat).unwrap();
/// ```
///
/// The `A cat` label will indicate that a file `dst` written to stores
/// data representing a cat (similar to, for example, zip comment).
///
/// The UNIX timestamp can be used in decoding later if, for example, we need
/// to validate that the cat we're reading now is the newest one
pub struct Frame<V, B> {
    _phantom: PhantomData<V>,
    metadata: FrameMetadata,
    seed: Seed,
    base: B,
}

impl<T, W> Frame<T, W>
where
    T: Encode,
    W: Writer,
{
    /// Constructs a new [`Frame`] bound to `dst` destination [`Writer`]
    #[inline]
    pub fn new(dst: W, seed: Seed) -> Self {
        Self {
            _phantom: Default::default(),
            metadata: Default::default(),
            base: dst,
            seed,
        }
    }

    /// Assigns a [`Label`] to the metadata of this frame
    #[inline]
    pub fn with_label(mut self, label: Label) -> Frame<T, W> {
        self.metadata.label = label;
        self
    }

    /// Assigns a generic 8-byte timestamp to the metadata of this frame
    #[inline]
    pub fn with_timestamp(mut self, timestamp: u64) -> Frame<T, W> {
        self.metadata.frame_descriptor.timestamp = Some(timestamp);
        self
    }

    /// Encodes this frame with the `value` provided.
    /// 
    /// # Example
    /// 
    /// ```
    /// use barrique::frame::Frame;
    /// use barrique::region::Seed;
    ///
    /// let mut dst = vec![];
    /// let frame = Frame::new(&mut dst, 0.into());
    /// 
    /// frame.encode("Hello, world".to_string()).unwrap();
    /// ```
    pub fn encode(mut self, value: T) -> Result<(), EncodeError> {
        if !self.seed.is_empty() {
            self.metadata.frame_descriptor.seeded = true;
        }
        self.metadata.encode_into_writer(&mut self.base)?;

        let mut encoder = StreamEncoder::new(self.base, self.seed, Size::Auto(&value));
        let result = T::encode(&mut encoder, &value);

        encoder.flush()?;
        result
    }
}

impl<T, R> Frame<T, R>
where
    T: Decode,
    R: Reader,
{
    /// Decodes a new [`Frame`] from `src` source [`Reader`].
    ///
    /// Generic `T` value is lazy-decoded, meaning this method will not construct
    /// [`DecodeBearer`] and decode frame value itself, which allows caller to
    /// access metadata values without complete decoding overhead.
    ///
    /// # Error considerations
    ///
    /// This method will only invoke *opening* a frame, that is, reading a header,
    /// so any issues with the encoded value are not revealed yet
    #[inline]
    pub fn decode(mut src: R, seed: Seed) -> Result<Self, FrameError> {
        let metadata = FrameMetadata::decode_from_reader(&mut src)?;

        Ok(Self {
            _phantom: Default::default(),
            base: src,
            metadata,
            seed,
        })
    }

    /// Returns a [`Label`] contained in this frame’s metadata.
    ///
    /// `None` returned if label is empty
    #[inline]
    pub fn get_label(&self) -> Option<&Label> {
        (!self.metadata.label.is_empty()).then_some(&self.metadata.label)
    }

    /// Returns an 8-byte header timestamp contained in this frame’s metadata.
    ///
    /// `None` returned if timestamp was not included
    #[inline]
    pub fn get_timestamp(&self) -> Option<u64> {
        self.metadata.frame_descriptor.timestamp
    }

    /// Returns a deserialized value of this frame.
    ///
    /// The value is lazy-decoded, meaning that [`DecodeBearer`] will be constructed
    /// and value decoded only on explicit value access request.
    ///
    /// # Example
    ///
    /// ```rust, no_run
    /// use barrique::region::{Size, Seed};
    /// use barrique::frame::Frame;
    ///
    /// let bytes = std::fs::read("serialized.bin").unwrap();
    /// let frame = Frame::<(), _>::decode(bytes.as_slice(), Seed::new(0))
    ///     .expect("Failed to open a frame");
    ///
    /// // Verify the label of our frame
    /// if frame.get_label() != Some(&"Verified".try_into().unwrap()) {
    ///     panic!("Invalid label: possibly incorrect contents");
    /// }
    ///
    /// let _ = frame.get_value(Size::manual(bytes.len()));
    /// ```
    ///
    /// # Metadata considerations
    ///
    /// If `seeded` metadata flag mismatched from actual significance of `seed` provided
    /// in [`Frame::decode`] method, [`DecodeError::FrameError`] error variant
    /// returned, as the situation described indicates incorrect pipeline for
    /// targeted contents
    #[inline]
    pub fn get_value(self, ord: Size) -> Result<T, DecodeError> {
        if self.seed.is_empty() == self.metadata.frame_descriptor.seeded {
            return Err(FrameError::EnvironmentMismatch.into());
        }

        let mut decoder = StreamDecoder::new(self.base, self.seed, ord)?;
        get(&mut decoder)
    }
}