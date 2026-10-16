// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS Lambda trigger types.

pub mod alb;
pub mod api_gateway_http;
pub mod api_gateway_rest;
pub mod api_gateway_websocket;
pub mod dynamodb;
pub mod event_bridge;
pub mod kinesis;
pub mod lambda_function_url;
pub mod msk;
pub mod s3;
pub mod sns;
pub mod sqs;
pub mod step_function;
