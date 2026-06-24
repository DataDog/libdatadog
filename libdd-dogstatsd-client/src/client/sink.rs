use libdd_common::Endpoint;

use anyhow::anyhow;
use cadence::UdpMetricSink;
use cadence::UnixMetricSink;
#[cfg(unix)]
use libdd_common::connector::uds::socket_path_from_uri;
use std::net::{ToSocketAddrs, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

pub(crate) fn create_unix_sink(endpoint: &Endpoint) -> anyhow::Result<UnixMetricSink> {
    let socket =
        UnixDatagram::unbound().map_err(|e| anyhow!("failed to make unbound unix port: {}", e))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| anyhow!("failed to set socket to nonblocking: {}", e))?;
    Ok(UnixMetricSink::from(
        socket_path_from_uri(&endpoint.url)
            .map_err(|e| anyhow!("failed to build socket path from uri: {}", e))?,
        socket,
    ))
}

pub(crate) fn create_udp_sink(endpoint: &Endpoint) -> anyhow::Result<UdpMetricSink> {
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

    Ok(UdpMetricSink::from((host, port), socket)
        .map_err(|e| anyhow!("failed to build UdpMetricSink: {}", e))?)
}
