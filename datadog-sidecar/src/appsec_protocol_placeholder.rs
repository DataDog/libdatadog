// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Stand-in for the AppSec protocol of the embedding application, used unless
//! it provides one (see the `appsec-protocol` feature). It only has to satisfy
//! the contract the sidecar relies on.

use serde::{Deserialize, Serialize};

/// Connection-wide AppSec settings, sent with the session config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSecSessionConfig {}

/// A request to the AppSec client of a connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppSecRequest {}

/// The answer to an [`AppSecRequest`]. The default is returned when no
/// backend can handle the request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AppSecVerdict {
    #[default]
    Unavailable,
    Handled,
}
