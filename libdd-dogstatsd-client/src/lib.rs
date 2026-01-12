// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! dogstatsd-client implements a client to emit metrics to a dogstatsd server.
//! This is made use of in at least the data-pipeline and sidecar crates.

use libdd_common::tag::Tag;
use libdd_common::Endpoint;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::error;

use anyhow::anyhow;
use cadence::prelude::*;
#[cfg(unix)]
use cadence::UnixMetricSink;
use cadence::{Metric, MetricBuilder, QueuingMetricSink, StatsdClient, UdpMetricSink};
#[cfg(unix)]
use libdd_common::connector::uds::socket_path_from_uri;
use std::net::{ToSocketAddrs, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
use std::sync::{Arc, Mutex};

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

/// The `DogStatsDActionOwned` enum gathers the metric types that can be sent to the DogStatsD
/// server. This type takes ownership of the relevant data to support the sidecar better.
/// For documentation on the dogstatsd metric types: <https://docs.datadoghq.com/metrics/types/?tab=count#metric-types>
///
/// Originally I attempted to combine this type with `DogStatsDAction` but this GREATLY complicates
/// the types to the point of insanity. I was unable to come up with a satisfactory approach that
/// allows both the data-pipeline and sidecar crates to use the same type. If a future rustacean
/// wants to take a stab and open a PR please do so!
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDActionOwned {
    #[allow(missing_docs)]
    Count(String, i64, Vec<Tag>),
    #[allow(missing_docs)]
    Distribution(String, f64, Vec<Tag>),
    #[allow(missing_docs)]
    Gauge(String, f64, Vec<Tag>),
    #[allow(missing_docs)]
    Histogram(String, f64, Vec<Tag>),
    /// Cadence only support i64 type as value
    /// but Golang implementation uses string (<https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230>)
    /// and PHP implementation uses float or string (<https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251>)
    Set(String, i64, Vec<Tag>),
}

/// The `DogStatsDAction` enum gathers the metric types that can be sent to the DogStatsD server.
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction<'a, T: AsRef<str>, V: IntoIterator<Item = &'a Tag>> {
    // TODO: instead of AsRef<str> we can accept a marker Trait that users of this crate implement
    #[allow(missing_docs)]
    Count(T, i64, V),
    #[allow(missing_docs)]
    Distribution(T, f64, V),
    #[allow(missing_docs)]
    Gauge(T, f64, V),
    #[allow(missing_docs)]
    Histogram(T, f64, V),
    /// Cadence only support i64 type as value
    /// but Golang implementation uses string (<https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230>)
    /// and PHP implementation uses float or string (<https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251>)
    Set(T, i64, V),
}

/// A dogstatsd-client that flushes stats to a given endpoint.
#[derive(Debug, Default)]
pub struct Client {
    client: Mutex<Arc<Option<StatsdClient>>>,
    endpoint: Option<Endpoint>,
}

/// Build a new flusher instance pointed at the provided endpoint.
/// Returns error if the provided endpoint is not valid.
pub fn new(endpoint: Endpoint) -> anyhow::Result<Client> {
    // defer initialization of the client until the first metric is sent and we definitely know the
    // client is going to be used to communicate with the endpoint.
    Ok(Client {
        endpoint: Some(endpoint),
        ..Default::default()
    })
}

impl Client {
    /// Send a vector of DogStatsDActionOwned, this is the same as `send` except it uses the "owned"
    /// version of DogStatsDAction. See the docs for DogStatsDActionOwned for details.
    pub fn send_owned(&self, actions: Vec<DogStatsDActionOwned>) {
        let client_opt = match self.get_or_init_client() {
            Ok(client) => client,
            Err(e) => {
                error!(?e, "Failed to get client");
                return;
            }
        };

        if let Some(client) = &*client_opt {
            for action in actions {
                if let Err(err) = match action {
                    DogStatsDActionOwned::Count(metric, value, tags) => {
                        do_send(client.count_with_tags(metric.as_ref(), value), &tags)
                    }
                    DogStatsDActionOwned::Distribution(metric, value, tags) => {
                        do_send(client.distribution_with_tags(metric.as_ref(), value), &tags)
                    }
                    DogStatsDActionOwned::Gauge(metric, value, tags) => {
                        do_send(client.gauge_with_tags(metric.as_ref(), value), &tags)
                    }
                    DogStatsDActionOwned::Histogram(metric, value, tags) => {
                        do_send(client.histogram_with_tags(metric.as_ref(), value), &tags)
                    }
                    DogStatsDActionOwned::Set(metric, value, tags) => {
                        do_send(client.set_with_tags(metric.as_ref(), value), &tags)
                    }
                } {
                    error!(?err, "Error while sending metric");
                }
            }
        }
    }

    /// Send a vector of DogStatsDAction, this is the same as `send_owned` except it only borrows
    /// the provided values.See the docs for DogStatsDActionOwned for details.
    pub fn send<'a, T: AsRef<str>, V: IntoIterator<Item = &'a Tag>>(
        &self,
        actions: Vec<DogStatsDAction<'a, T, V>>,
    ) {
        let client_opt = match self.get_or_init_client() {
            Ok(client) => client,
            Err(e) => {
                error!(?e, "Failed to get client");
                return;
            }
        };
        if let Some(client) = &*client_opt {
            for action in actions {
                if let Err(err) = match action {
                    DogStatsDAction::Count(metric, value, tags) => {
                        let metric_builder = client.count_with_tags(metric.as_ref(), value);
                        do_send(metric_builder, tags)
                    }
                    DogStatsDAction::Distribution(metric, value, tags) => {
                        do_send(client.distribution_with_tags(metric.as_ref(), value), tags)
                    }
                    DogStatsDAction::Gauge(metric, value, tags) => {
                        do_send(client.gauge_with_tags(metric.as_ref(), value), tags)
                    }
                    DogStatsDAction::Histogram(metric, value, tags) => {
                        do_send(client.histogram_with_tags(metric.as_ref(), value), tags)
                    }
                    DogStatsDAction::Set(metric, value, tags) => {
                        do_send(client.set_with_tags(metric.as_ref(), value), tags)
                    }
                } {
                    error!(?err, "Error while sending metric");
                }
            }
        }
    }

    fn get_or_init_client(&self) -> anyhow::Result<Arc<Option<StatsdClient>>> {
        if let Some(endpoint) = &self.endpoint {
            let mut client_guard = self
                .client
                .lock()
                .map_err(|e| anyhow!("Failed to acquire dogstatsd client lock: {e}"))?;
            return if client_guard.is_some() {
                Ok(client_guard.clone())
            } else {
                let client = Arc::new(Some(create_client(endpoint)?));
                *client_guard = client.clone();
                Ok(client)
            };
        }

        Ok(None.into())
    }
}

fn do_send<'m, 't, T, V: IntoIterator<Item = &'t Tag>>(
    mut builder: MetricBuilder<'m, '_, T>,
    tags: V,
) -> anyhow::Result<()>
where
    T: Metric + From<String>,
    't: 'm,
{
    let mut tags_iter = tags.into_iter();
    let mut tag_opt = tags_iter.next();
    #[allow(clippy::unwrap_used)]
    while tag_opt.is_some() {
        builder = builder.with_tag_value(tag_opt.unwrap().as_ref());
        tag_opt = tags_iter.next();
    }
    builder.try_send()?;
    Ok(())
}

