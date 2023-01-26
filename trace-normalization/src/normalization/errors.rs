// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct NormalizerError {
    details: String
}

impl NormalizerError {
    pub fn new(msg: &str) -> NormalizerError {
        NormalizerError{details: msg.to_string()}
    }
}

impl fmt::Display for NormalizerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,"{}",self.details)
    }
}

impl Error for NormalizerError {
    fn description(&self) -> &str {
        &self.details
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum NormalizeErrors {
    // ErrorEmpty specifies that the passed input was empty.
    ErrorEmpty,
    // ErrorTooLong signifies that the input was too long.
    ErrorTooLong,
    // ErrorInvalid signifies that the input was invalid.
    ErrorInvalid
}

impl fmt::Display for NormalizeErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ErrorEmpty => "empty",
            Self::ErrorTooLong => "too long",
            Self::ErrorInvalid => "invalid",
        })
    }
}

impl Error for NormalizeErrors {}
