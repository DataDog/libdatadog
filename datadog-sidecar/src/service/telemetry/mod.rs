// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::telemetry::enqueued_telemetry_data::EnqueuedTelemetryData;
pub use app_instance::AppInstance;
use futures::future::Shared;
use manual_future::ManualFuture;

mod app_instance;
pub mod enqueued_telemetry_data;
pub mod enqueued_telemetry_stats;

#[allow(clippy::large_enum_variant)]
#[derive(Default)]
pub(crate) enum AppOrQueue {
    #[default]
    Inactive,
    App(Shared<ManualFuture<(String, String)>>),
    Queue(EnqueuedTelemetryData),
}
