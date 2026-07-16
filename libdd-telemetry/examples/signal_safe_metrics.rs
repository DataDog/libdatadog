// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    pin::pin,
    process::ExitCode,
    task::{Context, Poll, Waker},
    time::UNIX_EPOCH,
};

use libdd_http_client_lite::{
    client::HttpClient,
    io::{
        embedded_io::{ErrorKind, ErrorType},
        embedded_io_async::{Read, Write},
        embedded_nal_async::{AddrType, Dns, TcpConnect},
    },
};
use libdd_telemetry::{
    data::metrics::{MetricNamespace, MetricType},
    signal_safe::{self, Application, Metric, MetricsRequest},
};

const AGENT_URL: &str = "http://127.0.0.1:8126/telemetry/proxy/api/v2/apmtelemetry";
const TAGS: &[&str] = &["component:libdd-telemetry", "runtime:signal-safe"];

fn main() -> ExitCode {
    let timestamp = UNIX_EPOCH
        .elapsed()
        .map_or(0, |duration| duration.as_secs());
    let metrics = [Metric {
        namespace: MetricNamespace::Telemetry,
        name: "signal_safe.metrics_submissions",
        timestamp,
        value: 1.0,
        tags: TAGS,
        common: false,
        kind: MetricType::Count,
        interval: 0,
    }];
    let telemetry = MetricsRequest {
        tracer_time: timestamp,
        runtime_id: "00000000-0000-0000-0000-000000000000",
        seq_id: 0,
        application: Application {
            service_name: "libdd-telemetry-signal-safe-example",
            language_name: "rust",
            language_version: "unknown",
            library_version: env!("CARGO_PKG_VERSION"),
        },
        hostname: "unknown_hostname",
        metrics: &metrics,
    };

    let tcp = StdTcpConnector;
    let dns = LoopbackDns;
    let mut client = HttpClient::new(&tcp, &dns);
    let mut body_buffer = [0_u8; 2_048];
    let mut response_buffer = [0_u8; 1_024];

    match block_on(signal_safe::send_metrics(
        &mut client,
        AGENT_URL,
        &telemetry,
        &mut body_buffer,
        &mut response_buffer,
    )) {
        Ok(status) => {
            println!("telemetry metric submitted, status={status}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("telemetry metric submission failed: {error:?}");
            ExitCode::FAILURE
        }
    }
}

struct LoopbackDns;

impl Dns for LoopbackDns {
    type Error = ErrorKind;

    async fn get_host_by_name(
        &self,
        host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        if host == "127.0.0.1" || host == "localhost" {
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        } else {
            Err(ErrorKind::AddrNotAvailable)
        }
    }

    async fn get_host_by_address(
        &self,
        addr: IpAddr,
        result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        if !addr.is_loopback() {
            return Err(ErrorKind::AddrNotAvailable);
        }
        let name = b"localhost";
        let destination = result.get_mut(..name.len()).ok_or(ErrorKind::OutOfMemory)?;
        destination.copy_from_slice(name);
        Ok(name.len())
    }
}

struct StdTcpConnector;

impl TcpConnect for StdTcpConnector {
    type Error = ErrorKind;
    type Connection<'a> = StdTcpConnection;

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, Self::Error> {
        TcpStream::connect(remote)
            .map(|stream| StdTcpConnection { stream })
            .map_err(|error| map_error_kind(error.kind()))
    }
}

struct StdTcpConnection {
    stream: TcpStream,
}

impl ErrorType for StdTcpConnection {
    type Error = ErrorKind;
}

impl Read for StdTcpConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        std::io::Read::read(&mut self.stream, buffer).map_err(|error| map_error_kind(error.kind()))
    }
}

impl Write for StdTcpConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        std::io::Write::write(&mut self.stream, buffer)
            .map_err(|error| map_error_kind(error.kind()))
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::Write::flush(&mut self.stream).map_err(|error| map_error_kind(error.kind()))
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::hint::spin_loop(),
        }
    }
}

const fn map_error_kind(kind: std::io::ErrorKind) -> ErrorKind {
    match kind {
        std::io::ErrorKind::NotFound => ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
        std::io::ErrorKind::ConnectionRefused => ErrorKind::ConnectionRefused,
        std::io::ErrorKind::ConnectionReset => ErrorKind::ConnectionReset,
        std::io::ErrorKind::ConnectionAborted => ErrorKind::ConnectionAborted,
        std::io::ErrorKind::NotConnected => ErrorKind::NotConnected,
        std::io::ErrorKind::AddrInUse => ErrorKind::AddrInUse,
        std::io::ErrorKind::AddrNotAvailable => ErrorKind::AddrNotAvailable,
        std::io::ErrorKind::BrokenPipe => ErrorKind::BrokenPipe,
        std::io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
        std::io::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
        std::io::ErrorKind::InvalidData => ErrorKind::InvalidData,
        std::io::ErrorKind::TimedOut => ErrorKind::TimedOut,
        std::io::ErrorKind::Interrupted => ErrorKind::Interrupted,
        std::io::ErrorKind::Unsupported => ErrorKind::Unsupported,
        std::io::ErrorKind::OutOfMemory => ErrorKind::OutOfMemory,
        std::io::ErrorKind::WriteZero => ErrorKind::WriteZero,
        _ => ErrorKind::Other,
    }
}
