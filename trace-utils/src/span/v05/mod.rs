// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::v05::dict::SharedDict;
use crate::span::SpanBytes;
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span {
    pub service: u32,
    pub name: u32,
    pub resource: u32,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: i32,
    pub meta: HashMap<u32, u32>,
    pub metrics: HashMap<u32, f64>,
    pub r#type: u32,
}

pub fn from_span_bytes(span: &SpanBytes, dict: &mut SharedDict) -> Result<Span> {
    Ok(Span {
        service: dict.get_or_insert(&span.service)?,
        name: dict.get_or_insert(&span.name)?,
        resource: dict.get_or_insert(&span.resource)?,
        trace_id: span.trace_id,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span.meta.iter().try_fold(
            HashMap::with_capacity(span.meta.len()),
            |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
                meta.insert(dict.get_or_insert(k)?, dict.get_or_insert(v)?);
                Ok(meta)
            },
        )?,
        metrics: span.metrics.iter().try_fold(
            HashMap::with_capacity(span.metrics.len()),
            |mut metrics, (k, v)| -> anyhow::Result<HashMap<u32, f64>> {
                metrics.insert(dict.get_or_insert(k)?, *v);
                Ok(metrics)
            },
        )?,
        r#type: dict.get_or_insert(&span.r#type)?,
    })
}
