// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sends a plain HTTP request to a local Datadog Agent. The `rustix` transport
//! is Unix-only, so this example does nothing on other platforms.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
mod unix {
    use core::net::SocketAddr;
    use std::process::ExitCode;

    use embedded_io::{Read as _, Write as _};
    use libdd_http_client_lite::rustix::{Error, TcpStream};

    const REQUEST: &[u8] = b"GET /info HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";

    pub fn main() -> ExitCode {
        match get_agent_info() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("HTTP request failed: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn get_agent_info() -> Result<(), Error> {
        let remote = SocketAddr::from(([127, 0, 0, 1], 8126));
        let mut stream = TcpStream::connect(remote)?;
        stream.write_all(REQUEST)?;

        // A real caller owns this buffer and decides how to handle a larger response.
        let mut response = [0_u8; 4_096];
        let mut response_len = 0;
        while response_len < response.len() {
            let read = stream.read(&mut response[response_len..])?;
            if read == 0 {
                break;
            }
            response_len += read;
        }

        println!("{}", String::from_utf8_lossy(&response[..response_len]));
        Ok(())
    }
}
