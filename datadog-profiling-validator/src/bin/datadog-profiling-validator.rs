// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use clap::Parser;
use datadog_profiling_protobuf::prost_impls::*;
use prost::Message;
use std::collections::HashSet;
use std::io::{self, Read};
use std::{fs, path};

#[derive(Parser, Debug)]
struct Cli {
    /// Optional path to work on, otherwise reads from stdin
    input: Option<path::PathBuf>,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let mut buffer = Vec::new();
    if let Some(path) = cli.input {
        let mut file = fs::File::open(path)?;
        file.read_to_end(&mut buffer)?;
    } else {
        io::stdin().lock().read_to_end(&mut buffer)?;
    };

    let profile = Profile::decode(buffer.as_slice())?;
    validate_profile(&profile)?;
    println!("Profile validation successful!");
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum ValidationError {
    #[error(
        "String index `index` is out of range (max: `max_len`) in `context`"
    )]
    StringIndexOutOfRange { index: i64, max_len: usize, context: &'static str },
    #[error("Empty string appears twice at indices `first_index` and `second_index`")]
    DuplicateEmptyString { first_index: i64, second_index: i64 },
    #[error("Invalid string table: {reason}")]
    InvalidStringTable { reason: String },
    #[error("Referenced mapping ID `id` does not exist")]
    MissingMapping { id: u64 },
    #[error("Referenced location ID `id` does not exist")]
    MissingLocation { id: u64 },
    #[error("Referenced function ID `id` does not exist")]
    MissingFunction { id: u64 },
}

impl From<ValidationError> for io::Error {
    fn from(err: ValidationError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, err)
    }
}

fn validate_string_table(
    string_table: &[String],
) -> Result<(), ValidationError> {
    // Validate that the first exists and the string is empty.
    if !string_table.first().is_some_and(String::is_empty) {
        return Err(ValidationError::InvalidStringTable {
            reason: "First string must be empty".to_string(),
        });
    }

    // Check for any other empty strings (which would be duplicates).
    string_table
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, s)| s.is_empty())
        .map(|(i, _)| {
            Err(ValidationError::DuplicateEmptyString {
                first_index: 0,
                second_index: i as i64,
            })
        })
        .unwrap_or(Ok(()))
}

fn validate_string_index(
    index: i64,
    string_table: &[String],
    context: &'static str,
) -> Result<(), ValidationError> {
    if (0..string_table.len() as i64).contains(&index) {
        Ok(())
    } else {
        Err(ValidationError::StringIndexOutOfRange {
            index,
            max_len: string_table.len(),
            context,
        })
    }
}

fn validate_profile(profile: &Profile) -> Result<(), ValidationError> {
    // Validate string table first.
    validate_string_table(&profile.string_table)?;

    // Build lookup tables for IDs.
    let mapping_ids: HashSet<u64> =
        profile.mappings.iter().map(|m| m.id).collect();
    let location_ids: HashSet<u64> =
        profile.locations.iter().map(|l| l.id).collect();
    let function_ids: HashSet<u64> =
        profile.functions.iter().map(|f| f.id).collect();

    // Validate all string indices in functions.
    for function in &profile.functions {
        validate_string_index(
            function.name,
            &profile.string_table,
            "function name",
        )?;
        validate_string_index(
            function.system_name,
            &profile.string_table,
            "function system_name",
        )?;
        validate_string_index(
            function.filename,
            &profile.string_table,
            "function filename",
        )?;
    }

    // Validate all string indices in mappings.
    for mapping in &profile.mappings {
        validate_string_index(
            mapping.filename,
            &profile.string_table,
            "mapping filename",
        )?;
        validate_string_index(
            mapping.build_id,
            &profile.string_table,
            "mapping build_id",
        )?;
    }

    // Validate all string indices in locations.
    for location in &profile.locations {
        for line in &location.lines {
            if !function_ids.contains(&line.function_id) {
                return Err(ValidationError::MissingFunction {
                    id: line.function_id,
                });
            }
        }
        // Validate mapping ID references.
        if location.mapping_id != 0
            && !mapping_ids.contains(&location.mapping_id)
        {
            return Err(ValidationError::MissingMapping {
                id: location.mapping_id,
            });
        }
    }

    // Validate all string indices in samples.
    for sample in &profile.samples {
        for label in &sample.labels {
            validate_string_index(
                label.key,
                &profile.string_table,
                "sample label key",
            )?;
            validate_string_index(
                label.str,
                &profile.string_table,
                "sample label value",
            )?;
        }
        // Validate location ID references.
        for &location_id in &sample.location_ids {
            if !location_ids.contains(&location_id) {
                return Err(ValidationError::MissingLocation {
                    id: location_id,
                });
            }
        }
    }

    Ok(())
}
