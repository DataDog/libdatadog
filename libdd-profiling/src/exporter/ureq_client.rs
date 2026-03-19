// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::sync::Arc;

use anyhow::Context;
use http::StatusCode;
use libdd_common::{ResolvedEndpoint, ResolvedEndpointKind};
use ureq::unversioned::transport::Buffers;

use super::transport::PreparedRequest;

#[derive(Clone)]
pub(crate) struct UreqClient {
    agent: ureq::Agent,
    request_url: String,
}

impl UreqClient {
    pub(crate) fn new(
        resolved: ResolvedEndpoint,
        tls_config: Option<ureq::tls::TlsConfig>,
    ) -> anyhow::Result<Self> {
        let _ = resolved.use_system_resolver;

        let config = base_config_builder(resolved.timeout, tls_config).build();

        match resolved.kind {
            ResolvedEndpointKind::Tcp => Ok(Self {
                agent: config.new_agent(),
                request_url: resolved.request_url,
            }),
            #[cfg(unix)]
            ResolvedEndpointKind::UnixSocket { path } => Ok(Self {
                agent: ureq::Agent::with_parts(config, UnixConnector::new(path), UnixResolver),
                request_url: resolved.request_url,
            }),
            #[cfg(windows)]
            ResolvedEndpointKind::WindowsNamedPipe { .. } => {
                anyhow::bail!("Windows named pipes are not supported by the ureq-based exporter")
            }
        }
    }

    pub(crate) fn send(&self, request: PreparedRequest) -> anyhow::Result<StatusCode> {
        let mut builder = self.agent.post(&self.request_url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }

        let response = builder
            .send(request.body.as_slice())
            .with_context(|| format!("failed to send profiling request to {}", self.request_url))?;
        Ok(response.status())
    }
}

fn base_config_builder(
    timeout: std::time::Duration,
    tls_config: Option<ureq::tls::TlsConfig>,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    let builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout))
        .timeout_send_request(Some(timeout))
        .timeout_send_body(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_recv_body(Some(timeout));

    match tls_config {
        Some(tls_config) => builder.tls_config(tls_config),
        None => builder,
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct UnixResolver;

#[cfg(unix)]
impl ureq::unversioned::resolver::Resolver for UnixResolver {
    fn resolve(
        &self,
        _uri: &http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = ureq::unversioned::resolver::ArrayVec::from_fn(|_| {
            std::net::SocketAddr::from(([127, 0, 0, 1], 0))
        });
        addrs.push(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
        Ok(addrs)
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct UnixConnector {
    path: Arc<std::path::PathBuf>,
}

#[cfg(unix)]
impl UnixConnector {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path: Arc::new(path),
        }
    }
}

#[cfg(unix)]
impl fmt::Debug for UnixConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixConnector")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(unix)]
impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for UnixConnector
{
    type Out = ureq::unversioned::transport::Either<In, UnixTransport>;

    fn connect(
        &self,
        _details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        if chained.is_some() {
            return Ok(chained.map(ureq::unversioned::transport::Either::A));
        }

        let stream = std::os::unix::net::UnixStream::connect(&*self.path).map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to connect to UDS {}: {err}", self.path.display()),
            )
        })?;

        let buffers = ureq::unversioned::transport::LazyBuffers::new(128 * 1024, 128 * 1024);
        Ok(Some(ureq::unversioned::transport::Either::B(
            UnixTransport::new(stream, buffers),
        )))
    }
}

#[cfg(unix)]
struct UnixTransport {
    stream: std::os::unix::net::UnixStream,
    buffers: ureq::unversioned::transport::LazyBuffers,
    timeout_write: Option<ureq::unversioned::transport::time::Duration>,
    timeout_read: Option<ureq::unversioned::transport::time::Duration>,
}

#[cfg(unix)]
impl UnixTransport {
    fn new(
        stream: std::os::unix::net::UnixStream,
        buffers: ureq::unversioned::transport::LazyBuffers,
    ) -> Self {
        Self {
            stream,
            buffers,
            timeout_read: None,
            timeout_write: None,
        }
    }
}

#[cfg(unix)]
impl ureq::unversioned::transport::Transport for UnixTransport {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        &mut self.buffers
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        use std::io::Write;

        maybe_update_timeout(
            timeout,
            &mut self.timeout_write,
            &self.stream,
            std::os::unix::net::UnixStream::set_write_timeout,
        )?;

        let output = &self.buffers.output()[..amount];
        match self.stream.write_all(output) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                Err(ureq::Error::Timeout(timeout.reason))
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                Err(ureq::Error::Timeout(timeout.reason))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        use std::io::Read;

        maybe_update_timeout(
            timeout,
            &mut self.timeout_read,
            &self.stream,
            std::os::unix::net::UnixStream::set_read_timeout,
        )?;

        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(amount) => Ok(amount),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                Err(ureq::Error::Timeout(timeout.reason))
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                Err(ureq::Error::Timeout(timeout.reason))
            }
            Err(err) => Err(err.into()),
        }?;
        self.buffers.input_appended(amount);

        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        probe_unix_stream(&mut self.stream).unwrap_or(false)
    }
}

#[cfg(unix)]
fn maybe_update_timeout(
    timeout: ureq::unversioned::transport::NextTimeout,
    previous: &mut Option<ureq::unversioned::transport::time::Duration>,
    stream: &std::os::unix::net::UnixStream,
    f: impl Fn(&std::os::unix::net::UnixStream, Option<std::time::Duration>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let maybe_timeout = timeout.not_zero();

    if maybe_timeout != *previous {
        (f)(stream, maybe_timeout.map(|t| *t))?;
        *previous = maybe_timeout;
    }

    Ok(())
}

#[cfg(unix)]
fn probe_unix_stream(stream: &mut std::os::unix::net::UnixStream) -> Result<bool, ureq::Error> {
    use std::io::Read;

    stream.set_nonblocking(true)?;

    let mut buf = [0];
    let result = match stream.read(&mut buf) {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Ok(false),
    };

    stream.set_nonblocking(false)?;
    result
}

#[cfg(unix)]
impl fmt::Debug for UnixTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixTransport").finish()
    }
}
