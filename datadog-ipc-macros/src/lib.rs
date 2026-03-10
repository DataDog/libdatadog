// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use heck::{ToPascalCase, ToSnakeCase};
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote, ToTokens};
use syn::{FnArg, Ident, ItemTrait, ReturnType, TraitItem, Type};

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

fn is_unit_type(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.meta.path().to_token_stream().to_string() == name)
}

// ---------------------------------------------------------------------------
// Old macro — kept during migration to the new #[service] macro.
// ---------------------------------------------------------------------------

#[proc_macro_attribute]
pub fn impl_transfer_handles(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let mut item: ItemTrait = syn::parse(input).unwrap();
    let req_name = format_ident!("{}Request", item.ident);
    let res_name = format_ident!("{}Response", item.ident);
    let mut arms_req_move: Vec<syn::Arm> = vec![];
    let mut arms_req_recv: Vec<syn::Arm> = vec![];
    let mut arms_res_move: Vec<syn::Arm> = vec![];
    let mut arms_res_recv: Vec<syn::Arm> = vec![];
    for inner in item.items.iter_mut() {
        if let TraitItem::Fn(ref mut func) = inner {
            let mut params: Vec<syn::FieldPat> = vec![];
            let mut stmts_move: Vec<syn::Stmt> = vec![];
            let mut stmts_recv: Vec<syn::Stmt> = vec![];
            for any_arg in func.sig.inputs.iter_mut() {
                if let FnArg::Typed(ref mut arg) = any_arg {
                    let orig_attr_num = arg.attrs.len();
                    arg.attrs.retain(|attr| {
                        attr.meta.path().to_token_stream().to_string() != "SerializedHandle"
                    });
                    if orig_attr_num != arg.attrs.len() {
                        if let syn::Pat::Ident(ref ident) = *arg.pat {
                            params.push(syn::FieldPat {
                                attrs: vec![],
                                member: syn::Member::Named(ident.ident.clone()),
                                colon_token: None,
                                pat: Box::new(syn::parse_quote! { #ident }),
                            });
                            stmts_move.push(
                                syn::parse_quote! { __transport.copy_handle(#ident.clone().into())?; },
                            );
                            stmts_recv
                                .push(syn::parse_quote! { #ident.receive_handles(__transport)?; });
                        }
                    }
                }
            }
            let method = Ident::new(
                &snake_to_camel(&func.sig.ident.to_string()),
                Span::mixed_site(),
            );
            if !params.is_empty() {
                arms_req_move.push(syn::parse_quote! {
                    #req_name::#method { #(#params,)* .. } => {
                        #(#stmts_move)*
                        Ok(())
                    }
                });
                arms_req_recv.push(syn::parse_quote! {
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
                arms_res_move.push(syn::parse_quote! {
                    #res_name::#method(response) => response.copy_handles(transport)
                });
                arms_res_recv.push(syn::parse_quote! {
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
                    #(#arms_req_move,)*
                    _ => Ok(()),
                }
            }

            fn receive_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &mut self,
                __transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(#arms_req_recv,)*
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
                    #(#arms_res_move,)*
                    _ => Ok(()),
                }
            }

            fn receive_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &mut self,
                transport: Transport,
            ) -> Result<(), Transport::Error> {
                match self {
                    #(#arms_res_recv,)*
                    _ => Ok(()),
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// New #[service] macro
// ---------------------------------------------------------------------------

// Each param stores: (non-SerializedHandle attrs, name, type, is_handle).
// The attrs include #[cfg(...)], allowing conditional compilation of parameters.
type ParamInfo = (Vec<syn::Attribute>, Ident, Box<Type>);

struct MethodInfo {
    name: Ident,
    variant: Ident,
    discriminant: u32,
    is_blocking: bool,
    return_type: Option<Box<Type>>,
    params: Vec<ParamInfo>,
    handle_param_indices: Vec<usize>,
}

fn collect_methods(item: &ItemTrait) -> Vec<MethodInfo> {
    let mut methods = Vec::new();
    let mut discriminant: u32 = 0;

    for trait_item in &item.items {
        let TraitItem::Fn(func) = trait_item else {
            continue;
        };

        let name = func.sig.ident.clone();
        let variant = Ident::new(&name.to_string().to_pascal_case(), Span::mixed_site());
        let is_blocking = has_attr(&func.attrs, "blocking");

        let return_type = match &func.sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => {
                if is_unit_type(ty) {
                    None
                } else {
                    Some(ty.clone())
                }
            }
        };

        let mut params: Vec<ParamInfo> = Vec::new();
        let mut handle_param_indices: Vec<usize> = Vec::new();

        for arg in &func.sig.inputs {
            let FnArg::Typed(pat_ty) = arg else {
                continue;
            };
            let syn::Pat::Ident(ident_pat) = &*pat_ty.pat else {
                continue;
            };
            if has_attr(&pat_ty.attrs, "SerializedHandle") {
                handle_param_indices.push(params.len());
            }
            // Keep all attrs except #[SerializedHandle] (e.g. #[cfg(...)]).
            let pass_through_attrs: Vec<syn::Attribute> = pat_ty
                .attrs
                .iter()
                .filter(|a| a.meta.path().to_token_stream().to_string() != "SerializedHandle")
                .cloned()
                .collect();
            params.push((pass_through_attrs, ident_pat.ident.clone(), pat_ty.ty.clone()));
        }

        methods.push(MethodInfo {
            name,
            variant,
            discriminant,
            is_blocking,
            return_type,
            params,
            handle_param_indices,
        });
        discriminant += 1;
    }

    methods
}

fn gen_request_enum(enum_name: &Ident, methods: &[MethodInfo]) -> proc_macro2::TokenStream {
    let variants: Vec<_> = methods
        .iter()
        .map(|m| {
            let variant = &m.variant;
            let fields: Vec<_> = m
                .params
                .iter()
                .map(|(attrs, n, t)| quote! { #(#attrs)* #n: #t })
                .collect();
            quote! { #variant { #(#fields),* } }
        })
        .collect();

    let disc_arms: Vec<_> = methods
        .iter()
        .map(|m| {
            let variant = &m.variant;
            let d = m.discriminant;
            quote! { Self::#variant { .. } => #d }
        })
        .collect();

    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize)]
        pub enum #enum_name {
            #(#variants),*
        }

        impl #enum_name {
            pub fn discriminant(&self) -> u32 {
                match self {
                    #(#disc_arms),*
                }
            }
        }
    }
}

fn gen_transfer_handles(enum_name: &Ident, methods: &[MethodInfo]) -> proc_macro2::TokenStream {
    let copy_arms: Vec<_> = methods
        .iter()
        .filter(|m| !m.handle_param_indices.is_empty())
        .map(|m| {
            let variant = &m.variant;
            let handle_names: Vec<_> = m
                .handle_param_indices
                .iter()
                .map(|&i| &m.params[i].1)
                .collect();
            // One copy_handle call per #[SerializedHandle] param.
            // Uses .into() to convert from the param type to PlatformHandle<OwnedFileHandle>.
            let stmts: Vec<_> = handle_names
                .iter()
                .map(|hn| quote! { __transport.copy_handle(#hn.clone().into())?; })
                .collect();
            quote! {
                #enum_name::#variant { #(#handle_names,)* .. } => {
                    #(#stmts)*
                    Ok(())
                }
            }
        })
        .collect();

    let recv_arms: Vec<_> = methods
        .iter()
        .filter(|m| !m.handle_param_indices.is_empty())
        .map(|m| {
            let variant = &m.variant;
            let handle_names: Vec<_> = m
                .handle_param_indices
                .iter()
                .map(|&i| &m.params[i].1)
                .collect();
            let stmts: Vec<_> = handle_names
                .iter()
                .map(|hn| quote! { #hn.receive_handles(__transport)?; })
                .collect();
            quote! {
                #enum_name::#variant { #(#handle_names,)* .. } => {
                    #(#stmts)*
                    Ok(())
                }
            }
        })
        .collect();

    quote! {
        impl datadog_ipc::handles::TransferHandles for #enum_name {
            fn copy_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &self,
                __transport: Transport,
            ) -> ::std::result::Result<(), Transport::Error> {
                match self {
                    #(#copy_arms,)*
                    _ => Ok(()),
                }
            }

            fn receive_handles<Transport: datadog_ipc::handles::HandlesTransport>(
                &mut self,
                __transport: Transport,
            ) -> ::std::result::Result<(), Transport::Error> {
                match self {
                    #(#recv_arms,)*
                    _ => Ok(()),
                }
            }
        }
    }
}

fn gen_handler_trait(
    trait_name: &Ident,
    vis: &syn::Visibility,
    methods: &[MethodInfo],
) -> proc_macro2::TokenStream {
    let handler_methods: Vec<_> = methods
        .iter()
        .map(|m| {
            let name = &m.name;
            let params: Vec<_> = m
                .params
                .iter()
                .map(|(attrs, n, t)| quote! { #(#attrs)* #n: #t })
                .collect();
            let ret = match &m.return_type {
                None => quote! { () },
                Some(ty) => quote! { #ty },
            };
            quote! {
                fn #name(
                    &self,
                    peer: datadog_ipc::PeerCredentials,
                    #(#params),*
                ) -> impl ::std::future::Future<Output = #ret> + Send + '_;
            }
        })
        .collect();

    quote! {
        #vis trait #trait_name: Send + Sync + 'static {
            #(#handler_methods)*
        }
    }
}

fn gen_serve_fn(
    trait_name: &Ident,
    enum_name: &Ident,
    methods: &[MethodInfo],
) -> proc_macro2::TokenStream {
    let snake = trait_name.to_string().to_snake_case();
    let serve_fn = format_ident!("serve_{}_connection", snake);

    let match_arms: Vec<_> = methods
        .iter()
        .map(|m| {
            let variant = &m.variant;
            let name = &m.name;
            // field_names: includes leading #[cfg(...)] attrs for conditional params.
            let field_names: Vec<_> = m
                .params
                .iter()
                .map(|(attrs, n, _)| quote! { #(#attrs)* #n })
                .collect();

            let response_code = if m.return_type.is_some() {
                quote! {
                    let result = handler.#name(peer, #(#field_names),*).await;
                    let __resp_data = datadog_ipc::codec::encode_response(&result);
                    datadog_ipc::send_raw_async(&async_fd, &__resp_data).await.ok();
                }
            } else {
                quote! {
                    handler.#name(peer, #(#field_names),*).await;
                    // 1-byte ack: distinguishable from EOF (0 bytes from recvmsg on closed socket).
                    datadog_ipc::send_raw_async(&async_fd, &[0u8]).await.ok();
                }
            };

            quote! {
                #enum_name::#variant { #(#field_names),* } => {
                    #response_code
                }
            }
        })
        .collect();

    quote! {
        pub async fn #serve_fn<H: #trait_name>(
            conn: datadog_ipc::SeqpacketConn,
            handler: ::std::sync::Arc<H>,
        ) {
            let peer = conn.peer_credentials().unwrap_or_default();
            let async_fd = match conn.into_async_conn() {
                Ok(fd) => fd,
                Err(e) => {
                    ::tracing::error!("IPC serve: into_async_conn failed: {e}");
                    return;
                }
            };
            let mut recv_counter: u64 = 0;
            let mut buf = vec![0u8; datadog_ipc::MAX_MESSAGE_SIZE + datadog_ipc::HANDLE_SUFFIX_SIZE];
            loop {
                let (n, fds) = match datadog_ipc::recv_raw_async(&async_fd, &mut buf).await {
                    Ok(x) => x,
                    Err(e) => {
                        ::tracing::trace!("IPC serve: recv (connection closed?): {e}");
                        break;
                    }
                };
                let Ok((discriminant, mut req)) =
                    datadog_ipc::codec::decode::<#enum_name>(&buf[..n])
                else {
                    ::tracing::warn!("IPC serve: failed to decode request");
                    break;
                };
                let mut __source = datadog_ipc::handles::FdSource::new(fds);
                if datadog_ipc::handles::TransferHandles::receive_handles(
                    &mut req,
                    &mut __source,
                ).is_err() {
                    ::tracing::warn!("IPC serve: failed to receive handles");
                    break;
                }
                recv_counter += 1;
                ::tracing::trace!(recv_counter, discriminant, pid = peer.pid, "IPC recv");

                match req {
                    #(#match_arms)*
                }
            }
        }
    }
}

fn gen_channel(
    trait_name: &Ident,
    vis: &syn::Visibility,
    enum_name: &Ident,
    methods: &[MethodInfo],
) -> proc_macro2::TokenStream {
    let channel_name = format_ident!("{}Channel", trait_name);

    let channel_methods: Vec<_> = methods
        .iter()
        .map(|m| {
            let name = &m.name;
            let params: Vec<_> = m
                .params
                .iter()
                .map(|(attrs, n, t)| quote! { #(#attrs)* #n: #t })
                .collect();
            // field_names includes leading attrs (e.g. #[cfg(windows)]) for struct init + call args.
            let field_names: Vec<_> = m
                .params
                .iter()
                .map(|(attrs, n, _)| quote! { #(#attrs)* #n })
                .collect();
            let d = m.discriminant;
            let variant = &m.variant;

            // Build the request and collect fds via TransferHandles.
            let build_req_and_fds = quote! {
                let __req = #enum_name::#variant { #(#field_names),* };
                let mut __sink = datadog_ipc::handles::FdSink::new();
                datadog_ipc::handles::TransferHandles::copy_handles(
                    &__req, &mut __sink
                ).ok();
                let mut __data = datadog_ipc::codec::encode(#d, &__req);
                let __fds = __sink.into_fds();
            };

            if m.return_type.is_none() && !m.is_blocking {
                let method_name = format_ident!("try_send_{}", name);
                quote! {
                    pub fn #method_name(&mut self, #(#params),*) -> bool {
                        #build_req_and_fds
                        self.0.try_send(&mut __data, &__fds)
                    }
                }
            } else if m.return_type.is_none() {
                let method_name = format_ident!("call_{}", name);
                quote! {
                    pub fn #method_name(&mut self, #(#params),*) -> ::std::io::Result<()> {
                        #build_req_and_fds
                        self.0.call(&mut __data, &__fds)?;
                        Ok(())
                    }
                }
            } else {
                let method_name = format_ident!("call_{}", name);
                let ret_ty = m.return_type.as_ref().unwrap();
                quote! {
                    pub fn #method_name(&mut self, #(#params),*) -> ::std::result::Result<#ret_ty, datadog_ipc::codec::DecodeError> {
                        #build_req_and_fds
                        let (__resp, _) = self.0.call(&mut __data, &__fds)
                            .map_err(datadog_ipc::codec::DecodeError::Io)?;
                        datadog_ipc::codec::decode_response::<#ret_ty>(&__resp)
                    }
                }
            }
        })
        .collect();

    quote! {
        #vis struct #channel_name(pub datadog_ipc::IpcClientConn);

        impl #channel_name {
            pub fn new(conn: datadog_ipc::SeqpacketConn) -> Self {
                Self(datadog_ipc::IpcClientConn::new(conn))
            }

            #(#channel_methods)*

            /// Generic fire-and-forget send (used by SidecarSender outbox drain).
            pub fn try_send_request(&mut self, req: &#enum_name) -> bool {
                let mut __sink = datadog_ipc::handles::FdSink::new();
                datadog_ipc::handles::TransferHandles::copy_handles(req, &mut __sink).ok();
                let mut __data = datadog_ipc::codec::encode(req.discriminant(), req);
                let __fds = __sink.into_fds();
                self.0.try_send(&mut __data, &__fds)
            }

            /// Generic blocking send (used by SidecarSender outbox drain).
            pub fn send_request_blocking(
                &mut self,
                req: &#enum_name,
            ) -> ::std::io::Result<()> {
                let mut __sink = datadog_ipc::handles::FdSink::new();
                datadog_ipc::handles::TransferHandles::copy_handles(req, &mut __sink).ok();
                let mut __data = datadog_ipc::codec::encode(req.discriminant(), req);
                let __fds = __sink.into_fds();
                self.0.send_blocking(&mut __data, &__fds)
            }
        }
    }
}

/// `#[service]` replaces `#[tarpc::service]` + `#[impl_transfer_handles]`.
///
/// Generates from a `trait` definition:
/// - `{Trait}Request` enum (Clone, Serialize, Deserialize, TransferHandles)
/// - Handler trait with RPIT async methods (no `async_trait`)
/// - `serve_{trait}_connection` async dispatch function (Unix)
/// - `{Trait}Channel` client struct with `try_send_*` / `call_*` methods (Unix)
///
/// Method attributes recognized (stripped before emission):
/// - `#[blocking]` — `-> ()` method where client waits for ack (vs fire-and-forget)
/// - `#[SerializedHandle]` on a parameter — the value carries an fd via SCM_RIGHTS
#[proc_macro_attribute]
pub fn service(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let item: ItemTrait = syn::parse(input).unwrap();

    let trait_name = item.ident.clone();
    let vis = item.vis.clone();
    let enum_name = format_ident!("{}Request", trait_name);

    let methods = collect_methods(&item);

    let enum_def = gen_request_enum(&enum_name, &methods);
    let transfer_handles = gen_transfer_handles(&enum_name, &methods);
    let handler_trait = gen_handler_trait(&trait_name, &vis, &methods);
    let serve_fn = gen_serve_fn(&trait_name, &enum_name, &methods);
    let channel = gen_channel(&trait_name, &vis, &enum_name, &methods);

    TokenStream::from(quote! {
        #enum_def
        #transfer_handles
        #handler_trait
        #serve_fn
        #channel
    })
}
