// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(clippy::all)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod credit_cards;
pub mod http;
pub mod ip_address;
pub mod memcached;
pub mod obfuscate;
pub mod obfuscation_config;
pub mod redis;
pub mod redis_tokenizer;
pub mod replacer;
pub mod sql;
