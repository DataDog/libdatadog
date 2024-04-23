// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::{debug, info, warn};

use anyhow::anyhow;
use cadence::prelude::*;
use cadence::{
    Metric, MetricBuilder, QueuingMetricSink, StatsdClient, UdpMetricSink,
};
#[cfg(unix)]
use cadence::UnixMetricSink;
use ddcommon::connector::uds::socket_path_from_uri;
use std::net::{ToSocketAddrs, UdpSocket};

#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction {
    Count(String, u64, Vec<Tag>),
    Distribution(String, f64, Vec<Tag>),
    Gauge(String, f64, Vec<Tag>),
    Histogram(String, f64, Vec<Tag>),
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
        if self.endpoint.is_none() {
            return;
        }

        let client = match self.get_client() {
            Ok(client) => client,
            Err(msg) => {
                self.endpoint = None;
                warn!("Cannot send DogStatsD metrics: {}", msg);
                return;
            }
        };

        for action in actions {
            if let Err(err) = match action {
                DogStatsDAction::Count(metric, value, tags) => {
                    do_send(client.count_with_tags(metric.as_str(), value), &tags)
                }
                DogStatsDAction::Distribution(metric, value, tags) => {
                    do_send(client.distribution_with_tags(metric.as_str(), value), &tags)
                }
                DogStatsDAction::Gauge(metric, value, tags) => {
                    do_send(client.gauge_with_tags(metric.as_str(), value), &tags)
                }
                DogStatsDAction::Histogram(metric, value, tags) => {
                    do_send(client.histogram_with_tags(metric.as_str(), value), &tags)
                }
            } {
                debug!("Error while sending metric: {}", err); // FIXME: log?
            }
        }
    }

    fn get_client(&mut self) -> anyhow::Result<&StatsdClient> {
        let opt = &mut self.client;
        let client = match opt {
            Some(client) => client,
            None => opt.get_or_insert(create_client(self.endpoint.clone())?),
        };

        Ok(client)
    }
}

fn do_send<'a, T>(mut builder: MetricBuilder<'a, '_, T>, tags: &'a Vec<Tag>) -> anyhow::Result<()>
where
    T: Metric + From<String>,
{
    for tag in tags {
        builder = builder.with_tag_value(tag.as_ref());
    }
    builder.try_send()?;
    Ok(())
}

fn create_client(endpoint: Option<Endpoint>) -> anyhow::Result<StatsdClient> {
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => return Err(anyhow!("no endpoint set")),
    };

    return match endpoint.url.scheme_str() {
        #[cfg(unix)]
        Some("unix") => {
            let socket = UnixDatagram::unbound()?;
            socket.set_nonblocking(true)?;
            let sink = QueuingMetricSink::with_capacity(
                UnixMetricSink::from(socket_path_from_uri(&endpoint.url)?, socket),
                QUEUE_SIZE,
            );

            Ok(StatsdClient::from_sink("", sink))
        }
        _ => {
            let host = endpoint.url.host().ok_or(anyhow!("invalid host"))?;
            let port = endpoint.url.port().ok_or(anyhow!("invalid port"))?.as_u16();

            let server_address = (host, port)
                .to_socket_addrs()?
                .next()
                .ok_or(anyhow!("invalid address"))?;

            let socket;
            if server_address.is_ipv4() {
                socket = UdpSocket::bind("0.0.0.0:0")?;
            } else {
                socket = UdpSocket::bind("[::]:0")?;
            }
            socket.set_nonblocking(true)?;

            let sink = QueuingMetricSink::with_capacity(
                UdpMetricSink::from((host, port), socket)?,
                QUEUE_SIZE,
            );

            Ok(StatsdClient::from_sink("", sink))
        }
    };
}
