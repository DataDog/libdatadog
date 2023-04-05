// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::config::Config;
use crate::metrics::Metric;
use crate::payload::construct_distribution_payload;

use std::net::SocketAddr;
use std::str;
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use std::time::{self, SystemTime};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use hyper::header::CONTENT_TYPE;
use hyper::http::HeaderValue;
use hyper::{Body, Client, Request};
use hyper_rustls::HttpsConnectorBuilder;

const DOGSTATSD_PORT: u16 = 8125;
const BUFFER_SIZE: usize = 8192;

pub struct MetricsAgent {
    config: Config,
    buf: Arc<Mutex<Vec<Metric>>>,
}

impl MetricsAgent {
    pub fn with_config(config: Config) -> Self {
        Self {
            config,
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn run(&self) {
        // Create a UDP socket and bind to the dogstatsd port
        let socket = UdpSocket::bind(SocketAddr::new(
            std::net::Ipv4Addr::new(0, 0, 0, 0).into(),
            DOGSTATSD_PORT,
        ))
        .await
        .expect("Error binding to socket");

        println!("Listening for dogstatsd packets on port {}", DOGSTATSD_PORT);

        // We use a shared vec here instead of a channel because we need to buffer the data at
        // some point anyways. If we had used a channel here, we'd still need to add it to
        // some type of buffer, so we'd need to implement a vector buffer anyways.
        // Instead, we can save a step and just add it to the buffer directly.
        let buf_producer = self.buf.clone();
        let buf_consumer = self.buf.clone();

        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_only()
            .enable_http1()
            .build();
        let http_client = Client::builder().build::<_, Body>(https);

        // Process DogstatsD UDP packets and write them to our shared buffer
        tokio::spawn(async move {
            loop {
                let mut buffer = [0; BUFFER_SIZE];
                let bytes_received = match socket.recv_from(&mut buffer).await {
                    Ok((size, _)) => size,
                    Err(e) => {
                        println!("Error receiving bytes from UDP packet: {}", e);
                        continue;
                    }
                };

                let unix_timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
                    Ok(duration) => duration.as_secs(),
                    Err(e) => {
                        println!("Error generating unix timestamp: {}", e);
                        continue;
                    }
                };

                let metrics: Vec<Metric> = match str::from_utf8(&buffer[..bytes_received]) {
                    Ok(metrics_str) => metrics_str
                        .split('\n')
                        .filter_map(|metric_str| Metric::from_string(metric_str, unix_timestamp))
                        .collect(),
                    Err(e) => {
                        println!("Error converting metric to str: {}", e);
                        continue;
                    }
                };

                let mut metrics_to_process = buf_producer.lock().await;
                for metric in metrics {
                    metrics_to_process.push(metric);
                }
            }
        });

        // Process metrics we've parsed and flush them to Datadog
        loop {
            // We flush every three seconds so we don't send too many requests to Datadog
            // in a short amount of time. It's a good default for now.
            tokio::time::sleep(time::Duration::from_millis(3000)).await;

            let mut metrics_to_process = buf_consumer.lock().await;

            // We don't have any new metrics
            if metrics_to_process.is_empty() {
                continue;
            }

            let payload = match construct_distribution_payload(metrics_to_process.to_vec()) {
                Ok(payload) => payload,
                Err(e) => {
                    println!("Error serializing payload: {}", e);
                    continue;
                }
            };

            println!("Sending payload: {}", payload);

            // Create a POST request with the headers and payload
            let request_option = Request::builder()
                .method("POST")
                .uri(format!(
                    "https://api.{}/api/v1/distribution_points",
                    self.config.site
                ))
                .header("DD-API-KEY", &self.config.api_key)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(Body::from(payload.to_string()));

            let request = match request_option {
                Ok(request) => request,
                Err(e) => {
                    println!("Error constructing payload: {}", e);
                    continue;
                }
            };

            // Send the request and handle the response
            match http_client.request(request).await {
                Ok(_) => {
                    println!("Successfully posted request");
                    // Remove all elements from the buffer, as they've
                    // been sent at this point
                    metrics_to_process.clear();
                }
                Err(e) => {
                    println!("Error sending request to Datadog: {}", e);
                }
            }
        }
    }
}
