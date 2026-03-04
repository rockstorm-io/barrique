//! `Decode` trait implementation for enums:
//! ```non-rust
//! #[derive(Decode)]
//! enum <...> {
//!     ...
//! }
//! ```

use crate::{anon_ident_iter, DeriveArgs, Field, Variant};
use crate::decode::skip_by_value;

use quote::{quote, quote_spanned};
use proc_macro2::{Span, TokenStream};
use darling::ast::Style;

use syn::spanned::Spanned;
use syn::{LitInt, Type};

/// Generates `decode` method implementation for an enum
pub fn impl_enum(args: &DeriveArgs, variants: &[Variant]) -> TokenStream {
    let variant_impls = variants.iter()
        .enumerate()
        .map(|(idx, variant)| {
            let ident = &variant.ident;
            let match_arm = LitInt::new(&idx.to_string(), Span::call_site());

            match variant.fields.style {
                Style::Tuple | Style::Struct => {
                    let variant_impl = impl_newtype_variant(variant);
                    quote! {
                        #match_arm => {
                            let value = { #variant_impl };
                            dst.write(value);
                        }
                    }
                },
                Style::Unit => {
                    quote! {
                        #match_arm => {
                            dst.write(Self::#ident);
                        }
                    }
                }
            }
        })
        .collect::<Vec<_>>();

    let tag_impl = impl_enum_tag(&args.tag_repr);
    quote! {
        #tag_impl
        match tag {
            #(#variant_impls),*
            _ => return Err(DecodeError::InvalidPattern)
        }
        Ok(())
    }
}

/// Generates part of `decode` implementation for a newtype `variant`
fn impl_newtype_variant(variant: &Variant) -> TokenStream {
    let mut field_idents = Vec::with_capacity(variant.fields.len());
    let mut anon_iter = anon_ident_iter();

    let field_impls = variant.fields.fields.iter()
        .map(|field| {
            let value_impl = impl_variant_field_value(field);
            let ident = field
                .ident
                .clone()
                .unwrap_or(anon_iter.next().unwrap());

            let field_impl = quote! {
                let #ident = { #value_impl };
            };
            field_idents.push(ident);

            field_impl
        })
        .collect::<Vec<_>>();

    let ident = &variant.ident;
    if variant.fields.is_tuple() {
        quote! {
            #(#field_impls)*
            Self::#ident(#(#field_idents),*)
        }
    } else {
        quote! {
            #(#field_impls)*
            Self::#ident { #(#field_idents),* }
        }
    }
}

/// Generates an expression for decoding value of `field`
fn impl_variant_field_value(field: &Field) -> TokenStream {
    if let Some(default) = &field.skip {
        let value = skip_by_value(default);
        return quote! {
            #value
        };
    }

    let field_impl = if let Some(with) = &field.decode_with {
        quote_spanned! { with.span() =>
            #with(bearer, &mut uninit)?
        }
    } else {
        let ty = &field.ty;
        quote! {
            <#ty as Decode>::decode(bearer, &mut uninit)?;
        }
    };

    quote! {
        let mut uninit = MaybeUninit::uninit();
        #field_impl
        unsafe { uninit.assume_init() }
    }
}

/// Generates part of `decode` method implementation responsible for decoding an enum tag
fn impl_enum_tag(tag_repr: &Option<Type>) -> TokenStream {
    let tag_impl = if let Some(ty) = tag_repr {
        quote! {
            <#ty as Decode>::decode(bearer, &mut uninit)?;
        }
    } else {
        quote! {
            u32::decode(bearer, &mut uninit)?;
        }
    };

    quote! {
        let mut uninit = MaybeUninit::uninit();
        #tag_impl
        let tag = unsafe { uninit.assume_init() };
    }
}