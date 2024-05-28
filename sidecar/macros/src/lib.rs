// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::FnArg::Typed;
use syn::__private::Span;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, parse_quote, Arm, Ident, ItemTrait, Pat, TraitItem};

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
pub fn extract_request_id(_attr: TokenStream, mut input: TokenStream) -> TokenStream {
    let item: ItemTrait = syn::parse(input.clone()).unwrap();
    let name = &format_ident!("{}Request", item.ident);
    let mut arms: Vec<Arm> = vec![];
    for inner in item.items {
        if let TraitItem::Fn(func) = inner {
            for any_arg in func.sig.inputs {
                if let Typed(arg) = any_arg {
                    if let Pat::Ident(ident) = *arg.pat {
                        let matched_enum_type = match ident.ident.to_string().as_str() {
                            "session_id" => Some(format_ident!("SessionId")),
                            "instance_id" => Some(format_ident!("InstanceId")),
                            _ => None,
                        };
                        if let Some(enum_type) = matched_enum_type {
                            let method = Ident::new(
                                &snake_to_camel(&func.sig.ident.to_string()),
                                Span::mixed_site(),
                            );
                            arms.push(parse_quote! {
                                #name::#method { #ident, .. } => RequestIdentifier::#enum_type(#ident.clone())
                            });
                        }
                    }
                }
            }
        }
    }
    input.extend(TokenStream::from(quote! {
        impl RequestIdentification for tarpc::Request<#name> {
            fn extract_identifier(&self) -> RequestIdentifier {
                match &self.message {
                    #(
                        #arms,
                    )*
                    _ => RequestIdentifier::None,
                }
            }
        }
    }));
    input
}

struct EnvOrDefault {
    name: syn::LitStr,
    default: syn::Expr,
}

impl Parse for EnvOrDefault {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: syn::LitStr = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let default = input.parse()?;
        Ok(Self { name, default })
    }
}

#[proc_macro]
pub fn env_or_default(input: TokenStream) -> TokenStream {
    let env = parse_macro_input!(input as EnvOrDefault);
    let default = env.default;

    TokenStream::from(match std::env::var(env.name.value()) {
        Ok(var) => quote! { #var },
        Err(_) => quote! { #default },
    })
}
