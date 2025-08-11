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
}
