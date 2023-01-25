// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::pprof::ValueType;
use super::symbol_table::Diff;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnifiedServiceTags {
    env: Option<String>,
    service: Option<String>,
    version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sample {
    locations: Vec<u64>,
    values: Vec<i64>,
    labels: Vec<i64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Profile {
    // todo: session_id: u64,
    unified_service_tags: UnifiedServiceTags,
    sample_types: Vec<ValueType>,
    start_time: i64,
    period: Option<(i64, ValueType)>,
}

impl Profile {
    pub fn new(
        unified_service_tags: UnifiedServiceTags,
        sample_types: Vec<ValueType>,
        start_time: i64,
        period: Option<(i64, ValueType)>,
    ) -> Self {
        Self {
            unified_service_tags,
            sample_types,
            start_time,
            period,
        }
    }
}

pub struct Record {
    pub profile: Profile,
    pub symbol_diff: Diff,
    pub samples: Vec<Sample>,
}

#[cfg(test)]
mod tests {
    use super::super::{Function, Line, Location, SymbolTable};
    use super::*;

    use bumpalo::Bump;
    use std::sync::mpsc::channel;

    #[test]
    pub fn send() {
        let symbol_arena = Bump::new();
        let string_arena = Bump::new();
        // Safety: the arena is not touched outside of the symbol table.
        let mut symbols = unsafe { SymbolTable::new(&string_arena, &symbol_arena) };

        let (sender, receiver) = channel::<Record>();

        let join_handle = std::thread::spawn(move || {
            let symbol_arena = Bump::new();
            let string_arena = Bump::new();
            // Safety: the arena is not touched outside of the symbol table.
            let mut symbols = unsafe { SymbolTable::new(&string_arena, &symbol_arena) };

            let record = receiver.recv().unwrap();

            let diff = record.symbol_diff;
            let diff_range = symbols.apply_diff(diff.clone()).unwrap();

            let actual_diff = symbols.fetch_diff(diff_range).unwrap();

            assert_eq!(diff, actual_diff);
        });

        // Transaction begin {{
        let mut transaction = symbols.begin_transaction();
        let wall_samples = transaction.add_string("wall-samples");
        let count = transaction.add_string("count");
        let wall_time = transaction.add_string("wall-time");
        let milliseconds = transaction.add_string("milliseconds");

        let str_main = transaction.add_string("main");
        let str_main_c = transaction.add_string("main.c");
        let function_main = transaction.add_function(Function {
            name: str_main,
            filename: str_main_c,
            ..Function::default()
        });

        let location = transaction.add_location(Location {
            lines: vec![Line {
                function_id: function_main,
                line: 4,
            }],
            ..Location::default()
        });

        let diff_range = transaction.save();

        // }}} Transaction end
        let symbol_diff = symbols.fetch_diff(diff_range).unwrap();

        let unified_service_tags = UnifiedServiceTags {
            env: None,
            service: None,
            version: None,
        };

        let sample_types = vec![ValueType {
            r#type: wall_samples,
            unit: count,
        }];

        let period = Some((
            10,
            ValueType {
                r#type: wall_time,
                unit: milliseconds,
            },
        ));
        let profile = Profile::new(unified_service_tags, sample_types, 0, period);

        let samples = vec![Sample {
            locations: vec![location],
            values: vec![1],
            labels: vec![],
        }];

        sender
            .send(Record {
                profile,
                symbol_diff,
                samples,
            })
            .unwrap();

        join_handle.join().unwrap()
    }
}
