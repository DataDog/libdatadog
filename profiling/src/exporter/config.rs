// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::exporter::Uri;
use ddcommon_net::compat::Endpoint;
use ddcommon_net::dep::http;
use std::borrow::Cow;
use std::str::FromStr;

pub trait ProfilingEndpoint {
    fn profiling_agentless(
        site: impl AsRef<str>,
        api_key: Cow<'static, str>,
    ) -> http::Result<Endpoint>;
    fn profiling_agent<U: TryInto<Uri>>(uri: U) -> Result<Endpoint, http::Error>
    where
        http::Error: From<U::Error>;
}

impl ProfilingEndpoint for Endpoint {
    fn profiling_agentless(
        site: impl AsRef<str>,
        api_key: Cow<'static, str>,
    ) -> http::Result<Endpoint> {
        let intake_url = format!("https://intake.profile.{}/api/v2/profile", site.as_ref());
        Ok(Self {
            url: Uri::try_from(intake_url)?,
            api_key: Some(api_key),
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
}
