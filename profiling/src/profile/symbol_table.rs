// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use super::pprof::*;
use super::u63::u63;
use std::hash::{BuildHasherDefault, Hash};
use std::ops::Range;

type SymbolSet<K> = indexmap::IndexSet<K, BuildHasherDefault<rustc_hash::FxHasher>>;

pub struct SymbolTable {
    mappings: SymbolSet<Mapping>,
    locations: SymbolSet<Location>,
    functions: SymbolSet<Function>,
    strings: SymbolSet<String>,
}

impl SymbolTable {
    pub fn new() -> Self {
        /* Populate all the values so they have something at offset 0.
         * These will never be sent as a diff, not even the empty string.
         */
        Self {
            mappings: SymbolSet::from_iter(std::iter::once(Default::default())),
            locations: SymbolSet::from_iter(std::iter::once(Default::default())),
            functions: SymbolSet::from_iter(std::iter::once(Default::default())),
            strings: SymbolSet::from_iter(std::iter::once(Default::default())),
        }
    }

    fn fetch_range<T>(set: &SymbolSet<T>, mut range: Range<usize>) -> anyhow::Result<Vec<T>>
    where
        T: Clone + Eq + Hash,
    {
        if range.start == 0 {
            if range.end > 1 {
                range.start += 1;
            } else {
                return Ok(Vec::new());
            }
        }

        if range.start < set.len() && range.end <= set.len() {
            let start = range.start;
            let count = range.count();
            return Ok(set.iter().skip(start).take(count).cloned().collect());
        }
        anyhow::bail!(
            "provided range {}..{} is out-of-bounds",
            range.start,
            range.end
        );
    }

    pub fn mappings(&self, range: Range<usize>) -> anyhow::Result<Vec<Mapping>> {
        Self::fetch_range(&self.mappings, range)
    }

    pub fn locations(&self, range: Range<usize>) -> anyhow::Result<Vec<Location>> {
        Self::fetch_range(&self.locations, range)
    }

    pub fn functions(&self, range: Range<usize>) -> anyhow::Result<Vec<Function>> {
        Self::fetch_range(&self.functions, range)
    }

    pub fn strings(&self, mut range: Range<usize>) -> anyhow::Result<Vec<(u63, String)>> {
        let set = &self.strings;

        if range.start == 0 {
            if range.end > 1 {
                range.start += 1;
            } else {
                return Ok(Vec::new());
            }
        }

        if range.start < set.len() && range.end <= set.len() {
            let start = range.start;
            let count = range.end - range.start;
            let result: anyhow::Result<Vec<_>> = set
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .map(|(offset, str)| Ok((u63::try_from(offset)?, str.clone())))
                .collect();
            Ok(result?)
        } else {
            anyhow::bail!(
                "provided range {}..{} is out-of-bounds",
                range.start,
                range.end
            )
        }
    }

    pub fn fetch_diff(&self, diff: DiffRange) -> anyhow::Result<Diff> {
        let locations = self.locations(diff.locations)?;
        let mappings = self.mappings(diff.mappings)?;
        let functions = self.functions(diff.functions)?;
        let strings = self.strings(diff.strings)?;

        Ok(Diff {
            locations,
            mappings,
            functions,
            strings,
        })
    }

