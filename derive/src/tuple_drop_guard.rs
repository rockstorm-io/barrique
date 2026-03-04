//! Implementation of main method of the `Drop` trait for tuple guards.
//!
//! Simple macro is not capable to implement this behavior due to `=>` token
//! dismiss, although this is the only part implemented using
//! procedural macros

use crate::TupleGuardInput;

use proc_macro2::TokenStream;
use quote::quote;

/// Generates body of a `drop` method implementation for tuple drop guard
pub fn impl_input(input: TupleGuardInput) -> TokenStream {
    let drop_impls = (1..input.fields.len())
        .map(|idx| {
            let drop_impls = input.fields.iter()
                .rev()
                .skip(input.fields.len() - idx)
                .map(|field| {
                    quote! {
                        unsafe {
                            core::ptr::drop_in_place(&raw mut (*self.ptr).#field);
                        }
                    }
                })
                .collect::<Vec<_>>();

            let match_arm = idx as u8;
            quote! {
                #match_arm => {
                    #(#drop_impls)*
                }
            }
        })
        .collect::<Vec<_>>();

    quote! {
        match self.count {
            0 => {},
            #(#drop_impls),*
            _ => unreachable!()
        }
    }
}