# Derive macros for [barrique](https://crates.io/crate/barrique)

This crate provides two derives: `Encode` and `Decode`, see examples in the main documentation.

### Container-level attributes

`#[barrique(tag_repr = "...")]` (enum only) — specifies a type which will be used to represent enum's tag.

### Field-level attributes

`#[barrique(decode_with = "...")]` — specifies a function which will be used to decode this field.

`#[barrique(encode_with = "...")]` — specifies a function which will be used to encode this field.

`#[barrique(skip = "...")]` — skips encoding of this field and uses specified expression or `Default::default()` for decoded value