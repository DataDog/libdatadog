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

use crate::fetch::{ConfigInvariants, ConfigOptions, ConfigProductCapabilities};
use crate::targets::{TargetData, TargetsCustom, TargetsData, TargetsList};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use base64::Engine;
use http::Response;
use http_body_util::BodyExt;
use hyper::service::service_fn;
use libdd_common::{Endpoint, http_common};
use libdd_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ConfigStatus, File,
};
use serde_json::value::{RawValue, to_raw_value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;

pub struct RemoteConfigServer {
    pub last_request: Mutex<Option<ClientGetConfigsRequest>>,
    #[allow(clippy::type_complexity)]
    pub files: Mutex<HashMap<RemoteConfigPath, (Vec<Arc<Target>>, u64, String)>>,
    pub next_response: Mutex<Option<Response<http_common::Body>>>,
    /// The `config_status` reported in the response; set to
    /// [`ConfigStatus::Expired`] to simulate an agent serving from a stale
    /// cache whose TUF signatures have expired.
    pub config_status: Mutex<ConfigStatus>,
    /// The body of this path is served corrupted (not matching its hash) in the next response
    /// carrying it.
    pub corrupt_next_body: Mutex<Option<RemoteConfigPath>>,
    /// Paths listed in `client_configs` in addition to the applicable files, without any
    /// target metadata nor body.
    pub extra_client_configs: Mutex<Vec<String>>,
    /// See [`Self::hold_next_response`].
    #[allow(clippy::type_complexity)]
    next_request_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    /// The current targets version and the fingerprint of the files it was computed for.
    targets_state: Mutex<(u64, Option<u64>)>,
    pub endpoint: Endpoint,
    #[allow(dead_code)] // stops receiver on drop
    shutdown_complete_tx: Sender<()>,
}

impl RemoteConfigServer {
    fn handle_request(
        &self,
        body_bytes: hyper::body::Bytes,
    ) -> Result<Response<http_common::Body>, Infallible> {
        let request: ClientGetConfigsRequest =
            serde_json::from_str(core::str::from_utf8(&body_bytes).unwrap()).unwrap();
        let response = if let Some(response) = self.next_response.lock().unwrap().take() {
            response
        } else {
            self.agent_response(&request)
        };
        *self.last_request.lock().unwrap() = Some(request);
        eprintln!("reponse finished");
        Ok(response)
    }

    /// Answers like the agent's `ClientGetConfigs`: the client gets nothing unless its
    /// `targets_version` differs from the current one, and then gets the full targets
    /// metadata, the configs that apply to it, and the bodies of those it hasn't cached.
    fn agent_response(&self, request: &ClientGetConfigsRequest) -> Response<http_common::Body> {
        let files = self.files.lock().unwrap();
        let targets_version = self.targets_version(&files);
        let client = request.client.as_ref().unwrap();
        let tracer = client.client_tracer.as_ref().unwrap();

        let target_info: HashMap<String, (String, Box<RawValue>, &str)> = files
            .iter()
            .map(|(p, (_, v, file))| {
                (
                    p.to_string(),
                    (
                        format!("{:x}", Sha256::digest(file)),
                        to_raw_value(v).unwrap(),
                        file.as_str(),
                    ),
                )
            })
            .collect();
        let targets = TargetsList {
            signed: TargetsData {
                custom: TargetsCustom {
                    agent_refresh_interval: Some(1000),
                    opaque_backend_state: "some state",
                },
                targets: target_info
                    .iter()
                    .map(|(p, (hash, version, file))| {
                        (
                            p.as_str(),
                            TargetData {
                                custom: HashMap::from([("v", &**version)]),
                                hashes: HashMap::from([("sha256", hash.as_str())]),
                                length: file.len() as u32,
                            },
                        )
                    })
                    .collect(),
                version: targets_version as i64,
            },
        };
        let targets = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&targets).unwrap())
            .into_bytes();

        let config_status = *self.config_status.lock().unwrap();
        if config_status == ConfigStatus::Expired {
            // The agent flushes all clients, regardless of their targets_version.
            let response = ClientGetConfigsResponse {
                roots: vec![],
                targets,
                target_files: vec![],
                client_configs: vec![],
                config_status: config_status as i32,
            };
            return Response::new(http_common::Body::from(
                serde_json::to_vec(&response).unwrap(),
            ));
        }

        if client.state.as_ref().unwrap().targets_version == targets_version {
            return Response::new(http_common::Body::from("{}"));
        }

        let mut client_configs: Vec<String> = files
            .iter()
            .filter(|(p, (targets, _, _))| {
                client.products.contains(&p.product().to_string())
                    && targets.iter().any(|t| {
                        t.service == tracer.service
                            && t.env == tracer.env
                            && t.app_version == tracer.app_version
                    })
            })
            .map(|(p, _)| p.to_string())
            .collect();
        let mut corrupt_next_body = self.corrupt_next_body.lock().unwrap();
        let target_files = client_configs
            .iter()
            .filter(|p| {
                let (hash, _, file) = &target_info[p.as_str()];
                !request.cached_target_files.iter().any(|cached| {
                    cached.path == **p
                        && cached.length == file.len() as i64
                        && !cached.hashes.is_empty()
                        && cached
                            .hashes
                            .iter()
                            .all(|h| h.algorithm == "sha256" && h.hash == *hash)
                })
            })
            .map(|p| {
                let mut body = target_info[p.as_str()].2.to_string();
                if corrupt_next_body
                    .as_ref()
                    .is_some_and(|c| c.to_string() == *p)
                {
                    *corrupt_next_body = None;
                    body.push_str("-corrupted");
                }
                File {
                    path: p.clone(),
                    raw: base64::engine::general_purpose::STANDARD
                        .encode(body)
                        .into_bytes(),
                }
            })
            .collect();
        client_configs.extend(self.extra_client_configs.lock().unwrap().iter().cloned());
        let response = ClientGetConfigsResponse {
            roots: vec![], /* not checked */
            targets,
            target_files,
            client_configs,
            config_status: config_status as i32,
        };
        Response::new(http_common::Body::from(
            serde_json::to_vec(&response).unwrap(),
        ))
    }

    /// The agent's director targets version: bumped whenever the files differ from those seen
    /// by the previous request, so tests can keep mutating `files` directly.
    fn targets_version(
        &self,
        files: &HashMap<RemoteConfigPath, (Vec<Arc<Target>>, u64, String)>,
    ) -> u64 {
        let mut snapshot: Vec<_> = files
            .iter()
            .map(|(p, (targets, v, file))| (p.to_string(), targets, v, file))
            .collect();
        snapshot.sort();
        let mut hasher = DefaultHasher::new();
        snapshot.hash(&mut hasher);
        let fingerprint = hasher.finish();

        let mut state = self.targets_state.lock().unwrap();
        if state.1 != Some(fingerprint) {
            *state = (state.0 + 1, Some(fingerprint));
        }
        state.0
    }

    pub fn spawn() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown_complete_tx, mut shutdown_complete_rx) = tokio::sync::mpsc::channel::<()>(1);
        let server = Arc::new(RemoteConfigServer {
            last_request: Mutex::new(None),
            files: Default::default(),
            next_response: Mutex::new(None),
            config_status: Mutex::new(ConfigStatus::Ok),
            corrupt_next_body: Mutex::new(None),
            extra_client_configs: Default::default(),
            next_request_gate: Default::default(),
            targets_state: Mutex::new((0, None)),
            endpoint: Endpoint::from_slice(&format!("http://127.0.0.1:{port}/")),
            shutdown_complete_tx,
        });
        let this = server.clone();
        let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let this = this.clone();
            async move {
                let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                let gate = this.next_request_gate.lock().unwrap().take();
                if let Some((received, release)) = gate {
                    received.send(()).ok();
                    release.await.ok();
                }
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

    /// Holds the response to the next request, keeping it in flight: the first returned value
    /// completes once that request arrived, and the response is computed, from the files as they
    /// are by then, once the second is used or dropped.
    pub fn hold_next_response(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (received_tx, received_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.next_request_gate.lock().unwrap() = Some((received_tx, release_rx));
        (received_rx, release_tx)
    }

    pub fn dummy_options(&self) -> ConfigOptions {
        ConfigOptions {
            invariants: ConfigInvariants {
                language: "php".to_string(),
                tracer_version: "1.2.3".to_string(),
                endpoint: self.endpoint.clone(),
                agentless: None,
            },
            products: vec![
                RemoteConfigProduct::ApmTracing,
                RemoteConfigProduct::LiveDebugging,
            ],
            capabilities: vec![RemoteConfigCapabilities::ApmTracingCustomTags],
        }
    }

    /// The (version, apply_state, apply_error) of each config state the last request reported,
    /// sorted by version.
    pub fn reported_config_states(&self) -> Vec<(u64, u64, String)> {
        let request = self.last_request.lock().unwrap();
        let state = request
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap();
        let mut states: Vec<_> = state
            .config_states
            .iter()
            .map(|s| (s.version, s.apply_state, s.apply_error.clone()))
            .collect();
        states.sort();
        states
    }

    pub fn dummy_product_capabilities(&self) -> ConfigProductCapabilities {
        ConfigProductCapabilities::new(
            self.dummy_options().products,
            self.dummy_options().capabilities,
        )
    }
}
