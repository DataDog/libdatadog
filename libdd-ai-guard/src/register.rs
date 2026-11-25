// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, header::CONTENT_TYPE};
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const REGISTER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostApplication {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub connection_id: String,
    pub host_application: HostApplication,
    pub host_tags: Vec<String>,
}

pub async fn register_connection(
    register_url: &str,
    payload: &RegisterRequest,
) -> Result<u16, String> {
    let body = serde_json::to_vec(payload).map_err(|e| format!("serialize register body: {e}"))?;
    let uri: http::Uri = register_url
        .parse()
        .map_err(|e| format!("invalid register URL '{register_url}': {e}"))?;

    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| format!("build register request: {e}"))?;

    let client = Client::builder(TokioExecutor::new()).build_http();
    let resp = tokio::time::timeout(REGISTER_TIMEOUT, client.request(req))
        .await
        .map_err(|_| format!("register POST timed out after {}s", REGISTER_TIMEOUT.as_secs()))?
        .map_err(|e| format!("register POST failed: {e}"))?;

    let status = resp.status().as_u16();
    let resp_bytes = resp
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    if (200..300).contains(&status) {
        info!(
            connection_id = %payload.connection_id,
            register_url = %register_url,
            status,
            "registered RC x509 connection"
        );
        Ok(status)
    } else {
        let resp_text = String::from_utf8_lossy(&resp_bytes);
        warn!(
            connection_id = %payload.connection_id,
            register_url = %register_url,
            status,
            body = %resp_text,
            "RC x509 registration returned non-2xx"
        );
        Err(format!("HTTP {status}: {resp_text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn posts_new_connection_shape_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
            raw
        });

        let payload = RegisterRequest {
            connection_id: "11111111-2222-3333-4444-555555555555".to_string(),
            host_application: HostApplication {
                name: "dd-trace-py".to_string(),
                version: "4.18.0".to_string(),
            },
            host_tags: vec!["env:staging".to_string(), "service:poc".to_string()],
        };

        let status = register_connection(&format!("http://{addr}/register"), &payload)
            .await
            .expect("register ok");
        assert_eq!(status, 200);

        let raw_req = server.await.unwrap();
        let body_str = raw_req.split("\r\n\r\n").nth(1).unwrap();
        let parsed: RegisterRequest = serde_json::from_str(body_str).unwrap();
        assert_eq!(parsed, payload);
    }
}