    pub fn apply_diff(&mut self, diff: Diff) -> anyhow::Result<DiffRange> {
        let mut transaction = self.begin_transaction();
        for (id, string) in &diff.strings {
            let id = *id;
            let new_id = transaction.add_string(string);
            if id != u63::default() {
                anyhow::ensure!(
                    id == new_id,
                    "interning string \"{string}\" resulted in {new_id}, expected {id}"
                );
            }
        }
        for function in &diff.functions {
            let id = function.id;
            let new_id: u64 = transaction.add_function(*function).into();
            if id != 0 {
                anyhow::ensure!(
                    new_id == id,
                    "inserting function id {id} resulted in a different id {new_id}"
                );
            }
        }
        for mapping in &diff.mappings {
            let id = mapping.id;
            let new_id: u64 = transaction.add_mapping(*mapping).into();
            if id != 0 {
                anyhow::ensure!(
                    new_id == id,
                    "inserting mapping id {id} resulted in a different id {new_id}"
                );
            }
        }
        for location in &diff.locations {
            let id = location.id;
            let new_id: u64 = transaction.add_location(location.clone()).into();
            if id != 0 {
                anyhow::ensure!(
                    new_id == id,
                    "inserting location id {id} resulted in a different id {new_id}"
                );
            }
        }

        Ok(transaction.save())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diff {
    locations: Vec<Location>,
    mappings: Vec<Mapping>,
    functions: Vec<Function>,
    strings: Vec<(u63, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct DiffRange {
    pub mappings: Range<usize>,
    pub locations: Range<usize>,
    pub functions: Range<usize>,
    pub strings: Range<usize>,
}

pub struct Transaction<'a> {
    symbol_table: &'a mut SymbolTable,
    diff: DiffRange,
}

impl<'a> Transaction<'a> {
    pub fn new(symbol_table: &'a mut SymbolTable) -> Self {
        Self {
            symbol_table,
            diff: DiffRange::default(),
        }
    }

    fn push_range(range: &mut Range<usize>, offset: usize) {
        if Range::is_empty(range) {
            range.start = offset;
        } else {
            assert_eq!(range.end, offset);
        }
        range.end = offset + 1;
    }

    pub fn add_mapping(&mut self, mut mapping: Mapping) -> u63 {
        mapping.id = self.symbol_table.mappings.len().try_into().unwrap();
        let (offset, inserted) = self.symbol_table.mappings.insert_full(mapping);
        if inserted {
            Self::push_range(&mut self.diff.mappings, offset);
        }
        offset.try_into().unwrap()
    }

    pub fn add_location(&mut self, mut location: Location) -> u63 {
        location.id = self.symbol_table.locations.len().try_into().unwrap();
        let (offset, inserted) = self.symbol_table.locations.insert_full(location);
        if inserted {
            Self::push_range(&mut self.diff.locations, offset);
        }
        offset.try_into().unwrap()
    }

    pub fn add_function(&mut self, mut function: Function) -> u63 {
        function.id = self.symbol_table.functions.len().try_into().unwrap();
        let (offset, inserted) = self.symbol_table.functions.insert_full(function);
        if inserted {
            Self::push_range(&mut self.diff.functions, offset);
        }
        offset.try_into().unwrap()
    }

    pub fn add_string(&mut self, str: impl AsRef<str>) -> u63 {
        let str = str.as_ref();

        let (offset, inserted) = match self.symbol_table.strings.get_index_of(str) {
            Some(offset) => (offset, false),
            None => {
                let (offset, inserted) = self.symbol_table.strings.insert_full(str.into());
                // This wouldn't make any sense; the item couldn't be found so
                // it was inserted but then it already existed? Screams race-
                // -condition to me!
                assert!(inserted);
                (offset, inserted)
            }
        };

        if inserted {
            Self::push_range(&mut self.diff.strings, offset);
        }

        offset.try_into().unwrap()
    }

    pub fn save(mut self) -> DiffRange {
        let mut diff = DiffRange::default();
        std::mem::swap(&mut diff, &mut self.diff);
        diff
    }
}

impl<'a> Drop for Transaction<'a> {
    fn drop(&mut self) {
        if !self.diff.mappings.is_empty() {
            todo!("implement drop for mappings")
        }
        if !self.diff.functions.is_empty() {
            todo!("implement drop for functions")
        }
        if !self.diff.locations.is_empty() {
            todo!("implement drop for locations")
        }
    }
}

impl SymbolTable {
    pub fn begin_transaction(&mut self) -> Transaction {
        Transaction::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::symbol_table;

    fn execute(symbol_table: &mut SymbolTable, input: Diff, expect: &Diff) {
        let diff = symbol_table.apply_diff(input).unwrap();
        let actual = symbol_table.fetch_diff(diff).unwrap();
        assert_eq!(expect, &actual);
    }

    #[test]
    fn test_empty_cases() {
        let mut symbol_table = SymbolTable::new();

        let input = Diff {
            locations: vec![Default::default()],
            mappings: vec![Default::default()],
            functions: vec![Default::default()],
            strings: vec![Default::default()],
        };

        // All of the above will return nothing, since they are the zero cases.
        let expect = Diff::default();
        execute(&mut symbol_table, input, &expect);
    }

    #[test]
    fn test_symbol_table_happy_path() {
        let mut symbol_table = SymbolTable::new();

        let test1 = Diff {
            locations: vec![],
            mappings: vec![],
            functions: vec![Function {
                id: 1,
                name: 1,
                filename: 2,
                ..Function::default()
            }],
            strings: vec![
                (u63::new(1), String::from("main")),
                (u63::new(2), String::from("main.c")),
            ],
        };

        let expect = test1.clone();
        execute(&mut symbol_table, test1, &expect);

        let test2 = Diff {
            locations: vec![
                Location {
                    id: 1,
                    mapping_id: 1,
                    lines: vec![Line {
                        function_id: 2,
                        line: 113,
                    }],
                    ..Location::default()
                },
                Location {
                    id: 2,
                    mapping_id: 1,
                    lines: vec![Line {
                        function_id: 2,
                        line: 7,
                    }],
                    ..Location::default()
                },
                Location {
                    id: 3,
                    mapping_id: 1,
                    lines: vec![Line {
                        function_id: 1,
                        line: 13,
                    }],
                    ..Location::default()
                },
            ],
            mappings: vec![Mapping {
                id: 1,
                filename: 1,
                ..Mapping::default()
            }],
            functions: vec![Function {
                id: 2,
                name: 3,
                filename: 4,
                ..Function::default()
            }],
            strings: vec![
                (u63::new(3), String::from("test")),
                (u63::new(4), String::from("test.c")),
            ],
        };

        let expect = test2.clone();
        execute(&mut symbol_table, test2, &expect);
    }
}
