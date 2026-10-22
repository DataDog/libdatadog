// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// The generated protobuf code copies upstream proto comments verbatim, some of which contain
// placeholders like `<version>` that rustdoc parses as HTML tags.
#![allow(rustdoc::invalid_html_tags)]

#[rustfmt::skip]
include!("_includes.rs");
pub mod deserializers;
mod serde;

#[cfg(test)]
mod pb_test;
