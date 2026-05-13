use crate::decode::struct_::impl_struct;
use crate::decode::enum_::impl_enum;
use crate::{DeriveArgs, SkipBy};

use quote::{quote, quote_spanned};
use proc_macro2::TokenStream;

use darling::{Error, FromDeriveInput, Result};
use darling::ast::Data;

use syn::spanned::Spanned;
use syn::DeriveInput;

mod struct_;
mod enum_;

/// Returns token stream of a value pasted into skipped field
pub(super) fn skip_by_value(skip_by: &SkipBy) -> TokenStream {
    match &skip_by.expr {
        Some(expr) => {
            quote_spanned! { expr.span() =>
                #expr
            }
        },
        _ => quote! { Default::default() }
    }
}

/// Generates `Decode` trait implementation for a struct or an enum
pub fn impl_derive(input: DeriveInput) -> Result<TokenStream> {
    let args = DeriveArgs::from_derive_input(&input)?;
    let (impl_generics, ty_generics, where_clause) = args.generics.split_for_impl();
    let ident = &args.ident;

    let decode_impl = match &args.data {
        Data::Enum(variants) => impl_enum(&args, variants),
        Data::Struct(fields) => {
            if args.tag_repr.is_some() {
                return Err(Error::custom(
                    "#[barrique(tag_repr = \"...\")] can not be applied to structs",
                ));
            }
            impl_struct(&args, fields)
        }
    };

    let derive_impl = quote! {
        #[doc(hidden)]
        const _: () = {
            use ::barrique::decode::{Decode, DecodeError, DecodeBearer};
            use ::core::mem::MaybeUninit;

            #[automatically_derived]
            unsafe impl #impl_generics Decode for #ident #ty_generics #where_clause {
                fn decode(bearer: &mut impl DecodeBearer, dst: &mut MaybeUninit<Self>) -> Result<(), DecodeError>
                {
                    #decode_impl
                }
            }
        };
    };
    Ok(derive_impl)
}