// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt::Debug;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use ddcommon::Endpoint;
use ddcommon::tag::Tag;

use std::net::{ToSocketAddrs, UdpSocket};
use std::os::unix::net::UnixDatagram;
use cadence::prelude::*;
use cadence::{MetricResult, QueuingMetricSink, StatsdClient, UdpMetricSink, UnixMetricSink};
use ddcommon::connector::uds::socket_path_from_uri;

/////////////////////////////////////////
// FIXME: error handling everywhere!!!
/////////////////////////////////////////

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction {
    Count(String, u64, Vec<Tag>),
    Gauge(String, f64, Vec<Tag>),
}

#[derive(Default)]
pub struct Flusher {
    endpoint: Option<Endpoint>,
    client: Option<StatsdClient>,
}

impl Flusher {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        self.client = None;
        match endpoint.url.host() {
            None => {
                info!("DogStatsD is disabled");
                self.endpoint = None;
            }
            Some(_) => {
                debug!("Updating DogStatsD endpoint to {}", endpoint.url.clone());
                self.endpoint = Some(endpoint);
            }
        }
    }

    pub fn send(&mut self, actions: Vec<DogStatsDAction>) {
        let client = match self.get_client() {
            Ok(client) => client,
            Err(msg) => {
                warn!("Cannot send DogStatsD metrics: {}", msg); // FIXME: avoid logs flood
                return;
            }
        };

        for action in actions {
            match action {
                DogStatsDAction::Count(metric, value, tags) => {
                    let mut builder = client.count_with_tags(metric.as_str(), value);
                    for tag in &tags {
                        builder = builder.with_tag_value(tag.as_ref());
                    }
                    let _ = builder.try_send();
                },
                DogStatsDAction::Gauge(metric, value, tags) => {
                    let mut builder = client.gauge_with_tags(metric.as_str(), value);
                    for tag in &tags {
                        builder = builder.with_tag_value(tag.as_ref());
                    }
                    let _ = builder.try_send();
                },
            }
        }
    }

    fn get_client(&mut self) -> Result<&StatsdClient, &str> {
        let endpoint= self.endpoint.clone();
        Ok(self.client.get_or_insert_with(|| create_client(endpoint).unwrap())) // FIXME: handle errors
    }
}

fn create_client(endpoint: Option<Endpoint>) -> Result<StatsdClient, &'static str> {
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => return Err("no endpoint set"),
    };

    match endpoint.url.scheme_str() {
        Some("unix") => {
            let socket_path = socket_path_from_uri(&endpoint.url).unwrap();

            let socket = UnixDatagram::unbound().unwrap();
            socket.set_nonblocking(true).unwrap();
            let sink = UnixMetricSink::from(socket_path, socket);
            let queuing_sink = QueuingMetricSink::with_capacity(sink, QUEUE_SIZE);

            return Ok(StatsdClient::from_sink("", queuing_sink));
        },
        _ => {
            let host = endpoint.url.host().unwrap();
            let port = endpoint.url.port().unwrap().as_u16();

            let server_address = (host, port)
                .to_socket_addrs().unwrap()
                .next()
                .ok_or_else(|| {}).unwrap();

            let socket;
            if server_address.is_ipv4() {
                 socket = UdpSocket::bind("0.0.0.0:0").unwrap();
            } else {
                socket = UdpSocket::bind("[::]:0").unwrap();
            }
            socket.set_nonblocking(true).unwrap();

            let sink = UdpMetricSink::from((host, port), socket).unwrap();
            let queuing_sink = QueuingMetricSink::with_capacity(sink, QUEUE_SIZE);

            return Ok(StatsdClient::from_sink("", queuing_sink));
        }
    }
}
