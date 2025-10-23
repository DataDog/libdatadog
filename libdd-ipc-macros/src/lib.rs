// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::FnArg::Typed;
use syn::__private::Span;
use syn::{parse_quote, Arm, FieldPat, Ident, ItemTrait, Member, Pat, Stmt, TraitItem};

fn snake_to_camel(ident_str: &str) -> String {
    let mut camel_ty = String::with_capacity(ident_str.len());

    let mut last_char_was_underscore = true;
    for c in ident_str.chars() {
        match c {
            '_' => last_char_was_underscore = true,
            c if last_char_was_underscore => {
                camel_ty.extend(c.to_uppercase());
                last_char_was_underscore = false;
            }
            c => camel_ty.extend(c.to_lowercase()),
        }
    }

    camel_ty.shrink_to_fit();
    camel_ty
}

#[proc_macro_attribute]
pub fn impl_transfer_handles(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let mut item: ItemTrait = syn::parse(input).unwrap();
    let req_name = format_ident!("{}Request", item.ident);
    let res_name = format_ident!("{}Response", item.ident);
    let mut arms_req_move: Vec<Arm> = vec![];
    let mut arms_req_recv: Vec<Arm> = vec![];
    let mut arms_res_move: Vec<Arm> = vec![];
    let mut arms_res_recv: Vec<Arm> = vec![];
    for inner in item.items.iter_mut() {
        if let TraitItem::Fn(ref mut func) = inner {
            let mut params: Vec<FieldPat> = vec![];
            let mut stmts_move: Vec<Stmt> = vec![];
            let mut stmts_recv: Vec<Stmt> = vec![];
            for any_arg in func.sig.inputs.iter_mut() {
                if let Typed(ref mut arg) = any_arg {
                    let orig_attr_num = arg.attrs.len();
                    arg.attrs.retain(|attr| {
                        attr.meta.path().to_token_stream().to_string() != "SerializedHandle"
                    });
                    if orig_attr_num != arg.attrs.len() {
                        if let Pat::Ident(ref ident) = *arg.pat {
                            params.push(FieldPat {
                                attrs: vec![],
                                member: Member::Named(ident.ident.clone()),
                                colon_token: None,
                                pat: Box::new(parse_quote! { #ident }),
                            });
                            stmts_move.push(
                                parse_quote! { __transport.copy_handle(#ident.clone().into())?; },
                            );
                            stmts_recv.push(parse_quote! { #ident.receive_handles(__transport)?; });
                        }
                    }
                }
            }
            let method = Ident::new(
                &snake_to_camel(&func.sig.ident.to_string()),
                Span::mixed_site(),
            );
            if !params.is_empty() {
                arms_req_move.push(parse_quote! {
                    #req_name::#method { #(#params,)* .. } => {
                        #(#stmts_move)*
                        Ok(())
                    }
                });
                arms_req_recv.push(parse_quote! {
                    #req_name::#method { #(#params,)* .. } => {
                        #(#stmts_recv)*
                        Ok(())
                    }
                });
            }
            let orig_attr_num = func.attrs.len();
            func.attrs.retain(|attr| {
                attr.meta.path().to_token_stream().to_string() != "SerializedHandle"
            });
            if orig_attr_num != func.attrs.len() {
                arms_res_move.push(parse_quote! {
                    #res_name::#method(response) => response.copy_handles(transport)
                });
                arms_res_recv.push(parse_quote! {
                    #res_name::#method(response) => response.receive_handles(transport)
                });
            }
        }
    }

    TokenStream::from(quote! {
        #item

        impl datadog_ipc::handles::TransferHandles for #req_name {
            fn copy_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &self,
                __transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(
                        #arms_req_move,
                    )*
                    _ => Ok(()),
                }
            }

            fn receive_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &mut self,
                __transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(
                        #arms_req_recv,
                    )*
                    _ => Ok(()),
                }
            }
        }

        impl datadog_ipc::handles::TransferHandles for #res_name {
            fn copy_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &self,
                transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(
                        #arms_res_move,
                    )*
                    _ => Ok(()),
                }
            }

            fn receive_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &mut self,
                transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(
                        #arms_res_recv,
                    )*
                    _ => Ok(()),
                }
            }
        }
    })
}
