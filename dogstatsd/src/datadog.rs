// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//!Types to serialize data into the Datadog API

use datadog_protos::metrics::SketchPayload;
use protobuf::Message;
use reqwest;
use serde::{Serialize, Serializer};
use serde_json;
use std::fmt;
use std::time::Duration;
use tracing::{debug, error};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Site(String);

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Site {
    pub fn new(prefix: String) -> Self {
        Site(prefix)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DdUrl(String);

impl fmt::Display for DdUrl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl DdUrl {
    pub fn new(prefix: String) -> Self {
        DdUrl(prefix)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DdDdUrl(String);

impl fmt::Display for DdDdUrl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl DdDdUrl {
    pub fn new(prefix: String) -> Self {
        DdDdUrl(prefix)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntakeUrlPrefix(String);

impl fmt::Display for IntakeUrlPrefix {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl IntakeUrlPrefix {
    /// # Safety
    ///
    /// This function is unsafe because it does no validation on the input string. It also does not
    /// follow our convention of using the metrics intake url prefix over the site name. This is
    /// fine for tests, but in production we should be using from_site_or_dd_urls instead.
    #[inline]
    pub unsafe fn new_unchecked(prefix: String) -> Self {
        IntakeUrlPrefix(prefix)
    }

    #[inline]
    fn new(prefix: String) -> Self {
        // Maybe we will validate this in the future, but for now we assume that the prefix is
        // sensible.
        IntakeUrlPrefix(prefix)
    }

    #[inline]
    fn from_site(site: String) -> Self {
        IntakeUrlPrefix(format!("https://api.{}", site))
    }

    #[inline]
    pub fn from_site_or_dd_urls(
        site: Option<Site>,
        dd_url: Option<DdUrl>,
        dd_dd_url: Option<DdDdUrl>,
    ) -> Result<Self, &'static str> {
        match (site, dd_url, dd_dd_url) {
            (None, None, None) => Err("No intake URL configuration"),
            (_, _, Some(dd_dd_url)) => Ok(Self::new(dd_dd_url.into_string())),
            (_, Some(dd_url), None) => Ok(Self::new(dd_url.into_string())),
            (Some(site), None, None) => Ok(Self::from_site(site.into_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intake_url_prefix_from_site_or_dd_urls_requires_something() {
        assert_eq!(
            IntakeUrlPrefix::from_site_or_dd_urls(None, None, None),
            Err("No intake URL configuration")
        );
    }

    #[test]
    fn test_intake_url_prefix_from_site_or_dd_urls_picks_dd_dd_url_above_all() {
        assert_eq!(
            IntakeUrlPrefix::from_site_or_dd_urls(
                Some(Site::new("a_site".to_string())),
                Some(DdUrl::new("a_dd_url".to_string())),
                Some(DdDdUrl::new("a_dd_dd_url".to_string())),
            ),
            Ok(IntakeUrlPrefix::new("a_dd_dd_url".to_string()))
        );
    }

    #[test]
    fn test_intake_url_prefix_from_site_or_dd_urls_picks_dd_url_if_it_must() {
        assert_eq!(
            IntakeUrlPrefix::from_site_or_dd_urls(
                Some(Site::new("a_site".to_string())),
                Some(DdUrl::new("a_dd_url".to_string())),
                None,
            ),
            Ok(IntakeUrlPrefix::new("a_dd_url".to_string()))
        );
    }

    #[test]
    fn test_intake_url_prefix_from_site_or_dd_urls_picks_site_as_a_fallback() {
        assert_eq!(
            IntakeUrlPrefix::from_site_or_dd_urls(
                Some(Site::new("a_site".to_string())),
                None,
                None,
            ),
            Ok(IntakeUrlPrefix::new("https://api.a_site".to_string()))
        );
    }
}

/// Interface for the `DogStatsD` metrics intake API.
#[derive(Debug)]
pub struct DdApi {
    api_key: String,
    intake_url_prefix: IntakeUrlPrefix,
    client: reqwest::Client,
}

impl DdApi {
    #[must_use]
    pub fn new(
        api_key: String,
        intake_url_prefix: IntakeUrlPrefix,
        https_proxy: Option<String>,
        timeout: Duration,
    ) -> Self {
        let client = match Self::build_client(https_proxy, timeout) {
            Ok(client) => client,
            Err(e) => {
                error!("Unable to parse proxy URL, no proxy will be used. {:?}", e);
                reqwest::Client::new()
            }
        };
        DdApi {
            api_key,
            intake_url_prefix,
            client,
        }
    }

    /// Ship a serialized series to the API, blocking
    pub async fn ship_series(&self, series: &Series) {
        let body = serde_json::to_vec(&series).expect("failed to serialize series");
        debug!("Sending body: {:?}", &series);

        let url = format!("{}/api/v2/series", &self.intake_url_prefix);
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
        let url = format!("{}/api/beta/sketches", &self.intake_url_prefix);
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

    fn build_client(
        https_proxy: Option<String>,
        timeout: Duration,
    ) -> Result<reqwest::Client, reqwest::Error> {
        let mut builder = reqwest::Client::builder().timeout(timeout);
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
