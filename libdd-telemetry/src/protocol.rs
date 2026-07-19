// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(crate) const API_VERSION: &str = "v2";
pub(crate) const GENERATE_METRICS_REQUEST_TYPE: &str = "generate-metrics";
#[cfg(any(feature = "std", feature = "signal-safe"))]
pub(crate) const REQUEST_TYPE_HEADER: &str = "dd-telemetry-request-type";
#[cfg(any(feature = "std", feature = "signal-safe"))]
pub(crate) const API_VERSION_HEADER: &str = "dd-telemetry-api-version";
#[cfg(any(feature = "std", feature = "signal-safe"))]
pub(crate) const LIBRARY_LANGUAGE_HEADER: &str = "dd-client-library-language";
#[cfg(any(feature = "std", feature = "signal-safe"))]
pub(crate) const LIBRARY_VERSION_HEADER: &str = "dd-client-library-version";
