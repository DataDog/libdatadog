// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::{debug, error, info};

use anyhow::anyhow;
use cadence::prelude::*;
#[cfg(unix)]
use cadence::UnixMetricSink;
use cadence::{Metric, MetricBuilder, QueuingMetricSink, StatsdClient, UdpMetricSink};
#[cfg(unix)]
use ddcommon::connector::uds::socket_path_from_uri;
use std::net::{ToSocketAddrs, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

/// The `DogStatsDActionRef` enum gathers the metric types that can be sent to the DogStatsD server.
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction<T: AsRef<str>, V: std::ops::Deref>
where
    for<'a> &'a <V as std::ops::Deref>::Target: IntoIterator<Item = &'a Tag>,
{
    // TODO: instead of AsRef<str> we can accept a marker Trait that users of this crate implement
    Count(T, i64, V),
    Distribution(T, f64, V),
    Gauge(T, f64, V),
    Histogram(T, f64, V),
    // Cadence only support i64 type as value
    // but Golang implementation uses string (https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230)
    // and PHP implementation uses float or string (https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251)
    Set(T, i64, V),
}

/// A dogstatsd-client that flushes stats to a given endpoint.
/// The default value has no address and is thus disabled, use `new_flusher` or `set_endpoint` to
/// configure an endpoint.
#[derive(Default)]
pub struct Flusher {
    client: Option<StatsdClient>,
}

pub fn new_flusher(endpoint: Endpoint) -> anyhow::Result<Flusher> {
    let mut f = Flusher::default();
    f.set_endpoint(endpoint)?;
    Ok(f)
}

impl Flusher {
    /// Set the destination for dogstatsd metrics, if an API Key is provided the client is disabled
    /// as dogstatsd is not allowed in agentless mode. Returns an error if the provided endpoint
    /// is invalid.
    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.client = match endpoint.api_key {
            Some(_) => {
                info!("DogStatsD is not available in agentless mode");
                None
            }
            None => {
                debug!("Updating DogStatsD endpoint to {}", endpoint.url);
                Some(create_client(&endpoint)?)
            }
        };
        Ok(())
    }

    pub fn send<T: AsRef<str>, V: std::ops::Deref>(&self, actions: Vec<DogStatsDAction<T, V>>)
    where
        for<'a> &'a <V as std::ops::Deref>::Target: IntoIterator<Item = &'a Tag>,
    {
        if self.client.is_none() {
            return;
        }
        let client = self.client.as_ref().unwrap();

        for action in actions {
            if let Err(err) = match action {
                DogStatsDAction::Count(metric, value, ref tags) => {
                    let metric_builder = client.count_with_tags(metric.as_ref(), value);
                    do_send(metric_builder, tags.deref())
                }
                DogStatsDAction::Distribution(metric, value, ref tags) => do_send(
                    client.distribution_with_tags(metric.as_ref(), value),
                    tags.deref(),
                ),
                DogStatsDAction::Gauge(metric, value, ref tags) => {
                    do_send(client.gauge_with_tags(metric.as_ref(), value), tags.deref())
                }
                DogStatsDAction::Histogram(metric, value, ref tags) => do_send(
                    client.histogram_with_tags(metric.as_ref(), value),
                    tags.deref(),
                ),
                DogStatsDAction::Set(metric, value, ref tags) => {
                    do_send(client.set_with_tags(metric.as_ref(), value), tags.deref())
                }
            } {
                error!("Error while sending metric: {}", err);
            }
        }
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

            let socket = if server_address.is_ipv4() {
                UdpSocket::bind("0.0.0.0:0")?
            } else {
                UdpSocket::bind("[::]:0")?
            };
            socket.set_nonblocking(true)?;

            let sink = QueuingMetricSink::with_capacity(
                UdpMetricSink::from((host, port), socket)?,
                QUEUE_SIZE,
            );

            Ok(StatsdClient::from_sink("", sink))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::DogStatsDAction::{Count, Distribution, Gauge, Histogram, Set};
    use crate::{create_client, Flusher};
    #[cfg(unix)]
    use ddcommon::connector::uds::socket_path_to_uri;
    use ddcommon::{tag, Endpoint};
    #[cfg(unix)]
    use http::Uri;
    use std::net;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flusher() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

        let mut flusher = Flusher::default();
        _ = flusher.set_endpoint(Endpoint::from_slice(
            socket.local_addr().unwrap().to_string().as_str(),
        ));
        flusher.send(vec![
            Count("test_count", 3, vec![tag!("foo", "bar")]),
            Count("test_neg_count", -2, vec![]),
            Distribution("test_distribution", 4.2, vec![]),
            Gauge("test_gauge", 7.6, vec![]),
            Histogram("test_histogram", 8.0, vec![]),
            Set("test_set", 9, vec![tag!("the", "end")]),
            Set("test_neg_set", -1, vec![]),
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
        assert_eq!("invalid url", res.unwrap_err().to_string().as_str());

        let res = create_client(&Endpoint::from_url(
            socket_path_to_uri("/path/to/a/socket.sock".as_ref()).unwrap(),
        ));
        assert!(res.is_ok());
    }
}
