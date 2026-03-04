use crate::encode::struct_::impl_struct;
use crate::encode::enum_::impl_enum;
use crate::DeriveArgs;

use darling::{Error, FromDeriveInput, Result};
use darling::ast::Data;

use proc_macro2::TokenStream;
use syn::DeriveInput;
use quote::quote;

mod enum_;
mod struct_;

/// Generates `Encode` implementation for a struct or an enum
pub fn impl_derive(input: DeriveInput) -> Result<TokenStream> {
    let args = DeriveArgs::from_derive_input(&input)?;

    let (encode_impl, size_of_impl) = match &args.data {
        Data::Enum(variants) => impl_enum(variants, args.tag_repr),
        Data::Struct(fields) => {
            if args.tag_repr.is_some() {
                return Err(Error::custom(
                    "#[barrique(tag_repr = \"...\")] can not be applied to structs",
                ));
            }
            impl_struct(fields)
        }
    };

    let (impl_generics, ty_generics, where_clause) = args.generics.split_for_impl();
    let ident = &args.ident;

    let derive_impl = quote! {
        #[automatically_derived]
        const _: () = {
            use ::barrique::encode::{Encode, EncodeError, EncodeBearer};

            impl #impl_generics Encode for #ident #ty_generics #where_clause {
                fn encode(bearer: &mut impl EncodeBearer, src: &Self) -> Result<(), EncodeError>
                {
                    #encode_impl
                }

                fn size_of(&self) -> usize {
                    #size_of_impl
                }
            }
        };
    };
    Ok(derive_impl)
}
