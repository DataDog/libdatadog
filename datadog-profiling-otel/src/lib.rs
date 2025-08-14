// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

pub use proto::*;

/// Extension trait for ProfilesData to add serialization methods
pub trait ProfilesDataExt {
    /// Serializes the profile into a zstd compressed protobuf byte array.
    /// This method consumes the ProfilesData and returns the compressed bytes.
    fn serialize_into_compressed_proto(self) -> anyhow::Result<Vec<u8>>;
}

impl ProfilesDataExt for ProfilesData {
    fn serialize_into_compressed_proto(self) -> anyhow::Result<Vec<u8>> {
        // TODO, streaming into zstd is difficult because prost wants a BytesMut which zstd doesn't
        // easily supply.
        let proto_bytes = prost::Message::encode_to_vec(&self);
        let compressed_bytes = zstd::encode_all(&proto_bytes[..], 0)?;
        Ok(compressed_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proto_generation() {
        // Test that we can create basic protobuf messages
        let mut profiles_dict = ProfilesDictionary::default();
        profiles_dict.string_table.push("test".to_string());

        let profiles_data = ProfilesData {
            dictionary: Some(profiles_dict),
            ..Default::default()
        };

        // Verify the data was set correctly
        assert_eq!(
            profiles_data.dictionary.as_ref().unwrap().string_table[0],
            "test"
        );
    }

    #[test]
    fn test_serialize_into_compressed_proto() {
        // Test that we can serialize and compress a ProfilesData
        let mut profiles_dict = ProfilesDictionary::default();
        profiles_dict.string_table.push("test".to_string());

        let profiles_data = ProfilesData {
            dictionary: Some(profiles_dict),
            ..Default::default()
        };

        // Serialize and compress
        let compressed_bytes = profiles_data.serialize_into_compressed_proto().unwrap();

        // Verify we got compressed bytes
        assert!(!compressed_bytes.is_empty());

        // Verify we can decompress and deserialize
        let decompressed_bytes = zstd::decode_all(&compressed_bytes[..]).unwrap();
        let deserialized: ProfilesData = prost::Message::decode(&decompressed_bytes[..]).unwrap();

        // Verify the data is correct
        assert_eq!(
            deserialized.dictionary.as_ref().unwrap().string_table[0],
            "test"
        );
    }
}
