//! Implementations of [`Reader`] and [`Writer`] traits for
//! types requiring additional tracking of the state

use crate::decode::{ReadError, ReadResult, Reader};
use crate::encode::{WriteError, Writer};

use core::mem::MaybeUninit;

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
    fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
        let slice = self
            .inner
            .as_mut()
            .get_mut(n..)?;

        Some(unsafe {
            // Safety: `Self` assumed to be initialized, so cast to `MaybeUninit` is allowed
            core::slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len())
        })
    }

    #[inline]
    unsafe fn assume_init(&mut self, n: usize) {
        self.advance(n);
    }
}

#[cfg(feature = "std")]
pub use view::*;

#[cfg(feature = "std")]
mod view {
    use super::*;

    use crate::lz4::compress_bound;

    use std::io::{Error, Read, Write};
    use core::cell::UnsafeCell;

    /// A wrapper over [`Write`] implementations providing [`encode::Writer`] trait implementation.
    ///
    /// A flush will be performed automatically when the instance is dropped, although it's recommended
    /// to manually invoke [`Writer::flush`] to handle potential error.
    ///
    /// Data written into a writer initially located inside an internal [`Vec`] and stays there unless
    /// a flush invoked.
    ///
    /// # Example
    ///
    /// ```
    /// use std::ptr;
    /// use barrique::cursor::CursorWriter;
    /// use barrique::encode::Writer;
    ///
    /// let sample = "Boo! 👻".as_bytes();
    /// let mut dst = vec![];
    /// let mut writer = CursorWriter::new(&mut dst);
    ///
    /// let bytes = writer.write_mut(sample.len()).unwrap();
    ///
    /// // `CursorWriter` will always return a slice with exact length requested
    /// debug_assert_eq!(sample.len(), bytes.len());
    /// unsafe {
    ///     // Safety: ranges do not overlap and both `sample` and `bytes` have same size
    ///     ptr::copy_nonoverlapping(sample.as_ptr(), bytes.as_mut_ptr().cast(), sample.len());
    ///
    ///     // Safety: we've initialized these bytes
    ///     writer.assume_init(sample.len());
    /// }
    ///
    /// drop(writer);
    /// assert_eq!(sample, dst);
    /// ```
    pub struct CursorWriter<W>
    where
        W: Write,
    {
        writer: W,
        buffer: Vec<u8>,
        error: bool
    }

    impl<W> CursorWriter<W>
    where
        W: Write,
    {
        /// Creates a new [`CursorWriter`] over a [`Write`] implementation.
        /// 
        /// This method does not perform any allocations
        pub fn new(writer: W) -> CursorWriter<W> {
            Self {
                writer,
                buffer: Vec::new(),
                error: false,
            }
        }
        
        /// Creates a new [`CursorWriter`] over a [`Write`] implementation with specified `capacity`
        pub fn with_capacity(writer: W, capacity: usize) -> CursorWriter<W> {
            Self {
                writer,
                buffer: Vec::with_capacity(capacity),
                error: false
            }
        }
    }

    impl<W> Writer for CursorWriter<W>
    where
        W: Write,
    {
        fn write_mut(&mut self, n: usize) -> Option<&mut [MaybeUninit<u8>]> {
            self.buffer.reserve(n);
            self.buffer.spare_capacity_mut().get_mut(..n)
        }

        unsafe fn assume_init(&mut self, n: usize) {
            unsafe {
                // Safety: caller guarantees `..n` bytes to be initialized
                self.buffer.set_len(self.buffer.len() + n);
            }
        }

        fn flush(&mut self) -> Result<(), WriteError> {
            let mut handle_error = |e: Error| {
                self.error = true;
                Err(e.into())
            };

            if let Err(e) = self.writer.write_all(&self.buffer) {
                return handle_error(e);
            }
            if let Err(e) = self.writer.flush() {
                return handle_error(e);
            }

            unsafe {
                // Safety: `0` is always a valid length
                self.buffer.set_len(0);
            }

            Ok(())
        }
    }

