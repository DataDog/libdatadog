// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidUrl,
    OperationTimedOut,
    UnixSocketUnsupported,
    CannotEstablishTlsConnection,
    WindowsNamedPipeUnsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "invalid url",
            Self::OperationTimedOut => "operation timed out",
            Self::UnixSocketUnsupported => "unix sockets unsuported on windows",
            Self::CannotEstablishTlsConnection => {
                "cannot establish requested secure TLS connection"
            }
            Self::WindowsNamedPipeUnsupported => "windows named pipes unsupported",
        })
    }
}

impl error::Error for Error {}
