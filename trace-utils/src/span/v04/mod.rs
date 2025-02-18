// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;

pub mod trace_utils;

pub use span::{Span, SpanBytes, SpanKey, SpanKeyParseError, SpanLink, SpanLinkBytes, SpanValue};
