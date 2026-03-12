//! Implementations of [`Reader`] and [`Writer`] traits for
//! types requiring additional tracking of the state

use crate::decode::{ReadError, ReadResult, Reader};
use crate::encode::{WriteError, WriteResult, Writer};

use core::mem::MaybeUninit;

#[cfg(feature = "std")]
use std::io::{Read, Write};
#[cfg(feature = "std")]
use core::cell::UnsafeCell;

/// A wrapper for any type implementing [`AsRef<[u8]>`] or [`AsMut<[u8]>`]
/// providing implementations of [`Reader`] and [`Writer`] traits.
pub struct Cursor<T> {
    cursor: usize,
    inner: T,
}

impl<T> Cursor<T> {
    /// Create a new [`Cursor`]
    #[inline]
    pub const fn new(inner: T) -> Cursor<T> {
        Self { cursor: 0, inner }
    }

    /// Advance inner cursor by `n`.
    ///
    /// # Panic
    ///
    /// This method will panic if addition overflows `usize`
    #[inline]
    pub fn advance(&mut self, n: usize) {
        self.cursor += n;
    }
}

impl<T> Reader for Cursor<T>
where
    T: AsRef<[u8]>,
{
    #[inline]
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]> {
        self.inner
            .as_ref()
            .get(self.cursor..self.cursor + n)
            .ok_or(ReadError::OutOfBounds)
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.advance(n)
    }
}

impl<T> Writer for Cursor<T>
where
    T: AsMut<[u8]>,
{
    #[inline]
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        let slice = self
            .inner
            .as_mut()
            .get_mut(n..)
            .ok_or(WriteError::OutOfBounds)?;

        Ok(unsafe {
            // Safety: `Self` assumed to be initialized, so cast to `MaybeUninit` is allowed
            core::slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len())
        })
    }

    #[inline]
    unsafe fn commit(&mut self, n: usize) {
        self.advance(n);
    }
}

/// A buffer of incremental scoped span.
///
/// The underlying back buffer of [`CursorView`]
#[derive(Default)]
#[cfg(feature = "std")]
struct IncrementalBuffer {
    buffer: Vec<u8>,
    position: usize,
}

#[cfg(feature = "std")]
impl IncrementalBuffer {
    /// Extends capacity of this buffer to fit at least `n` more bytes by treating
    /// `overlap_index..` range as available capacity and reallocating the buffer
    /// if capacity is still insufficient.
    ///
    /// Returned slice will have length of `n`
    fn extend_capacity_overlapping(&mut self, n: usize, overlap_index: usize) -> &mut [MaybeUninit<u8>] {
        unsafe {
            // Safety:
            // - `new_len` is less or equal to `self.buffer.len()`
            self.buffer.set_len(overlap_index.min(self.buffer.len()));
        }

        self.buffer.reserve(n * 2);
        self.buffer
            .spare_capacity_mut()[..n]
            .as_mut()
    }

    /// Resizes this buffer to fit at least `n` more bytes and returns a slice
    /// pointing to extended span with length greater or equal to `n`.
    ///
    /// Span before the `position` cursor is drained
    fn resize_drained(&mut self, n: usize) -> &mut [u8] {
        self.buffer.drain(..self.position);
        self.position = 0;

        let old_len = self.buffer.len();
        // Might implement a custom realloc in order to escape unnecessary
        // `memmove` call and replace it with single copy with pointer
        // offset.
        // Note: `memmove` and `memcpy` are fast, so consider benchmarking
        // the overhead (on ranges < 64 KiB because `CursorView` is mostly
        // intended as a `Reader` or `Writer` for regions streams, which
        // maximum request is 64 KiB ~)
        self.buffer.resize(old_len + n * 2, 0);

        &mut self.buffer[old_len..]
    }

    /// Returns a slice starting from the cursor with length of `n`
    fn get(&self, n: usize) -> Option<&[u8]> {
        self.buffer.get(self.position..self.position + n)
    }

