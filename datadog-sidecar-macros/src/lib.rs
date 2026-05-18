// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
};

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
