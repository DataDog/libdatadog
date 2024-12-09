// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//!Types to serialize data into the Datadog API

use datadog_protos::metrics::SketchPayload;
use protobuf::Message;
use reqwest;
use serde::{Serialize, Serializer};
use serde_json;
use tracing::{debug, error};

/// Interface for the `DogStatsD` metrics intake API.
#[derive(Debug)]
pub struct DdApi {
    api_key: String,
    fqdn_site: String,
    client: reqwest::Client,
}

impl DdApi {
    #[must_use]
    pub fn new(api_key: String, site: String, https_proxy: Option<String>) -> Self {
        let client = match Self::build_client(https_proxy) {
            Ok(client) => client,
            Err(e) => {
                error!("Unable to parse proxy URL, no proxy will be used. {:?}", e);
                reqwest::Client::new()
            }
        };
        DdApi {
            api_key,
            fqdn_site: site,
            client,
        }
    }

    /// Ship a serialized series to the API, blocking
    pub async fn ship_series(&self, series: &Series) {
        let body = serde_json::to_vec(&series).expect("failed to serialize series");
        debug!("Sending body: {:?}", &series);

        let url = format!("{}/api/v2/series", &self.fqdn_site);
        let resp = self
            .client
            .post(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;

        match resp {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::ACCEPTED => {}
                unexpected_status_code => {
                    debug!(
                        "{}: Failed to push to API: {:?}",
                        unexpected_status_code,
                        resp.text().await.unwrap_or_default()
                    );
                }
            },
            Err(e) => {
                debug!("500: Failed to push to API: {:?}", e);
            }
        };
    }

    pub async fn ship_distributions(&self, sketches: &SketchPayload) {
        let url = format!("{}/api/beta/sketches", &self.fqdn_site);
        debug!("Sending distributions: {:?}", &sketches);
        // TODO maybe go to coded output stream if we incrementally
        // add sketch payloads to the buffer
        // something like this, but fix the utf-8 encoding issue
        // {
        //     let mut output_stream = CodedOutputStream::vec(&mut buf);
        //     let _ = output_stream.write_tag(1, protobuf::rt::WireType::LengthDelimited);
        //     let _ = output_stream.write_message_no_tag(&sketches);
        //     TODO not working, has utf-8 encoding issue in dist-intake
        //}
        let resp = self
            .client
            .post(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("Content-Type", "application/x-protobuf")
            .body(sketches.write_to_bytes().expect("can't write to buffer"))
            .send()
            .await;
        match resp {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::ACCEPTED => {}
                unexpected_status_code => {
                    debug!(
                        "{}: Failed to push to API: {:?}",
                        unexpected_status_code,
                        resp.text().await.unwrap_or_default()
                    );
                }
            },
            Err(e) => {
                debug!("500: Failed to push to API: {:?}", e);
            }
        };
    }

    fn build_client(https_proxy: Option<String>) -> Result<reqwest::Client, reqwest::Error> {
        let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5));
        if let Some(proxy) = https_proxy {
            builder = builder.proxy(reqwest::Proxy::https(proxy)?);
        }
        builder.build()
    }
}

#[derive(Debug, Serialize, Clone, Copy)]
/// A single point in time
pub(crate) struct Point {
    /// The time at which the point exists
    pub(crate) timestamp: u64,
    /// The point's value
    pub(crate) value: f64,
}

#[derive(Debug, Serialize)]
/// A named resource
pub(crate) struct Resource {
    /// The name of this resource
    pub(crate) name: &'static str,
    #[serde(rename = "type")]
    /// The kind of this resource
    pub(crate) kind: &'static str,
}

#[derive(Debug, Clone, Copy)]
/// The kinds of metrics the Datadog API supports
pub(crate) enum DdMetricKind {
    /// An accumulating sum
    Count,
    /// An instantaneous value
    Gauge,
}

impl Serialize for DdMetricKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match *self {
            DdMetricKind::Count => serializer.serialize_u32(0),
            DdMetricKind::Gauge => serializer.serialize_u32(1),
        }
    }
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)]
/// A named collection of `Point` instances.
pub(crate) struct Metric {
    /// The name of the point collection
    pub(crate) metric: &'static str,
    /// The collection of points
    pub(crate) points: [Point; 1],
    /// The resources associated with the points
    pub(crate) resources: Vec<Resource>,
    #[serde(rename = "type")]
    /// The kind of metric
    pub(crate) kind: DdMetricKind,
    pub(crate) tags: Vec<String>,
}

#[derive(Debug, Serialize)]
/// A collection of metrics as defined by the Datadog Metrics API.
// NOTE we have a number of `Vec` instances in this implementation that could
// otherwise be arrays, given that we have constants. Serializing to JSON would
// require us to avoid serializing None or Uninit values, so there's some custom
// work that's needed. For protobuf this more or less goes away.
pub struct Series {
    /// The collection itself
    pub(crate) series: Vec<Metric>,
}

impl Series {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.series.len()
    }
}
