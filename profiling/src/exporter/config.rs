// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use http::Uri;
use std::borrow::Cow;
use std::str::FromStr;

use ddcommon_net1::Endpoint;

#[cfg(unix)]
use ddcommon_net1::connector::uds;

#[cfg(windows)]
use ddcommon_net1::connector::named_pipe;

/// Creates an Endpoint for talking to the Datadog agent.
///
/// # Arguments
/// * `base_url` - has protocol, host, and port e.g. http://localhost:8126/
pub fn agent(base_url: Uri) -> anyhow::Result<Endpoint> {
    let mut parts = base_url.into_parts();
    let p_q = match parts.path_and_query {
        None => None,
        Some(pq) => {
            let path = pq.path();
            let path = path.strip_suffix('/').unwrap_or(path);
            Some(format!("{path}/profiling/v1/input").parse()?)
        }
    };
    parts.path_and_query = p_q;
    let url = Uri::from_parts(parts)?;
    Ok(Endpoint::from_url(url))
}

/// Creates an Endpoint for talking to the Datadog agent though a unix socket.
///
/// # Arguments
/// * `socket_path` - file system path to the socket
#[cfg(unix)]
pub fn agent_uds(path: &std::path::Path) -> anyhow::Result<Endpoint> {
    let base_url = uds::socket_path_to_uri(path)?;
    agent(base_url)
}

/// Creates an Endpoint for talking to the Datadog agent though a windows named pipe.
///
/// # Arguments
/// * `path` - file system path to the named pipe
#[cfg(windows)]
pub fn agent_named_pipe(path: &std::path::Path) -> anyhow::Result<Endpoint> {
    let base_url = named_pipe::named_pipe_path_to_uri(path)?;
    agent(base_url)
}

/// Creates an Endpoint for talking to Datadog intake without using the agent.
/// This is an experimental feature.
///
/// # Arguments
/// * `site` - e.g. "datadoghq.com".
/// * `api_key`
pub fn agentless<AsStrRef: AsRef<str>, IntoCow: Into<Cow<'static, str>>>(
    site: AsStrRef,
    api_key: IntoCow,
) -> anyhow::Result<Endpoint> {
    let intake_url: String = format!("https://intake.profile.{}/api/v2/profile", site.as_ref());

    Ok(Endpoint {
        url: Uri::from_str(intake_url.as_str())?,
        api_key: Some(api_key.into()),
        ..Default::default()
    })
}

pub fn file(path: impl AsRef<str>) -> anyhow::Result<Endpoint> {
    let url: String = format!("file://{}", path.as_ref());
    Ok(Endpoint::from_slice(&url))
}
