// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! health_metrics holds data to emit info about the health of the data-pipeline

pub(crate) const STAT_SEND_TRACES: &str = "datadog.libdatadog.send.traces";
pub(crate) const STAT_SEND_TRACES_ERRORS: &str = "datadog.libdatadog.send.traces.errors";
pub(crate) const STAT_DESER_TRACES: &str = "datadog.libdatadog.deser_traces";
pub(crate) const STAT_DESER_TRACES_ERRORS: &str = "datadog.libdatadog.deser_traces.errors";
#[allow(dead_code)] // TODO (APMSP-1584) Add support for health metrics when using trace utils
pub(crate) const STAT_SER_TRACES_ERRORS: &str = "datadog.libdatadog.ser_traces.errors";

pub(crate) enum HealthMetric {
    Count(&'static str, i64),
}
