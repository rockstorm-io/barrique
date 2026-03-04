//! `Decode` trait implementation for structs:
//! ```non-rust
//! #[derive(Decode)]
//! struct <...> {
//!     ...
//! }
//! ```

use crate::{DeriveArgs, Field, SkipBy, struct_member};

use quote::{quote, quote_spanned};
use proc_macro2::TokenStream;
use darling::ast::Fields;

use syn::spanned::Spanned;
use syn::Member;
use crate::decode::skip_by_value;

/// Generates body of `decode` method for `Decode` trait implementation
pub fn impl_struct(args: &DeriveArgs, fields: &Fields<Field>) -> TokenStream {
    let field_impls = fields
        .iter()
        .enumerate()
        .map(|(idx, field)| {
            let member = struct_member(&field.ident, idx);

            let field_impl = impl_struct_field(field, member);
            let count = idx + 1;

            quote! {
                #field_impl
                guard.count = #count;
            }
        })
        .collect::<Vec<_>>();

    let drop_guard_impl = impl_drop_guard(fields);

    let (impl_generics, ty_generics, where_clause) = args.generics.split_for_impl();
    let ident = &args.ident;

    quote! {
        struct LocalDropGuard #impl_generics #where_clause {
            ptr: *mut #ident #ty_generics,
            count: usize,
        }

        impl #impl_generics Drop for LocalDropGuard #ty_generics #where_clause {
            fn drop(&mut self) {
                #drop_guard_impl
            }
        }

        let ptr = dst.as_mut_ptr();
        let mut guard = LocalDropGuard {
            count: 0,
            ptr
        };
        #(#field_impls)*

        core::mem::forget(guard);
        Ok(())
    }
}

/// Generates a part of `Decode` implementation responsible of decoding `field`
fn impl_struct_field(field: &Field, member: Member) -> TokenStream {
    if let Some(default) = &field.skip {
        let value = skip_by_value(default);
        let field_impl = quote! {
            unsafe { (&raw mut (*ptr).#member).write(#value); }
        };

        return field_impl;
    }

    if let Some(with) = &field.decode_with {
        quote_spanned! { with.span() =>
            #with(bearer, unsafe { &mut *(&raw mut (*ptr).#member).cast() })?;
        }
    } else {
        let ty = &field.ty;
        quote! {
            <#ty as Decode>::decode(
                bearer,
                unsafe { &mut *(&raw mut (*ptr).#member).cast() }
            )?;
        }
    }
}

/// Generates a body of `drop` method for a struct drop guard
fn impl_drop_guard(fields: &Fields<Field>) -> TokenStream {
    let drop_impls = (1..fields.len())
        .map(|idx| {
            let drop_impl = fields.fields[..idx]
                .iter()
                .enumerate()
                .filter_map(|(idx, field)| {
                    if field.skip.is_some() {
                        return None;
                    }

                    let member = struct_member(&field.ident, idx);
                    let field_drop_impl = quote! {
                        unsafe {
                            core::ptr::drop_in_place(&raw mut (*self.ptr).#member);
                        }
                    };

                    Some(field_drop_impl)
                })
                .collect::<Vec<_>>();

            quote! {
                #idx => {
                    #(#drop_impl)*
                }
            }
        })
        .collect::<Vec<_>>();

    quote! {
        match self.count {
            0 => {}
            #(#drop_impls)*
            _ => unreachable!()
        }
    }
}