fn create_client(endpoint: &Endpoint) -> anyhow::Result<StatsdClient> {
    match endpoint.url.scheme_str() {
        #[cfg(unix)]
        Some("unix") => {
            let socket = UnixDatagram::unbound()
                .map_err(|e| anyhow!("failed to make unbound unix port: {}", e))?;
            socket
                .set_nonblocking(true)
                .map_err(|e| anyhow!("failed to set socket to nonblocking: {}", e))?;
            let sink = QueuingMetricSink::with_capacity(
                UnixMetricSink::from(
                    socket_path_from_uri(&endpoint.url)
                        .map_err(|e| anyhow!("failed to build socket path from uri: {}", e))?,
                    socket,
                ),
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

            let socket = if server_address.is_ipv4() {
                UdpSocket::bind("0.0.0.0:0")
                    .map_err(|e| anyhow!("failed to bind to 0.0.0.0:0: {}", e))?
            } else {
                UdpSocket::bind("[::]:0").map_err(|e| anyhow!("failed to bind to [::]:0: {}", e))?
            };
            socket.set_nonblocking(true)?;

            let sink = QueuingMetricSink::with_capacity(
                UdpMetricSink::from((host, port), socket)
                    .map_err(|e| anyhow!("failed to build UdpMetricSink: {}", e))?,
                QUEUE_SIZE,
            );

            Ok(StatsdClient::from_sink("", sink))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::DogStatsDAction::{Count, Distribution, Gauge, Histogram, Set};
    use crate::{create_client, new, DogStatsDActionOwned};
    #[cfg(unix)]
    use http::Uri;
    #[cfg(unix)]
    use libdd_common::connector::uds::socket_path_to_uri;
    use libdd_common::{tag, Endpoint};
    use std::net;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flusher() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

        let flusher = new(Endpoint::from_slice(
            socket.local_addr().unwrap().to_string().as_str(),
        ))
        .unwrap();
        flusher.send(vec![
            Count("test_count", 3, &vec![tag!("foo", "bar")]),
            Count("test_neg_count", -2, &vec![]),
            Distribution("test_distribution", 4.2, &vec![]),
            Gauge("test_gauge", 7.6, &vec![]),
            Histogram("test_histogram", 8.0, &vec![]),
            Set("test_set", 9, &vec![tag!("the", "end")]),
            Set("test_neg_set", -1, &vec![]),
        ]);

        fn read(socket: &net::UdpSocket) -> String {
            let mut buf = [0; 100];
            socket.recv(&mut buf).expect("No data");
            let datagram = String::from_utf8_lossy(buf.strip_suffix(&[0]).unwrap());
            datagram.trim_matches(char::from(0)).to_string()
        }

        assert_eq!("test_count:3|c|#foo:bar", read(&socket));
        assert_eq!("test_neg_count:-2|c", read(&socket));
        assert_eq!("test_distribution:4.2|d", read(&socket));
        assert_eq!("test_gauge:7.6|g", read(&socket));
        assert_eq!("test_histogram:8|h", read(&socket));
        assert_eq!("test_set:9|s|#the:end", read(&socket));
        assert_eq!("test_neg_set:-1|s", read(&socket));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_create_client_udp() {
        let res = create_client(&Endpoint::default());
        assert!(res.is_err());
        assert_eq!("invalid host", res.unwrap_err().to_string().as_str());

        let res = create_client(&Endpoint::from_slice("localhost:99999"));
        assert!(res.is_err());
        assert_eq!("invalid port", res.unwrap_err().to_string().as_str());

        let res = create_client(&Endpoint::from_slice("localhost:80"));
        assert!(res.is_ok());

        let res = create_client(&Endpoint::from_slice("http://localhost:80"));
        assert!(res.is_ok());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn test_create_client_unix_domain_socket() {
        let res = create_client(&Endpoint::from_url(
            "unix://localhost:80".parse::<Uri>().unwrap(),
        ));
        assert!(res.is_err());
        assert_eq!(
            "failed to build socket path from uri: invalid url",
            res.unwrap_err().to_string().as_str()
        );

        let res = create_client(&Endpoint::from_url(
            socket_path_to_uri("/path/to/a/socket.sock".as_ref()).unwrap(),
        ));
        assert!(res.is_ok());
    }

    #[test]
    fn test_owned_sync() {
        // This test ensures that if a new variant is added to either `DogStatsDActionOwned` or
        // `DogStatsDAction` this test will NOT COMPILE to act as a reminder that BOTH locations
        // must be updated.
        let owned_act = DogStatsDActionOwned::Count("test".to_string(), 1, vec![]);
        match owned_act {
            DogStatsDActionOwned::Count(_, _, _) => {}
            DogStatsDActionOwned::Distribution(_, _, _) => {}
            DogStatsDActionOwned::Gauge(_, _, _) => {}
            DogStatsDActionOwned::Histogram(_, _, _) => {}
            DogStatsDActionOwned::Set(_, _, _) => {}
        }

        let act = Count("test".to_string(), 1, vec![]);
        match act {
            Count(_, _, _) => {}
            Distribution(_, _, _) => {}
            Gauge(_, _, _) => {}
            Histogram(_, _, _) => {}
            Set(_, _, _) => {}
        }
        // TODO: when std::mem::variant_count is in stable we can do this instead
        // assert_eq!(
        //     std::mem::variant_count::<DogStatsDActionOwned>(),
        //     std::mem::variant_count::<DogStatsDAction<String, Vec<&Tag>>>(),
        //     "DogStatsDActionOwned and DogStatsDAction should have the same number of variants,
        // did you forget to update one?", );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_thread_safety() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let endpoint = Endpoint::from_slice(socket.local_addr().unwrap().to_string().as_str());
        let flusher = Arc::new(new(endpoint.clone()).unwrap());

        {
            let client = flusher
                .client
                .lock()
                .expect("failed to obtain lock on client");
            assert!(client.is_none());
        }

        let tasks: Vec<_> = (0..10)
            .map(|_| {
                let flusher_clone = Arc::clone(&flusher);
                tokio::spawn(async move {
                    flusher_clone.send(vec![
                        Count("test_count", 3, &vec![tag!("foo", "bar")]),
                        Count("test_neg_count", -2, &vec![]),
                        Distribution("test_distribution", 4.2, &vec![]),
                        Gauge("test_gauge", 7.6, &vec![]),
                        Histogram("test_histogram", 8.0, &vec![]),
                        Set("test_set", 9, &vec![tag!("the", "end")]),
                        Set("test_neg_set", -1, &vec![]),
                    ]);

                    let client = flusher_clone
                        .client
                        .lock()
                        .expect("failed to obtain lock on client within send thread");
                    assert!(client.is_some());
                })
            })
            .collect();

        for task in tasks {
            task.await.unwrap();
        }
    }
}
