// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::action::{DogStatsDAction, DogStatsDActionOwned};
use cadence::prelude::*;
use cadence::{Metric, MetricBuilder, QueuingMetricSink, StatsdClient};
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
#[cfg(feature = "shared-runtime")]
use libdd_shared_runtime::{SharedRuntime, WorkerHandle};
use sink::create_udp_sink;
#[cfg(unix)]
use sink::create_unix_sink;
use std::sync::{Arc, OnceLock};
use tracing::error;

/// Provides transport sink which can be wrapped in buffered sink
mod sink;

/// Provides a buffered sink running on a [libdd_shared_runtime::SharedRuntime]
#[cfg(feature = "shared-runtime")]
mod shared_runtime_sink;

// Queue with a maximum capacity of 32K elements
const QUEUE_SIZE: usize = 32 * 1024;

/// A dogstatsd-client that flushes stats to a given endpoint.
///
/// This client can be cloned and shared between threads.
#[derive(Debug, Default, Clone)]
pub struct DogStatsDClient {
    inner: Arc<InnerClient>,
}

/// Inner struct of [DogStatsDClient] to be wrapped in [Arc].
#[derive(Debug, Default)]
struct InnerClient {
    client: OnceLock<StatsdClient>,
    endpoint: Endpoint,
}

impl DogStatsDClient {
    /// Build a new client instance pointed at the provided endpoint.
    /// Returns error if the provided endpoint is not valid.
    pub fn new(endpoint: Endpoint) -> anyhow::Result<Self> {
        // defer initialization of the client until the first metric is sent and we definitely know
        // the client is going to be used to communicate with the endpoint.
        Ok(Self {
            inner: Arc::new(InnerClient {
                endpoint,
                ..Default::default()
            }),
        })
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
        runtime: &impl SharedRuntime,
    ) -> anyhow::Result<(Self, WorkerHandle)> {
        let (sink, handle) = shared_runtime_sink::create_shared_runtime_sink(&endpoint, runtime)?;

        let client = Self {
            inner: Arc::new(InnerClient {
                client: OnceLock::from(StatsdClient::from_sink("", sink)),
                endpoint,
            }),
        };

        Ok((client, handle))
    }

    /// Send a vector of DogStatsDActionOwned, this is the same as `send` except it uses the
    /// "owned" version of DogStatsDAction. See the docs for DogStatsDActionOwned for details.
    pub fn send_owned(&self, actions: Vec<DogStatsDActionOwned>) {
        match self.get_or_init_client() {
            Ok(client) => {
                for action in actions {
                    if let Err(err) = match action {
                        DogStatsDActionOwned::Count(metric, value, tags) => {
                            Self::do_send(client.count_with_tags(metric.as_ref(), value), &tags)
                        }
                        DogStatsDActionOwned::Distribution(metric, value, tags) => Self::do_send(
                            client.distribution_with_tags(metric.as_ref(), value),
                            &tags,
                        ),
                        DogStatsDActionOwned::Gauge(metric, value, tags) => {
                            Self::do_send(client.gauge_with_tags(metric.as_ref(), value), &tags)
                        }
                        DogStatsDActionOwned::Histogram(metric, value, tags) => {
                            Self::do_send(client.histogram_with_tags(metric.as_ref(), value), &tags)
                        }
                        DogStatsDActionOwned::Set(metric, value, tags) => {
                            Self::do_send(client.set_with_tags(metric.as_ref(), value), &tags)
                        }
                    } {
                        error!(?err, "Error while sending metric");
                    }
                }
            }
            Err(e) => {
                error!("Failed to acquire dogstatsd client lock: {e}");
            }
        };
    }

    /// Send a vector of DogStatsDAction, this is the same as `send_owned` except it only borrows
    /// the provided values. See the docs for DogStatsDActionOwned for details.
    pub fn send<'a, T: AsRef<str>, V: IntoIterator<Item = &'a Tag>>(
        &self,
        actions: Vec<DogStatsDAction<'a, T, V>>,
    ) {
        match self.get_or_init_client() {
            Ok(client) => {
                for action in actions {
                    if let Err(err) = match action {
                        DogStatsDAction::Count(metric, value, tags) => {
                            let metric_builder = client.count_with_tags(metric.as_ref(), value);
                            Self::do_send(metric_builder, tags)
                        }
                        DogStatsDAction::Distribution(metric, value, tags) => Self::do_send(
                            client.distribution_with_tags(metric.as_ref(), value),
                            tags,
                        ),
                        DogStatsDAction::Gauge(metric, value, tags) => {
                            Self::do_send(client.gauge_with_tags(metric.as_ref(), value), tags)
                        }
                        DogStatsDAction::Histogram(metric, value, tags) => {
                            Self::do_send(client.histogram_with_tags(metric.as_ref(), value), tags)
                        }
                        DogStatsDAction::Set(metric, value, tags) => {
                            Self::do_send(client.set_with_tags(metric.as_ref(), value), tags)
                        }
                    } {
                        error!(?err, "Error while sending metric");
                    }
                }
            }
            Err(e) => {
                error!(?e, "Failed to get client");
            }
        }
    }

    fn get_or_init_client(&self) -> anyhow::Result<&StatsdClient> {
        match self.inner.client.get() {
            Some(client) => Ok(client),
            None => {
                let client = Self::create_client(&self.inner.endpoint)?;
                Ok(self.inner.client.get_or_init(|| client))
            }
        }
    }

    fn create_client(endpoint: &Endpoint) -> anyhow::Result<StatsdClient> {
        match endpoint.url.scheme_str() {
            #[cfg(unix)]
            Some("unix") => Ok(StatsdClient::from_sink(
                "",
                QueuingMetricSink::with_capacity(create_unix_sink(endpoint)?, QUEUE_SIZE),
            )),
            _ => Ok(StatsdClient::from_sink(
                "",
                QueuingMetricSink::with_capacity(create_udp_sink(endpoint)?, QUEUE_SIZE),
            )),
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
        for tag in tags {
            builder = builder.with_tag_value(tag.as_ref());
        }
        builder.try_send()?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::action::DogStatsDAction::{Count, Distribution, Gauge, Histogram, Set};
    use libdd_common::{tag, Endpoint};
    use std::net;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flusher() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

        let flusher = DogStatsDClient::new(Endpoint::from_slice(
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

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_thread_safety() {
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let endpoint = Endpoint::from_slice(socket.local_addr().unwrap().to_string().as_str());
        let flusher = Arc::new(DogStatsDClient::new(endpoint.clone()).unwrap());

        {
            assert!(flusher.inner.client.get().is_none());
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

                    assert!(flusher_clone.inner.client.get().is_some());
                })
            })
            .collect();

        for task in tasks {
            task.await.unwrap();
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "shared-runtime")]
    fn test_shared_runtime_flusher() {
        use libdd_shared_runtime::{BasicRuntime, BlockingRuntime, SharedRuntime};

        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

        let runtime = BasicRuntime::new().unwrap();

        let (flusher, _handle) = DogStatsDClient::new_with_shared_runtime(
            Endpoint::from_slice(socket.local_addr().unwrap().to_string().as_str()),
            &runtime,
        )
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

        runtime
            .block_on(runtime.shutdown_async())
            .expect("Failed to shutdown runtime");

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
}