    impl<W> Drop for CursorWriter<W>
    where
        W: Write,
    {
        fn drop(&mut self) {
            if !self.error {
                let _ = self.writer.write_all(&self.buffer);
                let _ = self.writer.flush();
            }
        }
    }

    /// A wrapper over [`Read`] implementations providing [`decode::Reader`] trait implementation.
    ///
    /// Essentially, each [`CursorReader`] holds a [`Vec`] storing data received from reader
    /// and extends it as needed. The growth is amortized unless `new_cap` is less than the maximum
    /// size of compressed region.
    ///
    /// # Example
    ///
    /// ```
    /// use barrique::cursor::CursorReader;
    /// use barrique::decode::Reader;
    ///
    /// let src = "Hello, world!".as_bytes();
    /// let mut reader = CursorReader::new(src);
    ///
    /// assert_eq!(b"Hello, w", reader.read_borrow(8).unwrap());
    /// reader.advance(8);
    /// assert_eq!(b"orld!", reader.read_borrow(5).unwrap());
    /// ```
    ///
    /// [`decode::Reader`]: Reader
    pub struct CursorReader<R>
    where
        R: Read,
    {
        inner: UnsafeCell<CursorReaderInner<R>>,
    }

    impl<R> CursorReader<R>
    where
        R: Read,
    {
        /// Creates a new [`CursorReader`] over a [`Read`] implementation.
        ///
        /// This method does not perform any allocations
        pub fn new(reader: R) -> CursorReader<R> {
            CursorReader::with_capacity(reader, 0)
        }

        /// Creates a new [`CursorReader`] over a [`Read`] implementation with specified `capacity`.
        pub fn with_capacity(reader: R, capacity: usize) -> CursorReader<R> {
            Self {
                inner: UnsafeCell::new(CursorReaderInner {
                    buffer: Vec::with_capacity(capacity),
                    start: 0,
                    end: 0,
                    reader
                })
            }
        }
    }

    /// Internal implementation of [`CursorReader`], managing growth
    /// and interaction with [`Read`] `reader`
    struct CursorReaderInner<R>
    where
        R: Read,
    {
        buffer: Vec<u8>,
        start: usize,
        end: usize,

        reader: R,
    }

    /// If `n > THRESHOLD`, amortized grow is disabled and the new capacity equals to `n`
    const THRESHOLD: usize = compress_bound(u16::MAX as usize) / 2;

    impl<R> CursorReaderInner<R>
    where
        R: Read,
    {
        /// Reads next `n` bytes
        fn read(&mut self, n: usize) -> Result<&[u8], ReadError> {
            if self.start + n > self.end {
                if self.end < self.buffer.len() {
                    return Err(ReadError::OutOfBounds);
                }

                self.grow(n)?;
            }

            Ok(&self.buffer[self.start..self.start + n])
        }

        /// Grows this reader and requests at least `n` more bytes from [`Read`] implementation.
        ///
        /// A new capacity is amortized, i.e. greater than or equal to `n`, but it will try to keep
        /// itself close to [`THRESHOLD`], which is the maximum size of a region body
        fn grow(&mut self, n: usize) -> Result<(), ReadError> {
            // Amortized grow, but keep the capacity close to region size
            let new_len = if n > THRESHOLD { n } else { n * 2 };

            // We construct a new `Vec` because `..self.start` bytes are no longer
            // needed and if we try to truncate and then resize it will only waste
            // CPU cycles
            let mut grow = Vec::with_capacity(new_len);
            grow.extend_from_slice(&self.buffer[self.start..self.end]);
            grow.resize(grow.capacity(), 0);

            self.end = self.reader.read(&mut grow[self.end - self.start..])?;
            self.buffer = grow;
            self.start = 0;

            Ok(())
        }
    }

    impl<R> Reader for CursorReader<R>
    where
        R: Read
    {
        fn read_borrow(&self, n: usize) -> ReadResult<&[u8]> {
            let unique = unsafe {
                // Safety: reference to `self.inner` can not be obtained
                // outside this method
                &mut *self.inner.get()
            };

            unique.read(n)
        }

        fn advance(&mut self, n: usize) {
            self.inner.get_mut().start += n;
        }
    }
}