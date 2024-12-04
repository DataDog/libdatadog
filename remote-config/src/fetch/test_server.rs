// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::ConfigInvariants;
use crate::targets::{TargetData, TargetsCustom, TargetsData, TargetsList};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use base64::Engine;
use datadog_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, File,
};
use ddcommon_net1::Endpoint;
use http::{Request, Response};
use hyper::body::HttpBody;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Server};
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
    pub next_response: Mutex<Option<Response<Body>>>,
    pub endpoint: Endpoint,
    #[allow(dead_code)] // stops receiver on drop
    shutdown_complete_tx: Sender<()>,
}

impl RemoteConfigServer {
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
        tokio::spawn(async move {
            let service = make_service_fn(|_conn| {
                let this = this.clone();
                async move {
                    Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                        let this = this.clone();
                        async move {
                            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                            let request: ClientGetConfigsRequest =
                                serde_json::from_str(core::str::from_utf8(&body_bytes).unwrap())
                                    .unwrap();
                            let response =
                                if let Some(response) = this.next_response.lock().unwrap().take() {
                                    response
                                } else {
                                    let known: HashMap<_, _> = request
                                        .cached_target_files
                                        .iter()
                                        .map(|m| (m.path.clone(), m.hashes[0].hash.clone()))
                                        .collect();
                                    let files = this.files.lock().unwrap();
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
                                        Response::new(Body::from("{}"))
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
                                                expires: OffsetDateTime::from_unix_timestamp(
                                                    253402300799,
                                                )
                                                .unwrap(),
                                                spec_version: "1.0.0",
                                                targets: target_info
                                                    .iter()
                                                    .map(|(p, hash, version, _)| {
                                                        (
                                                            p.as_str(),
                                                            TargetData {
                                                                custom: HashMap::from([(
                                                                    "v", &**version,
                                                                )]),
                                                                hashes: HashMap::from([(
                                                                    "sha256",
                                                                    hash.as_str(),
                                                                )]),
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
                                            client_configs: applied_files
                                                .keys()
                                                .map(|k| k.to_string())
                                                .collect(),
                                        };
                                        Response::new(Body::from(
                                            serde_json::to_vec(&response).unwrap(),
                                        ))
                                    }
                                };
                            *this.last_request.lock().unwrap() = Some(request);
                            Ok::<_, Infallible>(response)
                        }
                    }))
                }
            });
            let server = Server::from_tcp(listener).unwrap().serve(service);

            select! {
                server_result = server => {
                    if let Err(e) = server_result {
                        eprintln!("server connection error: {}", e);
                    }
                },
                _ = shutdown_complete_rx.recv() => {},
            }
        });
        server
    }

    pub fn dummy_invariants(&self) -> ConfigInvariants {
        ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "1.2.3".to_string(),
            endpoint: self.endpoint.clone(),
            products: vec![
                RemoteConfigProduct::ApmTracing,
                RemoteConfigProduct::LiveDebugger,
            ],
            capabilities: vec![RemoteConfigCapabilities::ApmTracingCustomTags],
        }
    }
}
