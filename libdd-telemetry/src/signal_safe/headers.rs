// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::Error;

pub(super) fn validate(headers: &[(&str, &str)]) -> Result<(), Error> {
    for &(name, value) in headers {
        // Field names follow RFC 9110 section 5.6.2's nonempty token grammar.
        // Values allow HTAB, visible ASCII, and obs-text, like the worker's HeaderValue.
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(Error::InvalidHeaderName);
        }
        if value
            .bytes()
            .any(|byte| byte != b'\t' && (byte < b' ' || byte == 0x7f))
        {
            return Err(Error::InvalidHeaderValue);
        }
    }
    Ok(())
}
