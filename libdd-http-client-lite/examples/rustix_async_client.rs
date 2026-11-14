// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Resolves and queries a local Datadog Agent with reqwless. The `rustix`
//! transport is Unix-only, so this example does nothing on other platforms.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
mod unix {
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use std::process::ExitCode;

    use embedded_nal_async::AddrType;
    use libdd_http_client_lite::{
        dns::{DnsResolver, Resolver as _},
        env::Environment,
        rustix::TcpStream,
    };
    use reqwless::{
        Error,
        client::{HttpConnection, HttpResource},
        request::Method,
    };

    const AGENT_HOST: &str = "agent.local";
    const AGENT_PORT: u16 = 8126;
    const AGENT_PATH: &str = "/info";
    const DNS_ENTRIES: &[(&str, &str)] = &[("agent.local", "127.0.0.1")];

    pub fn main() -> ExitCode {
        match run() {
            Ok(status) => {
                println!("HTTP response status: {status}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        }
    }

    fn run() -> Result<u16, String> {
        let dns = DnsResolver::new(Environment::new(DNS_ENTRIES));
        let address = dns
            .resolve(AGENT_HOST, AddrType::Either)
            .map_err(|error| format!("DNS lookup failed: {error}"))?;
        let connection = TcpStream::connect((address, AGENT_PORT).into())
            .map_err(|error| format!("TCP connection failed: {error}"))?;
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(connection),
            host: AGENT_HOST,
            base_path: "",
        };
        block_on(get_agent_info(&mut resource))
            .map_err(|error| format!("HTTP request failed: {error:?}"))
    }

    async fn get_agent_info(resource: &mut HttpResource<'_, TcpStream>) -> Result<u16, Error> {
        let mut response_buffer = [0_u8; 4_096];
        let request = resource.request(Method::GET, AGENT_PATH);
        let response = request.send(&mut response_buffer).await?;
        Ok(response.status.0)
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);

        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => core::hint::spin_loop(),
            }
        }
    }
}
