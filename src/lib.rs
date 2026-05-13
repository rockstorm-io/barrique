#![doc = include_str!("../CRATE_README.md")]

#![cfg_attr(not(feature = "std"), no_std)]

// Underlying C implementation of LZ4 depends on allocation unless specific preprocessor
// definition is stated, which is impossible with current bindings. Either way pushing
// 128 KiB size RegionBuffer to the stack is a little bit unsuitable
extern crate alloc;

#[cfg(feature = "derive")]
pub use barrique_derive::{Encode, Decode};
pub(crate) use barrique_derive::tuple_drop_guard;

pub mod cursor;
pub mod decode;
pub mod encode;
pub mod frame;
pub mod r#impl;
pub mod region;

mod lz4;

#[cfg(test)]
mod tests {
    use crate::encode::{write_to_uninit, Encode, Writer, StreamEncoderBuilder};
    use crate::decode::{Decode, Reader, StreamDecoderBuilder};
    use crate::cursor::{CursorReader, CursorWriter};
    use crate::region::{Push, RegionBuffer};
    use crate::frame::Frame;

    use core::mem::MaybeUninit;

    #[test]
    fn back_and_forth() {
        let sample = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut dst = vec![];

        let mut encoder = StreamEncoderBuilder::new(0).build(&mut dst);

        Encode::encode(&mut encoder, &sample).unwrap();
        encoder.flush().unwrap();

        let mut decoder = StreamDecoderBuilder::new(0).build(dst.as_slice()).unwrap();

        let mut value = MaybeUninit::uninit();
        <[i32; 10] as Decode>::decode(&mut decoder, &mut value).unwrap();
        assert_eq!(unsafe { value.assume_init() }, sample);
    }

    #[test]
    fn frame_back_and_forth() {
        let sample = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut dst = vec![];

        let frame = Frame::new(&mut dst);
        frame.encode(sample).unwrap();

        let frame = Frame::<[i32; 10], _>::decode(dst.as_slice(), None).unwrap();
        assert_eq!(frame.get_value(0).unwrap(), sample);
    }

    #[test]
    fn frame_metadata() {
        let mut dst = vec![];

        let frame = Frame::<i32, _>::new(&mut dst)
            .with_label("A snack".try_into().unwrap())
            .with_timestamp(u64::MAX);

        // We need to pass at least some data so metadata gets flushed
        frame.encode(0).unwrap();

        let frame = Frame::<(), _>::decode(dst.as_slice(), None).unwrap();

        assert_eq!(frame.get_label().unwrap().as_str(), "A snack");
        assert_eq!(frame.get_timestamp().unwrap(), u64::MAX);
    }

    #[test]
    fn assert_region_static() {
        let sample = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let serialized_static
            = [11, 0, 10, 0, 216, 132, 159, 113, 125, 26, 190, 198, 160, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

        let mut dst = vec![];
        let mut switch = Push::new(&mut dst, Some(0));

        let mut region = RegionBuffer::new(0);
        region.write(&sample);
        region.pass(&mut switch).unwrap();

        assert_eq!(dst, serialized_static);
    }

    #[cfg(feature = "std")]
    #[test]
    fn cursor_writer() {
        let sample = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

        let mut dst = vec![];
        let mut writer = CursorWriter::new(&mut dst);

        write_to_uninit(&sample, writer.write_mut(10).unwrap());
        unsafe {
            writer.assume_init(10);
        }

        drop(writer);

        assert_eq!(dst, sample);
    }

    #[cfg(feature = "std")]
    #[test]
    fn cursor_reader() {
        let src = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut reader = CursorReader::new(src.as_slice());

        assert_eq!(reader.read_borrow(2).unwrap(), &[1, 2]);
        reader.advance(2);
        assert_eq!(reader.read_borrow(8).unwrap(), &[3, 4, 5, 6, 7, 8, 9, 10]);
    }
}