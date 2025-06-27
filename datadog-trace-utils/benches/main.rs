// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::criterion_main;

mod deserialization;
mod serialization;


criterion_main!(serialization::serialize_benches, deserialization::deserialize_benches);
