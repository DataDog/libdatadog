// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//!Types to serialize data into the Datadog API

use datadog_protos::metrics::SketchPayload;
use protobuf::Message;
use regex::Regex;
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

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Invalid site: {0}")]
pub struct SiteError(String);

impl Site {
    pub fn new(site: String) -> Result<Self, SiteError> {
        // Datadog sites are generally domain names. In particular, they shouldn't have any slashes
        // in them. We expect this to be coming from a `DD_SITE` environment variable or the `site`
        // config field.
        let re = Regex::new(r"^[a-zA-Z0-9._-]+$").expect("invalid regex");
        if re.is_match(&site) {
            Ok(Site(site))
        } else {
            Err(SiteError(site))
        }
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Invalid URL prefix: {0}")]
pub struct UrlPrefixError(String);

fn validate_url_prefix(prefix: &String) -> Result<(), UrlPrefixError> {
    let re = Regex::new(r"^https?://[a-zA-Z0-9._-]+$").expect("invalid regex");
    if re.is_match(prefix) {
        Ok(())
    } else {
        Err(UrlPrefixError(prefix.clone()))
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
    pub fn new(prefix: String) -> Result<Self, UrlPrefixError> {
        match validate_url_prefix(&prefix) {
            Ok(_) => Ok(Self(prefix)),
            Err(e) => Err(e),
        }
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
    pub fn new(prefix: String) -> Result<Self, UrlPrefixError> {
        match validate_url_prefix(&prefix) {
            Ok(_) => Ok(Self(prefix)),
            Err(e) => Err(e),
        }
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricsIntakeUrlPrefixOverride(String);

impl fmt::Display for MetricsIntakeUrlPrefixOverride {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl MetricsIntakeUrlPrefixOverride {
    pub fn maybe_new(dd_url: Option<DdUrl>, dd_dd_url: Option<DdDdUrl>) -> Option<Self> {
        match (dd_url, dd_dd_url) {
            (None, None) => None,
            (_, Some(dd_dd_url)) => Some(Self(dd_dd_url.into_string())),
            (Some(dd_url), None) => Some(Self(dd_url.into_string())),
        }
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn override_can_be_empty() {
        assert_eq!(MetricsIntakeUrlPrefixOverride::maybe_new(None, None), None);
    }

    #[test]
    fn override_prefers_dd_dd_url() {
        assert_eq!(
            MetricsIntakeUrlPrefixOverride::maybe_new(
                Some(DdUrl::new("http://a_dd_url".to_string()).unwrap()),
                Some(DdDdUrl::new("https://a_dd_dd_url".to_string()).unwrap())
            ),
            Some(MetricsIntakeUrlPrefixOverride(
                "https://a_dd_dd_url".to_string()
            ))
        );
    }

    #[test]
    fn override_will_take_dd_url() {
        assert_eq!(
            MetricsIntakeUrlPrefixOverride::maybe_new(
                Some(DdUrl::new("http://a_dd_url".to_string()).unwrap()),
                None
            ),
            Some(MetricsIntakeUrlPrefixOverride(
                "http://a_dd_url".to_string()
            ))
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricsIntakeUrlPrefix(String);

impl fmt::Display for MetricsIntakeUrlPrefix {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Missing intake URL configuration")]
pub struct MissingIntakeUrlError;

impl MetricsIntakeUrlPrefix {
    /// # Safety
    ///
    /// This function is unsafe because it does no validation on the input string. It also does not
    /// follow our convention of using the metrics intake url prefix over the site name. This is
    /// fine for tests, but in production we should be using from_site_or_dd_urls instead.
    #[inline]
    pub unsafe fn new_unchecked(prefix: String) -> Self {
        MetricsIntakeUrlPrefix(prefix)
    }

    #[inline]
    fn new(validated_prefix: String) -> Self {
        validate_url_prefix(&validated_prefix).expect("Invalid URL prefix");

        MetricsIntakeUrlPrefix(validated_prefix)
    }

    #[inline]
    fn from_site(site: Site) -> Self {
        MetricsIntakeUrlPrefix(format!("https://api.{}", site))
    }

    #[inline]
    pub fn from_site_or_override(
        site: Option<Site>,
        overridden_prefix: Option<MetricsIntakeUrlPrefixOverride>,
    ) -> Result<Self, MissingIntakeUrlError> {
        match (site, overridden_prefix) {
            (None, None) => Err(MissingIntakeUrlError),
            (_, Some(prefix)) => Ok(Self::new(prefix.into_string())),
            (Some(site), None) => Ok(Self::from_site(site)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intake_url_prefix_from_site_or_override_requires_something() {
        assert_eq!(
            MetricsIntakeUrlPrefix::from_site_or_override(None, None),
            Err(MissingIntakeUrlError)
        );
    }

    #[test]
    fn test_intake_url_prefix_from_site_or_override_picks_the_override() {
        assert_eq!(
            MetricsIntakeUrlPrefix::from_site_or_override(
                Some(Site::new("a_site".to_string()).unwrap()),
                MetricsIntakeUrlPrefixOverride::maybe_new(
                    Some(DdUrl::new("http://a_dd_url".to_string()).unwrap()),
                    None
                ),
            ),
            Ok(MetricsIntakeUrlPrefix::new("http://a_dd_url".to_string()))
        );
    }

    #[test]
    fn test_intake_url_prefix_from_site_or_override_picks_site_as_a_fallback() {
        assert_eq!(
            MetricsIntakeUrlPrefix::from_site_or_override(
                Some(Site::new("a_site".to_string()).unwrap()),
                None,
            ),
            Ok(MetricsIntakeUrlPrefix::new(
                "https://api.a_site".to_string()
            ))
        );
    }
}

/// Interface for the `DogStatsD` metrics intake API.
#[derive(Debug)]
pub struct DdApi {
    api_key: String,
    metrics_intake_url_prefix: MetricsIntakeUrlPrefix,
    client: reqwest::Client,
}

impl DdApi {
    #[must_use]
    pub fn new(
        api_key: String,
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix,
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
            metrics_intake_url_prefix,
            client,
        }
    }

    /// Ship a serialized series to the API, blocking
    pub async fn ship_series(&self, series: &Series) {
        let body = serde_json::to_vec(&series).expect("failed to serialize series");
        debug!("Sending body: {:?}", &series);

        let url = format!("{}/api/v2/series", &self.metrics_intake_url_prefix);
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
        let url = format!("{}/api/beta/sketches", &self.metrics_intake_url_prefix);
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
