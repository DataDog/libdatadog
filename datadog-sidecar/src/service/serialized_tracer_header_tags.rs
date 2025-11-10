// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_trace_utils::trace_utils::TracerHeaderTags;
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializedTracerHeaderTags {
    data: Vec<u8>,
}

/// `TryFrom` trait implementation for converting from `SerializedTracerHeaderTags` to
/// `TracerHeaderTags`.
///
/// # Errors
///
/// Returns an `io::Error` if the deserialization of the `SerializedTracerHeaderTags` data fails.
///
/// # Examples
///
/// ```
/// use bincode;
/// use datadog_sidecar::service::SerializedTracerHeaderTags;
/// use libdd_trace_utils::trace_utils::TracerHeaderTags;
/// use std::convert::TryInto;
///
/// let tracer_header_tags = TracerHeaderTags {
///     lang: "Rust",
///     lang_version: "1.55.0",
///     lang_interpreter: "rustc",
///     lang_vendor: "Mozilla",
///     tracer_version: "0.1.0",
///     container_id: "1234567890",
///     client_computed_top_level: true,
///     client_computed_stats: false,
///     ..Default::default()
/// };
///
/// let serialized: SerializedTracerHeaderTags = tracer_header_tags.try_into().unwrap();
///
/// let result: Result<TracerHeaderTags, _> = (&serialized).try_into();
/// assert!(result.is_ok());
/// ```
impl<'a> TryFrom<&'a SerializedTracerHeaderTags> for TracerHeaderTags<'a> {
    type Error = io::Error;

    fn try_from(serialized: &'a SerializedTracerHeaderTags) -> Result<Self, Self::Error> {
        bincode::deserialize(serialized.data.as_slice())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// `TryFrom` trait implementation for converting from `TracerHeaderTags` to
/// `SerializedTracerHeaderTags`.
///
/// # Errors
///
/// Returns a `bincode::Error` if the serialization of the `TracerHeaderTags` data fails.
///
/// # Examples
///
/// ```
/// use datadog_sidecar::service::SerializedTracerHeaderTags;
/// use libdd_trace_utils::trace_utils::TracerHeaderTags;
/// use std::convert::TryInto;
///
/// let tracer_header_tags = TracerHeaderTags {
///     lang: "Rust",
///     lang_version: "1.55.0",
///     lang_interpreter: "rustc",
///     lang_vendor: "Mozilla",
///     tracer_version: "0.1.0",
///     container_id: "1234567890",
///     client_computed_top_level: true,
///     client_computed_stats: false,
///     ..Default::default()
/// };
///
/// let serialized: Result<SerializedTracerHeaderTags, _> = tracer_header_tags.try_into();
/// assert!(serialized.is_ok());
/// ```
// DEV: This implementation is not tested for the case where bincode raises an error because there
// is no reasonable way to force bincode to fail during serialization without modifying the
// `TracerHeaderTags` struct.
impl<'a> TryFrom<TracerHeaderTags<'a>> for SerializedTracerHeaderTags {
    type Error = bincode::Error;

    fn try_from(value: TracerHeaderTags<'a>) -> Result<Self, Self::Error> {
        let data = bincode::serialize(&value)?;
        Ok(SerializedTracerHeaderTags { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryInto;

    #[test]
    fn test_try_from_tracer_header_tags_ok() {
        let tracer_header_tags = TracerHeaderTags {
            lang: "Rust",
            lang_version: "1.55.0",
            lang_interpreter: "rustc",
            lang_vendor: "Mozilla",
            tracer_version: "0.1.0",
            container_id: "1234567890",
            client_computed_top_level: true,
            client_computed_stats: false,
            ..Default::default()
        };

        let serialized: Result<SerializedTracerHeaderTags, _> = tracer_header_tags.try_into();

        assert!(serialized.is_ok());
    }

    #[test]
    fn test_try_from_serialized_tracer_header_tags_ok() {
        let tracer_header_tags = TracerHeaderTags {
            lang: "Rust",
            lang_version: "1.55.0",
            lang_interpreter: "rustc",
            lang_vendor: "Mozilla",
            tracer_version: "0.1.0",
            container_id: "1234567890",
            client_computed_top_level: true,
            client_computed_stats: false,
            ..Default::default()
        };

        let data = bincode::serialize(&tracer_header_tags).unwrap();
        let serialized = SerializedTracerHeaderTags { data };

        let result: Result<TracerHeaderTags, _> = (&serialized).try_into();

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.lang, tracer_header_tags.lang);
        assert_eq!(result.lang_version, tracer_header_tags.lang_version);
        assert_eq!(result.lang_interpreter, tracer_header_tags.lang_interpreter);
        assert_eq!(result.lang_vendor, tracer_header_tags.lang_vendor);
        assert_eq!(result.tracer_version, tracer_header_tags.tracer_version);
        assert_eq!(result.container_id, tracer_header_tags.container_id);
        assert_eq!(
            result.client_computed_top_level,
            tracer_header_tags.client_computed_top_level
        );
        assert_eq!(
            result.client_computed_stats,
            tracer_header_tags.client_computed_stats
        );
    }

    #[test]
    fn test_try_from_serialized_tracer_header_tags_error() {
        let serialized = SerializedTracerHeaderTags {
            data: vec![1, 2, 3, 4, 5], // This should be invalid data for deserialization
        };

        let tracer_header_tags: Result<TracerHeaderTags, _> = (&serialized).try_into();

        assert!(tracer_header_tags.is_err());
    }
}
