// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use reqwest::{header, Body, IntoUrl, Response};

// TODO: extract the reqwest to allow exchange for alternative implementations, in cases like wasm
pub async fn request<B: Into<Body>, T: IntoUrl>(
    url: T,
    body: B,
    api_key: Option<&str>,
) -> anyhow::Result<Response> {
    let client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .build()?;
    let mut req = client
        .post(url)
        .header(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        )
        .body(body);
    if let Some(api_key) = api_key {
        req = req.header("DD-API-KEY", api_key)
    }

    let res = client.execute(req.build()?).await?;

    Ok(res)
}
