// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    future::Future,
    pin::pin,
    process::ExitCode,
    task::{Context, Poll, Waker},
    time::UNIX_EPOCH,
};

use libdd_http_client_lite::{
    client::{HttpConnection, HttpResource},
    dns::{DnsResolver, Resolver as _},
    env::Environment,
    headers::ContentType,
    request::{Method, RequestBuilder as _},
    rustix::TcpStream,
    Error,
};
use libdd_telemetry::{
    data::{
        metrics::{MetricNamespace, MetricType, Serie},
        ApiVersion, Application, GenerateMetrics, Host, Payload, Telemetry,
    },
    worker::http_client::header,
};

const AGENT_HOST: &str = "agent.local";
const AGENT_PORT: u16 = 8126;
const AGENT_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";
const DNS_ENTRIES: &[(&str, &str)] = &[("agent.local", "127.0.0.1")];

fn main() -> ExitCode {
    let timestamp = UNIX_EPOCH
        .elapsed()
        .map_or(0, |duration| duration.as_secs());
    let application = Application {
        service_name: "libdd-http-client-lite-example".to_owned(),
        language_name: "rust".to_owned(),
        language_version: "unknown".to_owned(),
        tracer_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..Application::default()
    };
    let host = Host {
        hostname: "unknown_hostname".to_owned(),
        ..Host::default()
    };
    let payload = Payload::GenerateMetrics(GenerateMetrics {
        series: vec![Serie {
            namespace: MetricNamespace::Telemetry,
            metric: "http_client_lite.metrics_submissions".to_owned(),
            points: vec![(timestamp, 1.0)],
            tags: Vec::new(),
            common: false,
            _type: MetricType::Count,
            interval: 0,
        }],
    });
    let telemetry = Telemetry {
        api_version: ApiVersion::V2,
        tracer_time: timestamp,
        runtime_id: "00000000-0000-0000-0000-000000000000",
        seq_id: 0,
        application: &application,
        host: &host,
        origin: None,
        payload: &payload,
    };
    let body = match serde_json::to_vec(&telemetry) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("telemetry serialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let dns = DnsResolver::new(Environment::new(DNS_ENTRIES));
    let address = match dns.resolve(
        AGENT_HOST,
        libdd_http_client_lite::io::embedded_nal_async::AddrType::Either,
    ) {
        Ok(address) => address,
        Err(error) => {
            eprintln!("DNS lookup failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let connection = match TcpStream::connect((address, AGENT_PORT).into()) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("TCP connection failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut resource = HttpResource {
        conn: HttpConnection::Plain(connection),
        host: AGENT_HOST,
        base_path: "",
    };

    match block_on(send_metrics(&mut resource, &telemetry, &body)) {
        Ok(status) => {
            println!("telemetry metric submitted, status={status}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("telemetry metric submission failed: {error:?}");
            ExitCode::FAILURE
        }
    }
}

async fn send_metrics(
    resource: &mut HttpResource<'_, TcpStream>,
    telemetry: &Telemetry<'_>,
    body: &[u8],
) -> Result<u16, Error> {
    let request_type = header::REQUEST_TYPE;
    let api_version = header::API_VERSION;
    let library_language = header::LIBRARY_LANGUAGE;
    let library_version = header::LIBRARY_VERSION;
    let headers = [
        (request_type.as_str(), telemetry.payload.request_type()),
        (api_version.as_str(), telemetry.api_version.to_str()),
        (
            library_language.as_str(),
            telemetry.application.language_name.as_str(),
        ),
        (
            library_version.as_str(),
            telemetry.application.tracer_version.as_str(),
        ),
    ];
    let request = resource
        .request(Method::POST, AGENT_PATH)
        .headers(&headers)
        .content_type(ContentType::ApplicationJson)
        .body(body);
    let mut response_buffer = [0_u8; 1_024];
    let response = request.send(&mut response_buffer).await?;
    Ok(response.status.0)
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::hint::spin_loop(),
        }
    }
}
