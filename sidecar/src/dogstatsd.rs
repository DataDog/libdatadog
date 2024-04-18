// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_statsd::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use ddcommon::Endpoint;
use ddcommon::tag::Tag;

#[derive(Default, Clone)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
}

impl Config {
    pub fn host_with_port(&self) -> Result<String, &'static str> {
        if let Some(endpoint) = self.endpoint.clone() {
            let host = match endpoint.url.host() {
                None => return Err("the host is invalid"),
                Some(host) => host,
            };
            let port = match endpoint.url.port() {
                None => return Err("the port is invalid"),
                Some(port) => port,
            };

            return Ok(format!("{}:{}", host, port));
        }
        Err("the endpoint is not set")
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction {
    Count(String, f64, Vec<Tag>),
    Gauge(String, f64, Vec<Tag>),
}

#[derive(Default)]
pub struct Flusher {
    config: Config,
    client: Option<datadog_statsd::Client>,
}

impl Clone for Flusher {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: None,
        }
    }
}

impl Flusher {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        if endpoint.url.host().is_none() {
            info!("Disabling DogStatsD, no endpoint set");
            self.config.endpoint = None;
        } else {
            info!("Updating DogStatsD endpoint to {}", endpoint.url.clone());
            self.config.endpoint = Some(endpoint);
        }
        self.client = None;
    }

    pub fn send(&mut self, actions: Vec<DogStatsDAction>) {
        let host = self.config.host_with_port();
        if let Err(msg) = host {
            info!("Cannot send metrics to DogStatsd: {}", msg);
            return;
        }

        if self.client.is_none() {
            debug!("Creating a DogStatsD client sending to {}", host.clone().unwrap());

            // FIXME: can be a socket (DD_DOGSTATSD_URL => https://docs.datadoghq.com/developers/dogstatsd/?tab=hostagent)
            self.client = Some(Client::new(host.unwrap(), "", None).unwrap());
        }

        if let Some(client) = &self.client { // FIXME: Better way?
            // FIXME: use pipeline?
            for action in actions {
                match action {
                    DogStatsDAction::Count(metric, value, tags) => client.count(metric.as_str(), value, &self.convert_tags(&tags)),
                    DogStatsDAction::Gauge(metric, value, tags) => client.gauge(metric.as_str(), value, &self.convert_tags(&tags)),
                }
            }
        }
    }

    fn convert_tags<'a>(&'a self, tags: &'a Vec<Tag>) -> Option<Vec<&str>> { // FIXME: lifetime...
        if tags.len() == 0 {
            return None;
        }

        Some(tags.into_iter().map(|t| {
            t.as_ref()
        }).collect())
    }
}
