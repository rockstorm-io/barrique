//! `Encode` trait implementation for structs:
//! ```non-rust
//! #[derive(Encode)]
//! struct <...> {
//!     ...
//! }
//! ```

use crate::{struct_member, Field};

use darling::ast::Fields;
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};

use syn::Member;
use syn::spanned::Spanned;

/// Generates bodies of `encode` and `size_of` methods for `Encode` trait implementation
pub fn impl_struct(fields: &Fields<Field>) -> (TokenStream, TokenStream) {
    let mut size_of_fields = Vec::with_capacity(fields.len());
    let impl_fields = fields
        .iter()
        .filter(|field| field.skip.is_none())
        .enumerate()
        .map(|(idx, field)| {
            let member: Member = struct_member(&field.ident, idx);

            size_of_fields.push(quote! {
                self.#member.size_of()
            });

            impl_struct_field_encode(field, &member)
        })
        .collect::<Vec<_>>();

    (
        quote! {
            #(#impl_fields)*
            Ok(())
        },
        quote! {
            let mut count = 0_usize;
            #(
                count += #size_of_fields;
            )*
            count
        },
    )
}

/// Generates part of an `encode` method implementation for the `field`
fn impl_struct_field_encode(field: &Field, member: &Member) -> TokenStream {
    let ty = &field.ty;
    match &field.encode_with {
        Some(with) => quote_spanned! { field.encode_with.span() =>
            #with(bearer, &src.#member)?;
        },
        None => quote! {
            <#ty as Encode>::encode(bearer, &src.#member)?;
        },
    }
}
