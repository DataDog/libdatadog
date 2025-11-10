// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// TODO: This module should probably be gated behind a test-only feature flag

// This module should only ever be used in test code. Relaxing the crate level clippy lints to warn
// when panic macros are used.
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::todo)]
#![allow(clippy::unimplemented)]

use crate::fetch::ConfigInvariants;
use crate::targets::{TargetData, TargetsCustom, TargetsData, TargetsList};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use base64::Engine;
use http::Response;
use http_body_util::BodyExt;
use hyper::service::service_fn;
use libdd_common::{hyper_migration, Endpoint};
use libdd_trace_protobuf::remoteconfig::{ClientGetConfigsRequest, ClientGetConfigsResponse, File};
use serde_json::value::to_raw_value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tokio::select;
use tokio::sync::mpsc::Sender;

pub struct RemoteConfigServer {
    pub last_request: Mutex<Option<ClientGetConfigsRequest>>,
    #[allow(clippy::type_complexity)]
    pub files: Mutex<HashMap<RemoteConfigPath, (Vec<Arc<Target>>, u64, String)>>,
    pub next_response: Mutex<Option<Response<hyper_migration::Body>>>,
    pub endpoint: Endpoint,
    #[allow(dead_code)] // stops receiver on drop
    shutdown_complete_tx: Sender<()>,
}

impl RemoteConfigServer {
    fn handle_request(
        &self,
        body_bytes: hyper::body::Bytes,
    ) -> Result<Response<hyper_migration::Body>, Infallible> {
        let request: ClientGetConfigsRequest =
            serde_json::from_str(core::str::from_utf8(&body_bytes).unwrap()).unwrap();
        let response = if let Some(response) = self.next_response.lock().unwrap().take() {
            response
        } else {
            let known: HashMap<_, _> = request
                .cached_target_files
                .iter()
                .map(|m| (m.path.clone(), m.hashes[0].hash.clone()))
                .collect();
            let files = self.files.lock().unwrap();
            let applied_files: HashMap<_, _> = files
                .iter()
                .filter(|(_, (targets, _, _))| {
                    let tracer = request
                        .client
                        .as_ref()
                        .unwrap()
                        .client_tracer
                        .as_ref()
                        .unwrap();
                    targets.iter().any(|t| {
                        t.service == tracer.service
                            && t.env == tracer.env
                            && t.app_version == tracer.app_version
                    })
                })
                .collect();
            let states = &request
                .client
                .as_ref()
                .unwrap()
                .state
                .as_ref()
                .unwrap()
                .config_states;
            if applied_files.len() == states.len()
                && states.iter().all(|s| {
                    for (p, (_, v, _)) in applied_files.iter() {
                        if p.product.to_string() == s.product
                            && p.config_id == s.id
                            && *v == s.version
                        {
                            return true;
                        }
                    }
                    false
                })
            {
                Response::new(hyper_migration::Body::from("{}"))
            } else {
                let target_info: Vec<_> = applied_files
                    .iter()
                    .map(|(p, (_, v, file))| {
                        (
                            p.to_string(),
                            format!("{:x}", Sha256::digest(file)),
                            to_raw_value(v).unwrap(),
                            file.clone(),
                        )
                    })
                    .filter(|(p, hash, _, _)| {
                        if let Some(existing) = known.get(p) {
                            existing != hash
                        } else {
                            true
                        }
                    })
                    .collect();
                let targets = TargetsList {
                    signatures: vec![],
                    signed: TargetsData {
                        _type: "",
                        custom: TargetsCustom {
                            agent_refresh_interval: Some(1000),
                            opaque_backend_state: "some state",
                        },
                        expires: OffsetDateTime::from_unix_timestamp(253402300799).unwrap(),
                        spec_version: "1.0.0",
                        targets: target_info
                            .iter()
                            .map(|(p, hash, version, _)| {
                                (
                                    p.as_str(),
                                    TargetData {
                                        custom: HashMap::from([("v", &**version)]),
                                        hashes: HashMap::from([("sha256", hash.as_str())]),
                                        length: 0,
                                    },
                                )
                            })
                            .collect(),
                        version: 1,
                    },
                };
                let response = ClientGetConfigsResponse {
                    roots: vec![], /* not checked */
                    targets: base64::engine::general_purpose::STANDARD
                        .encode(serde_json::to_vec(&targets).unwrap())
                        .into_bytes(),
                    target_files: target_info
                        .iter()
                        .map(|(p, _, _, file)| File {
                            path: p.to_string(),
                            raw: base64::engine::general_purpose::STANDARD
                                .encode(file)
                                .into_bytes(),
                        })
                        .collect(),
                    client_configs: applied_files.keys().map(|k| k.to_string()).collect(),
                };
                Response::new(hyper_migration::Body::from(
                    serde_json::to_vec(&response).unwrap(),
                ))
            }
        };
        *self.last_request.lock().unwrap() = Some(request);
        eprintln!("reponse finished");
        Ok(response)
    }

    pub fn spawn() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown_complete_tx, mut shutdown_complete_rx) = tokio::sync::mpsc::channel::<()>(1);
        let server = Arc::new(RemoteConfigServer {
            last_request: Mutex::new(None),
            files: Default::default(),
            next_response: Mutex::new(None),
            endpoint: Endpoint::from_slice(&format!("http://127.0.0.1:{port}/")),
            shutdown_complete_tx,
        });
        let this = server.clone();
        let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let this = this.clone();
            async move {
                let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                this.handle_request(body_bytes)
            }
        });
        tokio::spawn(async move {
            listener.set_nonblocking(true).unwrap();
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                // let (stream, _) = listener.accept().await.unwrap();
                let stream = select! {
                    _ = shutdown_complete_rx.recv() => break,
                    res = listener.accept()
                    => {res.unwrap().0}
                };
                let service = service.clone();
                eprintln!("new conn");
                tokio::spawn(async move {
                    eprintln!("new conn inside");
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::tokio::TokioIo::new(stream), service)
                        .await
                        .unwrap();
                });
            }
        });
        server
    }

    pub fn dummy_invariants(&self) -> ConfigInvariants {
        ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "1.2.3".to_string(),
            endpoint: self.endpoint.clone(),
            #[cfg(not(feature = "live-debugger"))]
            products: vec![RemoteConfigProduct::ApmTracing],
            #[cfg(feature = "live-debugger")]
            products: vec![
                RemoteConfigProduct::ApmTracing,
                RemoteConfigProduct::LiveDebugger,
            ],
            capabilities: vec![RemoteConfigCapabilities::ApmTracingCustomTags],
        }
    }
}
