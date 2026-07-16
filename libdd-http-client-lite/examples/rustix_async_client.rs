// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    future::Future,
    pin::pin,
    process::ExitCode,
    task::{Context, Poll, Waker},
};

use libdd_http_client_lite::{
    client::{HttpConnection, HttpResource},
    dns::{DnsResolver, Resolver as _},
    env::Environment,
    request::Method,
    rustix::TcpStream,
    Error,
};

const AGENT_HOST: &str = "agent.local";
const AGENT_PORT: u16 = 8126;
const AGENT_PATH: &str = "/info";
const DNS_ENTRIES: &[(&str, &str)] = &[("agent.local", "127.0.0.1")];

fn main() -> ExitCode {
    let dns = DnsResolver::new(Environment::new(DNS_ENTRIES));
    let address = match dns.resolve(
        AGENT_HOST,
        libdd_http_client_lite::io::embedded_nal_async::AddrType::Either,
    ) {
        Ok(address) => address,
        Err(error) => {
            eprintln!("DNS lookup failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let connection = match TcpStream::connect((address, AGENT_PORT).into()) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("TCP connection failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut resource = HttpResource {
        conn: HttpConnection::Plain(connection),
        host: AGENT_HOST,
        base_path: "",
    };

    match block_on(get_agent_info(&mut resource)) {
        Ok(status) => {
            println!("HTTP response status: {status}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("HTTP request failed: {error:?}");
            ExitCode::FAILURE
        }
    }
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
            Poll::Pending => std::hint::spin_loop(),
        }
    }
}
