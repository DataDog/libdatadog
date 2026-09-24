// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::anyhow;
use cadence::UdpMetricSink;
#[cfg(unix)]
use cadence::{MetricSink, SinkStats, UnixMetricSink};
#[cfg(unix)]
use libdd_common::connector::uds::socket_path_from_uri;
use libdd_common::Endpoint;
use std::net::{ToSocketAddrs, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

/// Keeps the destination pinned for as long as the sink can send through its `/proc/self/fd` path.
/// A queued worker can outlive the client, so the descriptor must belong to the sink itself.
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct PinnedUnixSink {
    inner: UnixMetricSink,
    _pin: Option<std::os::fd::OwnedFd>,
}

#[cfg(unix)]
impl MetricSink for PinnedUnixSink {
    fn emit(&self, metric: &str) -> std::io::Result<usize> {
        self.inner.emit(metric)
    }

    fn stats(&self) -> SinkStats {
        self.inner.stats()
    }
}

/// Build a Unix datagram sink, pinning the destination when path restrictions are enabled.
#[cfg(unix)]
pub(super) fn create_unix_sink(endpoint: &Endpoint) -> anyhow::Result<PinnedUnixSink> {
    let socket =
        UnixDatagram::unbound().map_err(|e| anyhow!("failed to make unbound unix port: {}", e))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| anyhow!("failed to set socket to nonblocking: {}", e))?;
    let path = socket_path_from_uri(&endpoint.url)
        .map_err(|e| anyhow!("failed to build socket path from uri: {}", e))?;

    if libdd_common::unix_utils::worker_file_outputs_restricted() {
        let (pinned, connect_path) = libdd_common::unix_utils::constrained_unix_socket_fd(&path)
            .map_err(|e| anyhow!("failed to resolve dogstatsd socket path: {}", e))?;
        return Ok(PinnedUnixSink {
            inner: UnixMetricSink::from(connect_path, socket),
            _pin: Some(pinned),
        });
    }
    Ok(PinnedUnixSink {
        inner: UnixMetricSink::from(path, socket),
        _pin: None,
    })
}

pub(super) fn create_udp_sink(endpoint: &Endpoint) -> anyhow::Result<UdpMetricSink> {
    let host = endpoint.url.host().ok_or(anyhow!("invalid host"))?;
    let port = endpoint.url.port().ok_or(anyhow!("invalid port"))?.as_u16();

    let server_address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or(anyhow!("invalid address"))?;

    let socket = if server_address.is_ipv4() {
        UdpSocket::bind("0.0.0.0:0").map_err(|e| anyhow!("failed to bind to 0.0.0.0:0: {}", e))?
    } else {
        UdpSocket::bind("[::]:0").map_err(|e| anyhow!("failed to bind to [::]:0: {}", e))?
    };
    socket.set_nonblocking(true)?;

    UdpMetricSink::from((host, port), socket)
        .map_err(|e| anyhow!("failed to build UdpMetricSink: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use http::Uri;
    #[cfg(unix)]
    use libdd_common::connector::uds::socket_path_to_uri;
    use libdd_common::Endpoint;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_create_udp_sink() {
        let res = create_udp_sink(&Endpoint::default());
        assert!(res.is_err());
        assert_eq!("invalid host", res.unwrap_err().to_string().as_str());

        let res = create_udp_sink(&Endpoint::from_slice("localhost:99999"));
        assert!(res.is_err());
        assert_eq!("invalid port", res.unwrap_err().to_string().as_str());

        let res = create_udp_sink(&Endpoint::from_slice("localhost:80"));
        assert!(res.is_ok());

        let res = create_udp_sink(&Endpoint::from_slice("http://localhost:80"));
        assert!(res.is_ok());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn test_create_unix_sink() {
        let res = create_unix_sink(&Endpoint::from_url(
            "unix://localhost:80".parse::<Uri>().unwrap(),
        ));
        assert!(res.is_err());
        assert_eq!(
            "failed to build socket path from uri: invalid url",
            res.unwrap_err().to_string().as_str()
        );

        let res = create_unix_sink(&Endpoint::from_url(
            socket_path_to_uri("/path/to/a/socket.sock".as_ref()).unwrap(),
        ));
        assert!(res.is_ok());
    }
}
