// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tracing::{debug, error, info, warn};

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

/// The `DogStatsDAction` enum gathers the metric types that can be sent to the DogStatsD server.
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction {
    Count(String, i64, Vec<Tag>),
    Distribution(String, f64, Vec<Tag>),
    Gauge(String, f64, Vec<Tag>),
    Histogram(String, f64, Vec<Tag>),
    // Cadence only support i64 type as value
    // but Golang implementation uses string (https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230)
    // and PHP implementation uses float or string (https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251)
    Set(String, i64, Vec<Tag>),
}

#[derive(Default)]
pub struct Flusher {
    endpoint: Option<Endpoint>,
    client: Option<StatsdClient>,
}

impl Flusher {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        self.client = None;
        self.endpoint = match endpoint.api_key {
            Some(_) => {
                info!("DogStatsD is not available in agentless mode");
                None
            }
            None => {
                debug!("Updating DogStatsD endpoint to {}", endpoint.url);
                Some(endpoint)
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
                DogStatsDAction::Set(metric, value, tags) => {
                    do_send(client.set_with_tags(metric.as_str(), value), &tags)
                }
            } {
                error!("Error while sending metric: {}", err);
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
    };
}

#[cfg(test)]
mod test {
    use crate::dogstatsd::DogStatsDAction::{Count, Distribution, Gauge, Histogram, Set};
    use crate::dogstatsd::{create_client, Flusher};
    #[cfg(unix)]
    use ddcommon::connector::uds::socket_path_to_uri;
    use ddcommon::{tag, Endpoint};
    use http::Uri;
    use std::net;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flusher() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

        let mut flusher = Flusher::default();
        flusher.set_endpoint(Endpoint {
            url: socket
                .local_addr()
                .unwrap()
                .to_string()
                .as_str()
                .parse::<Uri>()
                .unwrap(),
            ..Default::default()
        });
        flusher.send(vec![
            Count("test_count".to_string(), 3, vec![tag!("foo", "bar")]),
            Count("test_neg_count".to_string(), -2, vec![]),
            Distribution("test_distribution".to_string(), 4.2, vec![]),
            Gauge("test_gauge".to_string(), 7.6, vec![]),
            Histogram("test_histogram".to_string(), 8.0, vec![]),
            Set("test_set".to_string(), 9, vec![tag!("the", "end")]),
            Set("test_neg_set".to_string(), -1, vec![]),
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
        let res = create_client(None);
        assert!(res.is_err());
        assert_eq!("no endpoint set", res.unwrap_err().to_string().as_str());

        let res = create_client(Some(Endpoint::default()));
        assert!(res.is_err());
        assert_eq!("invalid host", res.unwrap_err().to_string().as_str());

        let res = create_client(Some(Endpoint {
            url: "localhost:99999".parse::<Uri>().unwrap(),
            ..Default::default()
        }));
        assert!(res.is_err());
        assert_eq!("invalid port", res.unwrap_err().to_string().as_str());

        let res = create_client(Some(Endpoint {
            url: "localhost:80".parse::<Uri>().unwrap(),
            ..Default::default()
        }));
        assert!(res.is_ok());

        let res = create_client(Some(Endpoint {
            url: "http://localhost:80".parse::<Uri>().unwrap(),
            ..Default::default()
        }));
        assert!(res.is_ok());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn test_create_client_unix_domain_socket() {
        let res = create_client(Some(Endpoint {
            url: "unix://localhost:80".parse::<Uri>().unwrap(),
            ..Default::default()
        }));
        assert!(res.is_err());
        assert_eq!("invalid url", res.unwrap_err().to_string().as_str());

        let res = create_client(Some(Endpoint {
            url: socket_path_to_uri("/path/to/a/socket.sock".as_ref()).unwrap(),
            ..Default::default()
        }));
        assert!(res.is_ok());
    }
}
