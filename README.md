![Logo](https://github.com/rockstorm-io/barrique/blob/main/LOGO.png?raw=true)
##
[![License](https://img.shields.io/crates/l/barrique)](LICENSE)
[![Version](https://img.shields.io/crates/v/barrique)](https://crates.io/crates/barrique)

Barrique is a schema-based binary serialization format featuring compression, metadata and streaming. The format was designed for locally storing large (>8 MiB) long-term memory objects.

The format is capable of storing any data representable in a sequence of bytes, meaning a schema is required to interpret the bytes. Data stored in regions up to `64 KiB - 1` bytes of raw bytes, each region holds a header. The specification has two available formats: Stream and Frame. Frame is a wrapper for Stream featuring a metadata header and a magic number. Stream is a core structure the format declares representing a sequence of regions.

## Implementation

Standard barrique implementation published as a Rust crate. The implementation features in-place initialization for decoding pipeline and `#[derive(...)]` macros,
an example of which showed below:

```rust
use barrique::{Encode, Decode};

#[derive(Encode, Decode)]
struct Hello {
    hello: String,
    world: World,
}

#[derive(Encode, Decode)]
enum World {
    Flat(i32, #[barrique(skip)] u8),
    Circular
}
```

Visit [Docs](https:/docs.rs/barrique) for more information.

## Specification

Barrique serialization format specification declared [here](https://github.com/rockstorm-io/barrique/blob/main/SPEC)
