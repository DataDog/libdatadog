// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//!Types to serialize data into the Datadog API

use crate::flusher::ShippingError;
use datadog_protos::metrics::SketchPayload;
use derive_more::{Display, Into};
use protobuf::Message;
use regex::Regex;
use reqwest;
use reqwest::{Client, Response};
use serde::{Serialize, Serializer};
use serde_json;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, error};
use zstd::stream::write::Encoder;

// TODO: Move to the more ergonomic LazyLock when MSRV is 1.80
static SITE_RE: OnceLock<Regex> = OnceLock::new();
fn get_site_re() -> &'static Regex {
    #[allow(clippy::expect_used)]
    SITE_RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9._:-]+$").expect("invalid regex"))
}
static URL_PREFIX_RE: OnceLock<Regex> = OnceLock::new();
fn get_url_prefix_re() -> &'static Regex {
    #[allow(clippy::expect_used)]
    URL_PREFIX_RE.get_or_init(|| Regex::new(r"^https?://[a-zA-Z0-9._:-]+$").expect("invalid regex"))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display, Into)]
pub struct Site(String);

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Invalid site: {0}")]
pub struct SiteError(String);

impl Site {
    pub fn new(site: String) -> Result<Self, SiteError> {
        // Datadog sites are generally domain names. In particular, they shouldn't have any slashes
        // in them. We expect this to be coming from a `DD_SITE` environment variable or the `site`
        // config field.
        if get_site_re().is_match(&site) {
            Ok(Site(site))
        } else {
            Err(SiteError(site))
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Invalid URL prefix: {0}")]
pub struct UrlPrefixError(String);

fn validate_url_prefix(prefix: &str) -> Result<(), UrlPrefixError> {
    if get_url_prefix_re().is_match(prefix) {
        Ok(())
    } else {
        Err(UrlPrefixError(prefix.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display, Into)]
pub struct DdUrl(String);

impl DdUrl {
    pub fn new(prefix: String) -> Result<Self, UrlPrefixError> {
        validate_url_prefix(&prefix)?;
        Ok(Self(prefix))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display, Into)]
pub struct DdDdUrl(String);

impl DdDdUrl {
    pub fn new(prefix: String) -> Result<Self, UrlPrefixError> {
        validate_url_prefix(&prefix)?;
        Ok(Self(prefix))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display, Into)]
pub struct MetricsIntakeUrlPrefixOverride(String);

impl MetricsIntakeUrlPrefixOverride {
    pub fn maybe_new(dd_url: Option<DdUrl>, dd_dd_url: Option<DdDdUrl>) -> Option<Self> {
        match (dd_url, dd_dd_url) {
            (None, None) => None,
            (_, Some(dd_dd_url)) => Some(Self(dd_dd_url.into())),
            (Some(dd_url), None) => Some(Self(dd_url.into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display)]
pub struct MetricsIntakeUrlPrefix(String);

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("Missing intake URL configuration")]
pub struct MissingIntakeUrlError;

impl MetricsIntakeUrlPrefix {
    #[inline]
    pub fn new(
        site: Option<Site>,
        overridden_prefix: Option<MetricsIntakeUrlPrefixOverride>,
    ) -> Result<Self, MissingIntakeUrlError> {
        match (site, overridden_prefix) {
            (None, None) => Err(MissingIntakeUrlError),
            (_, Some(prefix)) => Ok(Self::new_expect_validated(prefix.into())),
            (Some(site), None) => Ok(Self::from_site(site)),
        }
    }

    #[inline]
    fn new_expect_validated(validated_prefix: String) -> Self {
        #[allow(clippy::expect_used)]
        validate_url_prefix(&validated_prefix).expect("Invalid URL prefix");

        Self(validated_prefix)
    }

    #[inline]
    fn from_site(site: Site) -> Self {
        Self(format!("https://api.{site}"))
    }
}

/// Interface for the `DogStatsD` metrics intake API.
#[derive(Debug, Clone)]
pub struct DdApi {
    api_key: String,
    metrics_intake_url_prefix: MetricsIntakeUrlPrefix,
    client: Option<Client>,
    retry_strategy: RetryStrategy,
}

impl DdApi {
    #[must_use]
    pub fn new(
        api_key: String,
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix,
        https_proxy: Option<String>,
        timeout: Duration,
        retry_strategy: RetryStrategy,
    ) -> Self {
        let client = build_client(https_proxy, timeout)
            .inspect_err(|e| {
                error!("Unable to create client {:?}", e);
            })
            .ok();
        DdApi {
            api_key,
            metrics_intake_url_prefix,
            client,
            retry_strategy,
        }
    }

    /// Ship a serialized series to the API, blocking
    pub async fn ship_series(&self, series: &Series) -> Result<Response, ShippingError> {
        let url = format!("{}/api/v2/series", &self.metrics_intake_url_prefix);
        let safe_body = serde_json::to_vec(&series)
            .map_err(|e| ShippingError::Payload(format!("Failed to serialize series: {e}")))?;
        debug!("Sending body: {:?}", &series);
        self.ship_data(url, safe_body, "application/json").await
    }

    pub async fn ship_distributions(
        &self,
        sketches: &SketchPayload,
    ) -> Result<Response, ShippingError> {
        let url = format!("{}/api/beta/sketches", &self.metrics_intake_url_prefix);
        let safe_body = sketches
            .write_to_bytes()
            .map_err(|e| ShippingError::Payload(format!("Failed to serialize series: {e}")))?;
        debug!("Sending distributions: {:?}", &sketches);
        self.ship_data(url, safe_body, "application/x-protobuf")
            .await
        // TODO maybe go to coded output stream if we incrementally
        // add sketch payloads to the buffer
        // something like this, but fix the utf-8 encoding issue
        // {
        //     let mut output_stream = CodedOutputStream::vec(&mut buf);
        //     let _ = output_stream.write_tag(1, protobuf::rt::WireType::LengthDelimited);
        //     let _ = output_stream.write_message_no_tag(&sketches);
        //     TODO not working, has utf-8 encoding issue in dist-intake
        //}
    }

    async fn ship_data(
        &self,
        url: String,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<Response, ShippingError> {
        let client = &self
            .client
            .as_ref()
            .ok_or_else(|| ShippingError::Destination(None, "No client".to_string()))?;
        let start = std::time::Instant::now();

        let result = (|| -> std::io::Result<Vec<u8>> {
            let mut encoder = Encoder::new(Vec::new(), 6)?;
            encoder.write_all(&body)?;
            encoder.finish()
        })();

        let mut builder = client
            .post(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("Content-Type", content_type);

        builder = match result {
            Ok(compressed) => builder.header("Content-Encoding", "zstd").body(compressed),
            Err(err) => {
                debug!("Sending uncompressed data, failed to compress: {err}");
                builder.body(body)
            }
        };

        let resp = self.send_with_retry(builder).await;

        let elapsed = start.elapsed();
        debug!("Request to {} took {}ms", url, elapsed.as_millis());
        resp
    }

    async fn send_with_retry(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<Response, ShippingError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let cloned_builder = match builder.try_clone() {
                Some(b) => b,
                None => {
                    return Err(ShippingError::Destination(
                        None,
                        "Failed to clone request".to_string(),
                    ));
                }
            };

            let response = cloned_builder.send().await;
            match response {
                Ok(response) if response.status().is_success() => {
                    return Ok(response);
                }
                _ => {}
            }

            match self.retry_strategy {
                RetryStrategy::LinearBackoff(max_attempts, _)
                | RetryStrategy::Immediate(max_attempts)
                    if attempts >= max_attempts =>
                {
                    let status = match response {
                        Ok(response) => Some(response.status()),
                        Err(err) => err.status(),
                    };
                    // handle if status code missing like timeout
                    return Err(ShippingError::Destination(
                        status,
                        format!("Failed to send request after {} attempts", max_attempts)
                            .to_string(),
                    ));
                }
                RetryStrategy::LinearBackoff(_, delay) => {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum RetryStrategy {
    Immediate(u64),          // attempts
    LinearBackoff(u64, u64), // attempts, delay
}

fn build_client(https_proxy: Option<String>, timeout: Duration) -> Result<Client, reqwest::Error> {
    let mut builder = Client::builder().timeout(timeout);
    if let Some(proxy) = https_proxy {
        builder = builder.proxy(reqwest::Proxy::https(proxy)?);
    }
    builder.build()
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
    pub(crate) name: String,
    #[serde(rename = "type")]
    /// The kind of this resource
    pub(crate) kind: String,
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

    #[test]
    fn test_intake_url_prefix_new_requires_something() {
        assert_eq!(
            MetricsIntakeUrlPrefix::new(None, None),
            Err(MissingIntakeUrlError)
        );
    }

    #[test]
    fn test_intake_url_prefix_new_picks_the_override() {
        assert_eq!(
            MetricsIntakeUrlPrefix::new(
                Some(Site::new("a_site".to_string()).unwrap()),
                MetricsIntakeUrlPrefixOverride::maybe_new(
                    Some(DdUrl::new("http://a_dd_url".to_string()).unwrap()),
                    None
                ),
            ),
            Ok(MetricsIntakeUrlPrefix::new_expect_validated(
                "http://a_dd_url".to_string()
            ))
        );
    }

    #[test]
    fn test_intake_url_prefix_new_picks_site_as_a_fallback() {
        assert_eq!(
            MetricsIntakeUrlPrefix::new(Some(Site::new("a_site".to_string()).unwrap()), None,),
            Ok(MetricsIntakeUrlPrefix::new_expect_validated(
                "https://api.a_site".to_string()
            ))
        );
    }
}
