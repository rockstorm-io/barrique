use darling::ast::{Data, Fields};
use darling::{FromDeriveInput, FromField, FromMeta, FromVariant, Result};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{ToTokens, quote};
use syn::{AttrStyle, Attribute, DeriveInput, Expr, ExprPath, Generics, Type, TypeGenerics, Visibility, parse_macro_input, Member, Token, LitInt};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;

mod decode;
mod encode;
mod tuple_drop_guard;

#[derive(FromField)]
#[darling(attributes(barrique), forward_attrs)]
pub(crate) struct Field {
    ident: Option<Ident>,
    ty: Type,

    #[darling(default)]
    encode_with: Option<ExprPath>,
    #[darling(default)]
    decode_with: Option<ExprPath>,
    
    skip: Option<SkipBy>,
}

#[derive(FromMeta)]
#[darling(from_word = || Ok(Self::Default))]
enum SkipBy {
    Default,
    DefaultExpr(Expr),
}

#[derive(FromVariant)]
#[darling(attributes(barrique), forward_attrs)]
pub(crate) struct Variant {
    ident: Ident,
    fields: Fields<Field>,
}

type DeriveData = Data<Variant, Field>;

#[derive(FromDeriveInput)]
#[darling(attributes(barrique), forward_attrs)]
pub(crate) struct DeriveArgs {
    ident: Ident,
    data: DeriveData,
    generics: Generics,
    vis: Visibility,
    
    #[darling(default)]
    tag_repr: Option<Type>,
}

// /// Check if given collection of attributes has `#[repr(transparent)]` or `#[repr(C)]`.
// ///
// /// This is a basic proc-macro level UB check for `transmute` attribute, which will
// /// result in incorrect pointer cast if implementing type has variable layout
// pub(crate) fn valid_to_transmute_repr(attrs: &[Attribute]) -> syn::Result<()> {
//     for attr in attrs {
//         if !attr.path().is_ident("repr") {
//             continue;
//         }
// 
//         return attr.parse_nested_meta(|meta| {
//             if meta.path.is_ident("transparent") || meta.path.is_ident("C") {
//                 return Ok(());
//             }
//             Err(meta.error("`#[barrique(transmute = \"...\")]` requires either `#[repr(C)]` or `#[repr(transparent)]`"))
//         });
//     }
//     Err(syn::Error::new(
//         Span::call_site(),
//         "Cannot implement `#[barrique(transmute = \"...\")]` for `#[repr(Rust)]` type",
//     ))
// }
// 
// /// Generate static assertions of cast between `ident` and `transmute_to`
// pub(crate) fn transmute_assert(
//     ident: &Ident,
//     transmute_to: &Type,
//     generics: &TypeGenerics,
// ) -> proc_macro2::TokenStream {
//     if generics.to_token_stream().is_empty() {
//         quote! {
//             const _: () = {
//                 let _ = core::mem::transmute::<#ident, #transmute_to>;
//                 let _: [(); core::mem::size_of::<#ident>()] = [(); core::mem::size_of::<#transmute_to>()];
//             };
//         }
//     } else {
//         // If implementing type has generics, we are not able to perform
//         // static assertion since generic parameters make a type
//         // variable-sized
//         quote! {}
//     }
// }

/// Returns a cycling iterator over anonymous identifiers.
///
/// # Example
///
/// ```non-rust
/// let mut iter = anon_ident_iter();
///
/// assert_eq!(iter.next().unwrap(), "a0".into());
/// assert_eq!(iter.next().unwrap(), "a1".into());
/// ```
pub(crate) fn anon_ident_iter() -> impl Iterator<Item = Ident> {
    ('a'..='z').cycle().enumerate().map(|(idx, ident)| {
        let name = format!("{}{}", ident, idx / 26);
        Ident::new(&name, Span::call_site())
    })
}

/// Returns a struct [`Member`] from `Some(ident)` or from `index` if `ident` is `None`.
/// 
/// # Example
/// 
/// ```non-rust
/// let ident = Ident::new("hello", Span::call_site());
/// 
/// assert_eq!(Member::Named(ident), struct_member(Some(&ident), 0));
/// assert_eq!(Member::Unnamed(0), struct_member(None, 0));
/// ```
pub(crate) fn struct_member(ident: &Option<Ident>, index: usize) -> Member {
    ident.clone()
        .map(|ident| ident.into())
        .unwrap_or_else(|| index.into())
}

#[proc_macro_derive(Encode, attributes(barrique))]
pub fn derive_encode(token_stream: TokenStream) -> TokenStream {
    let input = parse_macro_input!(token_stream as DeriveInput);
    match encode::impl_derive(input) {
        Ok(output) => output.into(),
        Err(error) => error.write_errors().into(),
    }
}

#[proc_macro_derive(Decode, attributes(barrique))]
pub fn derive_decode(token_stream: TokenStream) -> TokenStream {
    let input = parse_macro_input!(token_stream as DeriveInput);
    match decode::impl_derive(input) {
        Ok(output) => output.into(),
        Err(error) => error.write_errors().into(),
    }
}

pub(crate) struct TupleGuardInput {
    fields: Punctuated<LitInt, Token![,]>,
}

impl Parse for TupleGuardInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let input = Punctuated::parse_terminated(input)?;
        Ok(TupleGuardInput { fields: input })
    }
}

#[proc_macro]
pub fn tuple_drop_guard(token_stream: TokenStream) -> TokenStream {
    let input = parse_macro_input!(token_stream as TupleGuardInput);
    tuple_drop_guard::impl_input(input).into()
}