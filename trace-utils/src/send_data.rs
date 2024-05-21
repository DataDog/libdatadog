// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Context};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::{Body, Client, Method, Response, StatusCode};
use std::collections::HashMap;

use crate::tracer_header_tags::TracerHeaderTags;
use datadog_trace_protobuf::pb;
use ddcommon::{connector, Endpoint, HttpRequestBuilder};

#[derive(Debug)]
pub enum SendRequestError {
    Hyper(hyper::Error),
    Any(anyhow::Error),
}

pub struct SendDataResult {
    pub last_result: anyhow::Result<Response<Body>>,
    pub requests_count: u64,
    pub responses_count_per_code: HashMap<u16, u64>,
    pub errors_timeout: u64,
    pub errors_network: u64,
    pub errors_status_code: u64,
}

impl SendDataResult {
    fn new() -> SendDataResult {
        SendDataResult {
            last_result: Err(anyhow!("No requests sent")),
            requests_count: 0,
            responses_count_per_code: Default::default(),
            errors_timeout: 0,
            errors_network: 0,
            errors_status_code: 0,
        }
    }

    async fn update(
        &mut self,
        res: Result<Response<Body>, SendRequestError>,
        expected_status: StatusCode,
    ) {
        self.requests_count += 1;
        match res {
            Ok(response) => {
                *self
                    .responses_count_per_code
                    .entry(response.status().as_u16())
                    .or_default() += 1;
                self.last_result = if response.status() == expected_status {
                    Ok(response)
                } else {
                    self.errors_status_code += 1;

                    let body_bytes = hyper::body::to_bytes(response.into_body()).await;
                    let response_body = String::from_utf8(body_bytes.unwrap_or_default().to_vec())
                        .unwrap_or_default();
                    Err(anyhow::format_err!(
                        "Server did not accept traces: {response_body}"
                    ))
                }
            }
            Err(e) => match e {
                SendRequestError::Hyper(e) => {
                    if e.is_timeout() {
                        self.errors_timeout += 1;
                    } else {
                        self.errors_network += 1;
                    }
                    self.last_result = Err(anyhow!(e));
                }
                SendRequestError::Any(e) => {
                    self.last_result = Err(e);
                }
            },
        }
    }

    fn error(mut self, err: anyhow::Error) -> SendDataResult {
        self.last_result = Err(err);
        self
    }
}

fn construct_agent_payload(tracer_payloads: Vec<pb::TracerPayload>) -> pb::AgentPayload {
    pb::AgentPayload {
        host_name: "".to_string(),
        env: "".to_string(),
        agent_version: "".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
        rare_sampler_enabled: false,
    }
}

fn serialize_proto_payload<T>(payload: &T) -> anyhow::Result<Vec<u8>>
where
    T: prost::Message,
{
    let mut buf = Vec::with_capacity(payload.encoded_len());
    payload.encode(&mut buf)?;
    Ok(buf)
}

#[derive(Debug, Clone)]
pub struct SendData {
    pub tracer_payloads: Vec<pb::TracerPayload>,
    pub size: usize, // have a rough size estimate to force flushing if it's large
    pub target: Endpoint,
    headers: HashMap<&'static str, String>,
}

impl SendData {
    pub fn new(
        size: usize,
        tracer_payload: pb::TracerPayload,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendData {
        let headers = if let Some(api_key) = &target.api_key {
            HashMap::from([("DD-API-KEY", api_key.as_ref().to_string())])
        } else {
            tracer_header_tags.into()
        };

        SendData {
            tracer_payloads: vec![tracer_payload],
            size,
            target: target.clone(),
            headers,
        }
    }
    pub async fn send<'a>(self) -> SendDataResult {
        let target = &self.target;

        let req = self.create_request_builder();

        if self.use_protobuf() {
            self.send_with_protobuf(req).await
        } else {
            self.send_with_msgpack(req).await
        }
    }
    
    fn use_protobuf(&self) -> bool {
        self.target.api_key.is_some()
    }

    fn create_request_builder(&self) -> HttpRequestBuilder {
        let mut req = hyper::Request::builder()
            .uri(self.target.url.clone())
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST);

        for (key, value) in &self.headers {
            req = req.header(*key, value);
        }

        req
    }

    async fn send_request(
        &self,
        req: HttpRequestBuilder,
        payload: Vec<u8>,
    ) -> Result<Response<Body>, SendRequestError> {
        let req = req
            .body(Body::from(payload))
            .map_err(|e| SendRequestError::Any(anyhow!(e)))?;

        Client::builder()
            .build(connector::Connector::default())
            .request(req)
            .await
            .map_err(SendRequestError::Hyper)
    }

    async fn send_with_protobuf(&self, mut req: HttpRequestBuilder) -> SendDataResult {
        let mut result = SendDataResult::new();

        req = req.header("Content-type", "application/x-protobuf");

        let agent_payload = construct_agent_payload(self.tracer_payloads.clone());
        let serialized_trace_payload = match serialize_proto_payload(&agent_payload)
            .context("Failed to serialize trace agent payload, dropping traces")
        {
            Ok(p) => p,
            Err(e) => return result.error(e),
        };

        result
            .update(
                self.send_request(req, serialized_trace_payload).await,
                StatusCode::ACCEPTED,
            )
            .await;

        result
    }

    async fn send_with_msgpack(&self, mut req: HttpRequestBuilder) -> SendDataResult {
        let mut result = SendDataResult::new();

        req = req.header("Content-type", "application/msgpack");

        let (template, _) = req.body(()).unwrap().into_parts();

        let mut futures = FuturesUnordered::new();
        for tracer_payload in self.tracer_payloads.clone().into_iter() {
            let mut builder = HttpRequestBuilder::new()
                .method(template.method.clone())
                .uri(template.uri.clone())
                .version(template.version)
                .header(
                    "X-Datadog-Trace-Count",
                    tracer_payload.chunks.len().to_string(),
                );
            builder
                .headers_mut()
                .unwrap()
                .extend(template.headers.clone());

            let payload = match rmp_serde::to_vec_named(&tracer_payload) {
                Ok(p) => p,
                Err(e) => return result.error(anyhow!(e)),
            };

            futures.push(self.send_request(builder, payload));
        }
        loop {
            match futures.next().await {
                Some(response) => {
                    result.update(response, StatusCode::OK).await;
                    if result.last_result.is_err() {
                        return result;
                    }
                }
                None => return result,
            }
        }
    }
}
