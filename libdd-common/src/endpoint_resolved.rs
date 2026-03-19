// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub kind: ResolvedEndpointKind,
    pub request_url: String,
    pub timeout: Duration,
    pub use_system_resolver: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedEndpointKind {
    Tcp,
    #[cfg(unix)]
    UnixSocket {
        path: PathBuf,
    },
    #[cfg(windows)]
    WindowsNamedPipe {
        path: PathBuf,
    },
}
