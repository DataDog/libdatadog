use anyhow::anyhow;
use cadence::prelude::*;
use cadence::{Metric, MetricBuilder, QueuingMetricSink, StatsdClient};
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
#[cfg(feature = "shared-runtime")]
use libdd_shared_runtime::{SharedRuntime, WorkerHandle};
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use tracing::error;

use crate::action::{DogStatsDAction, DogStatsDActionOwned};

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

mod sink;

#[cfg(feature = "shared-runtime")]
mod shared_runtime_sink;

/// A dogstatsd-client that flushes stats to a given endpoint.
#[derive(Debug, Default)]
pub struct Client {
    pub(crate) client: Mutex<Arc<Option<StatsdClient>>>,
    pub(crate) endpoint: Option<Endpoint>,
}

/// Build a new flusher instance pointed at the provided endpoint.
/// Returns error if the provided endpoint is not valid.
pub fn new(endpoint: Endpoint) -> anyhow::Result<Client> {
    Ok(Client::new(endpoint))
}

impl Client {
    /// Build a new flusher instance pointed at the provided endpoint.
    /// The sink and underlying thread are initialized lazily when the first metric is sent.
    pub fn new(endpoint: Endpoint) -> Client {
        // defer initialization of the client until the first metric is sent and we definitely know the
        // client is going to be used to communicate with the endpoint.
        Client {
            endpoint: Some(endpoint),
            ..Default::default()
        }
    }

    /// Create a [`Client`] backed by a [`MetricSinkWorker`] running on the
    /// provided [`SharedRuntime`].
    ///
    /// Returns the client and a [`WorkerHandle`] that can be used to stop the
    /// worker independently of the runtime.
    ///
    /// # Errors
    /// Returns an error if the endpoint is invalid or the worker cannot be spawned.
    #[cfg(feature = "shared-runtime")]
    pub fn new_with_shared_runtime(
        endpoint: Endpoint,
        runtime: &SharedRuntime,
    ) -> anyhow::Result<(Client, WorkerHandle)> {
        let (sink, handle) = shared_runtime_sink::create_shared_runtime_sink(&endpoint, runtime)?;

        let statsd_client = StatsdClient::from_sink("", sink);

        let client = Client {
            client: Mutex::new(Arc::new(Some(statsd_client))),
            endpoint: None,
        };

        Ok((client, handle))
    }

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
    let sink = match endpoint.url.scheme_str() {
        Some("unix") => {
            QueuingMetricSink::with_capacity(sink::create_unix_sink(endpoint)?, QUEUE_SIZE)
        }
        _ => QueuingMetricSink::with_capacity(sink::create_udp_sink(endpoint)?, QUEUE_SIZE),
    };
    Ok(StatsdClient::from_sink("", sink))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::action::DogStatsDAction::{Count, Distribution, Gauge, Histogram, Set};
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
