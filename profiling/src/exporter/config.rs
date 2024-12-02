// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::exporter::Uri;
use ddcommon_net2::compat::Endpoint;
use ddcommon_net2::{dep::http, http::UriExt};
use std::borrow::Cow;
use std::path::Path;
use std::str::FromStr;

pub trait EndpointExt {
    fn profiling_agentless(
        site: impl AsRef<str>,
        api_key: impl Into<Cow<'static, str>>,
    ) -> http::Result<Endpoint>;

    fn profiling_agent<U: TryInto<Uri>>(uri: U) -> Result<Endpoint, http::Error>
    where
        http::Error: From<U::Error>;

    fn profiling_file(path: impl AsRef<str>) -> http::Result<Endpoint>;
}

impl EndpointExt for Endpoint {
    fn profiling_agentless(
        site: impl AsRef<str>,
        api_key: impl Into<Cow<'static, str>>,
    ) -> http::Result<Endpoint> {
        let intake_url = format!("https://intake.profile.{}/api/v2/profile", site.as_ref());
        Ok(Self {
            url: Uri::try_from(intake_url)?,
            api_key: Some(api_key.into()),
            timeout_ms: 0,
            test_token: None,
        })
    }

    fn profiling_agent<U: TryInto<Uri>>(uri: U) -> Result<Endpoint, http::Error>
    where
        http::Error: From<U::Error>,
    {
        let mut parts = uri.try_into()?.into_parts();
        parts.path_and_query = Some(http::uri::PathAndQuery::from_str("/profiling/v1/input")?);
        let url = Uri::from_parts(parts)?;
        Ok(Self {
            url,
            api_key: None,
            timeout_ms: 0,
            test_token: None,
        })
    }

    fn profiling_file(path: impl AsRef<str>) -> http::Result<Endpoint> {
        let raw_url = format!("file://{}", path.as_ref());
        let url = Uri::from_str(&raw_url)?;
        Ok(Self::from(url))
    }
}

#[cfg(unix)]
/// Creates a new Uri, with the `unix` scheme, and the path to the socket
/// encoded as a hex string, to prevent special characters in the url authority
pub fn try_socket_path_to_uri(path: &Path) -> http::Result<Uri> {
    Uri::from_path("unix", path)
}

#[cfg(windows)]
/// Windows Named Pipe
/// https://docs.microsoft.com/en-us/windows/win32/ipc/named-pipes
///
/// The form a windows named pipe path is either local to the computer:
/// \\.\pipe\pipename
/// or targeting a remote server
/// \\ServerName\pipe\pipename
///
/// Build a URI from a Path representing a named pipe
/// `path` - named pipe path. ex: \\.\pipe\pipename
pub fn try_named_pipe_path_to_uri(path: &Path) -> http::Result<Uri> {
    Uri::from_path("windows", path)
}