    /// Returns a slice pointing to the whole buffer
    fn get_all(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// Returns a length of the buffer
    fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Marks next `n` bytes starting from the current cursor as consumed
    fn consume(&mut self, n: usize) {
        self.position = core::cmp::min(self.position + n, self.buffer.len());
    }

    /// Increments the length by `n`.
    ///
    /// # Safety
    ///
    /// - incrementing length by `n` must not overflow the capacity.
    ///
    /// - the `old_len..new_len` elements must be initialized
    unsafe fn inc_length(&mut self, n: usize) {
        unsafe { self.buffer.set_len(self.buffer.len().saturating_add(n)); }
    }

    /// Decrements the length by `n`
    fn dec_length(&mut self, n: usize) {
        unsafe {
            // Safety: `saturating_sub` will not allow overflow and 0 is always a valid length
            self.buffer.set_len(self.buffer.len().saturating_sub(n));
        }
    }
}

/// A cursor for incremental window of data.
///
/// This struct is a wrapper for types implementing [`std::io::Read`]
/// or [`std::io::Write`], providing implementations of [`Reader`]
/// and [`Writer`] accordingly.
///
/// Results of IO requests performed by this wrapper stored in
/// a buffer and drained dynamically.
///
/// # Example
///
/// Usage within [`StreamEncoder`]:
/// 
/// ```rust, no_run
/// use barrique::encode::{StreamEncoder, Encode};
/// use barrique::cursor::CursorView;
/// use barrique::region::{AllocOrd, Seed};
///
/// use std::fs::File;
///
/// let src = File::create("large_file.bin").unwrap();
/// let mut src = CursorView::new(src);
///
/// let mut bearer = StreamEncoder::new(&mut src, Seed::new(0), AllocOrd::full());
/// <String as Encode>::encode(&mut bearer, &String::from("Hello, world!"))
///     .unwrap();
///
/// let dst = File::open("large_file.bin").unwrap();
/// let dst = CursorView::new(dst);
///
/// // ... decode
/// ```
///
/// [`StreamEncoder`]: crate::encode::StreamEncoder
/// [`std::io::Write`]: Write
/// [`std::io::Read`]: Read
#[cfg(feature = "std")]
pub struct CursorView<T>(UnsafeCell<CursorViewInner<T>>);

#[cfg(feature = "std")]
impl<T> CursorView<T> {
    /// Constructs a new [`CursorView`]
    pub fn new(inner: T) -> Self {
        Self(UnsafeCell::new(CursorViewInner::new(inner)))
    }

    /// Constructs a new [`CursorView`] with a buffer of `capacity` capacity
    pub fn with_capacity(inner: T, capacity: usize) -> Self {
        Self(UnsafeCell::new(CursorViewInner::with_capacity(inner, capacity)))
    }
}

#[cfg(feature = "std")]
impl<T> CursorView<T>
where
    T: Write,
{
    /// Flushes all contents of this buffer
    pub fn flush(&mut self) -> std::io::Result<()> {
        let borrow = self.0.get_mut();

        borrow.inner.write_all(borrow.buffer.get_all())?;
        borrow.inner.flush()
    }
}

#[cfg(feature = "std")]
impl<T> Reader for CursorView<T>
where
    T: Read,
{
    fn read_borrow(&self, n: usize) -> ReadResult<&[u8]> {
        let unique = unsafe {
            // Safety:
            // - reference we acquire is unique because there is no way
            //   caller to access `cell`.
            // - no other references to `cell` coexist within the body of
            //   this method
            &mut *self.0.get()
        };

        // `if let` construct will trigger a borrowck error in
        // the `resize_drained` mutable borrow
        if unique.buffer.get(n).is_some() {
            return Ok(unique.buffer.get(n).unwrap());
        }

        let extended = unique.buffer.resize_drained(n);
        let extended_len = extended.len();

        let bytes_read = unique.inner.read(extended)?;
        unique.buffer.dec_length(extended_len - bytes_read);

        unique.buffer.get(n).ok_or(ReadError::OutOfBounds)
    }

    fn advance(&mut self, n: usize) {
        self.0.get_mut().buffer.consume(n);
    }
}

#[cfg(feature = "std")]
impl<T> Writer for CursorView<T>
where
    T: Write,
{
    fn allocate(&mut self, n: usize) -> WriteResult<&mut [MaybeUninit<u8>]> {
        let borrow = self.0.get_mut();

        // This will decide to invoke flush or not in order to not allocate too
        // much memory. Might improve this in the future
        let overlap_index = if borrow.buffer.len().div_ceil(n) > 3 {
            borrow.inner.write_all(borrow.buffer.get_all())?;
            0
        } else {
            borrow.buffer.len()
        };

        Ok(borrow.buffer.extend_capacity_overlapping(n, overlap_index))
    }

    unsafe fn commit(&mut self, n: usize) {
        let borrow = self.0.get_mut();
        unsafe {
            // Safety: `inc_len` sets the same requirements as `Writer::commit`
            borrow.buffer.inc_length(n)
        }
    }
}

/// The main contents of the [`CursorView`]
#[cfg(feature = "std")]
struct CursorViewInner<T> {
    buffer: IncrementalBuffer,
    inner: T
}

#[cfg(feature = "std")]
impl<T> CursorViewInner<T> {
    /// Constructs a new [`CursorViewInner`]
    fn new(inner: T) -> Self {
        Self {
            buffer: IncrementalBuffer { buffer: Vec::new(), position: 0 },
            inner
        }
    }

    /// Constructs a new [`CursorViewInner`] with `capacity`
    fn with_capacity(inner: T, capacity: usize) -> Self {
        Self {
            buffer: IncrementalBuffer { buffer: Vec::with_capacity(capacity), position: 0 },
            inner
        }
    }
}