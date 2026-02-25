// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config::{HttpClientConfig, TransportConfig};
use crate::request::HttpMethod;
use crate::{HttpClientError, HttpRequest, HttpResponse};

use http_body_util::BodyExt;
use libdd_common::connector::Connector;
use libdd_common::http_common::{self, Body};

#[cfg(feature = "hyper-backend")]
pub(crate) struct HyperBackend {
    client: http_common::GenericHttpClient<Connector>,
    transport: TransportConfig,
}

impl std::fmt::Debug for HyperBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperBackend")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "hyper-backend")]
impl HyperBackend {
    pub(crate) fn new(
        _timeout: std::time::Duration,
        transport: TransportConfig,
    ) -> Result<Self, HttpClientError> {
        let client = http_common::client_builder().build(Connector::default());
        Ok(Self { client, transport })
    }

    /// Rewrite the request URL for UDS/Named Pipe transports.
    fn rewrite_url(&self, url: &str) -> Result<hyper::Uri, HttpClientError> {
        match &self.transport {
            TransportConfig::Tcp => url.parse().map_err(|e: hyper::http::uri::InvalidUri| {
                HttpClientError::InvalidConfig(e.to_string())
            }),
            #[cfg(unix)]
            TransportConfig::UnixSocket(path) => {
                build_transport_uri(url, libdd_common::connector::uds::socket_path_to_uri(path))
            }
            #[cfg(windows)]
            TransportConfig::WindowsNamedPipe(pipe) => build_transport_uri(
                url,
                libdd_common::connector::named_pipe::named_pipe_path_to_uri(std::path::Path::new(
                    pipe,
                )),
            ),
        }
    }
}

/// Build a transport URI by combining a transport-specific base URI with the
/// path from the original request URL.
fn build_transport_uri(
    url: &str,
    transport_uri: Result<hyper::Uri, hyper::http::Error>,
) -> Result<hyper::Uri, HttpClientError> {
    let parsed: hyper::Uri = url
        .parse()
        .map_err(|e: hyper::http::uri::InvalidUri| HttpClientError::InvalidConfig(e.to_string()))?;
    let request_path = parsed.path_and_query().map_or("/", |pq| pq.as_str());
    let base = transport_uri.map_err(|e| HttpClientError::InvalidConfig(e.to_string()))?;
    format!(
        "{}://{}{}",
        base.scheme_str().unwrap_or("unix"),
        base.authority().map_or("", |a| a.as_str()),
        request_path
    )
    .parse()
    .map_err(|e: hyper::http::uri::InvalidUri| HttpClientError::InvalidConfig(e.to_string()))
}

/// Build the request body from an HttpRequest, returning the body and an
/// optional Content-Type header for multipart requests.
fn build_body(request: &mut HttpRequest) -> Result<(Body, Option<String>), HttpClientError> {
    if !request.multipart_parts.is_empty() && !request.body.is_empty() {
        return Err(HttpClientError::InvalidConfig(
            "request cannot have both multipart parts and a body".to_owned(),
        ));
    }

    if !request.multipart_parts.is_empty() {
        let parts: Vec<libdd_common::multipart::MultipartPart> =
            std::mem::take(&mut request.multipart_parts)
                .into_iter()
                .map(convert_multipart_part)
                .collect();
        let form = libdd_common::multipart::MultipartFormData::encode(parts);
        let ct = form.content_type();
        Ok((Body::from_bytes(form.into_body().into()), Some(ct)))
    } else if !request.body.is_empty() {
        let body = std::mem::take(&mut request.body);
        Ok((Body::from_bytes(body), None))
    } else {
        Ok((Body::empty(), None))
    }
}

fn convert_multipart_part(
    p: crate::request::MultipartPart,
) -> libdd_common::multipart::MultipartPart {
    let mut part = libdd_common::multipart::MultipartPart::new(p.name, p.data.to_vec());
    if let Some(f) = p.filename {
        part = part.filename(f);
    }
    if let Some(ct) = p.content_type {
        part = part.content_type(ct);
    }
    part
}

fn convert_method(method: HttpMethod) -> hyper::http::Method {
    match method {
        HttpMethod::Get => hyper::http::Method::GET,
        HttpMethod::Post => hyper::http::Method::POST,
        HttpMethod::Put => hyper::http::Method::PUT,
        HttpMethod::Delete => hyper::http::Method::DELETE,
        HttpMethod::Head => hyper::http::Method::HEAD,
        HttpMethod::Patch => hyper::http::Method::PATCH,
        HttpMethod::Options => hyper::http::Method::OPTIONS,
    }
}

fn collect_response_headers<T>(
    response: &hyper::http::Response<T>,
) -> Result<Vec<(String, String)>, HttpClientError> {
    response
        .headers()
        .iter()
        .map(|(name, value)| {
            let v = value.to_str().map_err(|_| {
                HttpClientError::IoError(format!(
                    "response header '{}' contains non-UTF-8 value",
                    name
                ))
            })?;
            Ok((name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(feature = "hyper-backend")]
fn map_hyper_error(e: hyper_util::client::legacy::Error) -> HttpClientError {
    let err = http_common::into_error(e);
    match err.kind() {
        http_common::ErrorKind::Timeout => HttpClientError::TimedOut,
        http_common::ErrorKind::Closed => HttpClientError::ConnectionFailed(err.to_string()),
        _ => HttpClientError::IoError(err.to_string()),
    }
}

#[cfg(feature = "hyper-backend")]
impl super::Backend for HyperBackend {
    async fn send(
        &self,
        mut request: HttpRequest,
        config: &HttpClientConfig,
    ) -> Result<HttpResponse, HttpClientError> {
        let uri = self.rewrite_url(&request.url)?;
        let method = convert_method(request.method);
        let (body, multipart_content_type) = build_body(&mut request)?;

        let mut builder = hyper::http::Request::builder().method(method).uri(uri);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(ct) = multipart_content_type {
            builder = builder.header("content-type", ct);
        }

        let hyper_request = builder
            .body(body)
            .map_err(|e| HttpClientError::InvalidConfig(e.to_string()))?;

        let timeout = request.timeout.unwrap_or(config.timeout());
        let response = tokio::time::timeout(timeout, self.client.request(hyper_request))
            .await
            .map_err(|_| HttpClientError::TimedOut)?
            .map_err(map_hyper_error)?;

        let status = response.status().as_u16();
        let headers = collect_response_headers(&response)?;

        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| HttpClientError::IoError(e.to_string()))?
            .to_bytes();

        if config.treat_http_errors_as_errors() && status >= 400 {
            return Err(HttpClientError::RequestFailed {
                status,
                body: String::from_utf8_lossy(&body_bytes).into_owned(),
            });
        }

        Ok(HttpResponse {
            status_code: status,
            headers,
            body: body_bytes,
        })
    }
}
