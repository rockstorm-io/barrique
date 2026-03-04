//! `Encode` trait implementation for enums:
//! ```non-rust
//! #[derive(Encode)]
//! enum <...> {
//!     ...
//! }
//! ```

use crate::{Variant, anon_ident_iter};

use proc_macro2::{Ident, TokenStream};
use quote::{quote, quote_spanned};
use darling::ast::Style;

use syn::spanned::Spanned;
use syn::Type;

/// Generates bodies of `encode` and `size_of` methods for `Encode` trait implementation
pub fn impl_enum(variants: &[Variant], tag_repr: Option<Type>) -> (TokenStream, TokenStream) {
    let mut size_of_impls = Vec::with_capacity(variants.len());

    let variant_impls = variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let tag_impl = impl_variant_tag(index, &tag_repr);
            match variant.fields.style {
                newtype @ (Style::Tuple | Style::Struct) => {
                    let (match_arm, fields_impl, size_of_impl)
                        = impl_newtype_variant(variant, newtype);

                    size_of_impls.push(size_of_impl);
                    quote! {
                        #match_arm => {
                            #tag_impl
                            #fields_impl
                            Ok(())
                        }
                    }
                },
                Style::Unit => {
                    let ident = &variant.ident;
                    quote! {
                        Self::#ident => {
                            #tag_impl
                            Ok(())
                        }
                    }
                }
            }
        })
        .collect::<Vec<_>>();

    (
        quote! {
            match src {
                #(#variant_impls)*
            }
        },
        quote! {
            match self {
                #(#size_of_impls)*
                _ => 0
            }
        }
    )
}

/// Generates a part of `encode` method implementation responsible for encoding enum tag
fn impl_variant_tag(idx: usize, tag_repr: &Option<Type>) -> TokenStream {
    let tag = idx as u32;
    match tag_repr {
        Some(tag_repr) => quote! {
            <#tag_repr as Encode>::encode(bearer, &(#tag as #tag_repr))?;
        },
        None => quote! {
            u32::encode(bearer, &#tag)?;
        },
    }
}

/// Generates implementation for `variant`, returning a match arm, a field encoding body
/// and a `size_of` method body
fn impl_newtype_variant(variant: &Variant, style: Style) -> (TokenStream, TokenStream, TokenStream) {
    let mut match_patterns = Vec::with_capacity(variant.fields.len());
    let mut size_of_impls = Vec::with_capacity(variant.fields.len());
    let mut anon_iter = anon_ident_iter();

    let field_impls = variant.fields
        .iter()
        .filter_map(|field| {
            let ident = &field
                .ident
                .clone()
                .unwrap_or(anon_iter.next().unwrap());
            
            if field.skip.is_some() {
                if style.is_tuple() {
                    match_patterns.push(quote! { _ })
                } else {
                    match_patterns.push(quote! { #ident: _ })
                }
                return None;
            }

            size_of_impls.push(quote! { #ident.size_of() });
            match_patterns.push(quote! { #ident });

            let ty = &field.ty;
            let field_impl = match &field.encode_with {
                Some(with) => quote_spanned! { with.span() =>
                    #with(bearer, #ident)?;
                },
                None => quote! {
                    <#ty as Encode>::encode(bearer, #ident)?;
                },
            };
            Some(field_impl)
        })
        .collect::<Vec<_>>();

    let match_arm = impl_enum_match_pattern(&variant.ident, &match_patterns, &style);
    (
        quote! { #match_arm },
        quote! { #(#field_impls)* },
        quote! {
            #match_arm => {
                let mut count = 0usize;
                #(
                    count += #size_of_impls;
                )*
                count
            }
        }
    )
}

/// Generates an enum match expression arm with `patterns` for the `ident` variant
fn impl_enum_match_pattern(ident: &Ident, patterns: &[TokenStream], style: &Style) -> TokenStream {
    if style.is_tuple() {
        quote! {
            Self::#ident(#(#patterns),*)
        }
    } else {
        quote! {
            Self::#ident{#(#patterns),*}
        }
    }
}