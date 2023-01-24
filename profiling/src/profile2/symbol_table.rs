// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use super::pprof::*;
use super::prof_table::{ProfTable, Storable};
use super::string_table::StringTable;
use super::u63::u63;
use bumpalo::Bump;
use std::hash::Hash;
use std::ops::Range;

type SymbolSet<'arena, T> = ProfTable<'arena, T>;

pub struct SymbolTable<'arena> {
    mappings: ProfTable<'arena, Mapping>,
    locations: SymbolSet<'arena, Location>,
    functions: SymbolSet<'arena, Function>,
    strings: StringTable<'arena>,
}

impl<'s> SymbolTable<'s> {
    /// # Safety
    /// Do not reset the arena until the symbol table is gone.
    pub unsafe fn new(arena: &'s Bump) -> Self {
        Self {
            mappings: ProfTable::new(arena),
            locations: ProfTable::new(arena),
            functions: ProfTable::new(arena),
            strings: StringTable::new(arena),
        }
    }

    fn fetch_range<T: Storable>(set: &SymbolSet<T>, range: Range<usize>) -> Vec<T>
    where
        T: Clone + Eq + Hash,
    {
        set[range].iter().cloned().cloned().collect()
    }

    pub fn mappings(&self, range: Range<usize>) -> Vec<Mapping> {
        Self::fetch_range(&self.mappings, range)
    }

    pub fn locations(&self, range: Range<usize>) -> Vec<Location> {
        Self::fetch_range(&self.locations, range)
    }

    pub fn functions(&self, range: Range<usize>) -> Vec<Function> {
        Self::fetch_range(&self.functions, range)
    }

    pub fn strings(&self, range: Range<usize>) -> anyhow::Result<Vec<(u63, String)>> {
        let set = &self.strings;

        anyhow::ensure!(
            range.start <= range.end && range.end <= set.len(),
            "provided range {}..{} is out-of-bounds",
            range.start,
            range.end
        );

        // Iterate on one range, slice by the other.
        let range2 = range.clone();
        let result: anyhow::Result<Vec<_>> = range
            .into_iter()
            .zip(set[range2].iter())
            .map(|(offset, str)| Ok((u63::try_from(offset)?, str.to_string())))
            .collect();
        result
    }

    pub fn fetch_diff(&self, diff: DiffRange) -> anyhow::Result<Diff> {
        let locations = self.locations(diff.locations);
        let mappings = self.mappings(diff.mappings);
        let functions = self.functions(diff.functions);
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

pub struct Transaction<'symbol_table, 'arena: 'symbol_table> {
    symbol_table: &'symbol_table mut SymbolTable<'arena>,
    diff: DiffRange,
}

impl<'a, 's: 'a> Transaction<'a, 's> {
    pub fn new(symbol_table: &'a mut SymbolTable<'s>) -> Self {
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

    fn add<T: Storable>(table: &mut ProfTable<T>, diff: &mut Range<usize>, value: &T) -> u63 {
        let (value, inserted) = table.insert_full(value);
        let id = value.get_id().into();
        if inserted {
            Self::push_range(diff, id.into());
        }
        id
    }

    pub fn add_mapping(&mut self, mapping: Mapping) -> u63 {
        Self::add(
            &mut self.symbol_table.mappings,
            &mut self.diff.mappings,
            &mapping,
        )
    }

    pub fn add_location(&mut self, location: Location) -> u63 {
        Self::add(
            &mut self.symbol_table.locations,
            &mut self.diff.locations,
            &location,
        )
    }

    pub fn add_function(&mut self, function: Function) -> u63 {
        Self::add(
            &mut self.symbol_table.functions,
            &mut self.diff.functions,
            &function,
        )
    }

    pub fn add_string(&mut self, str: impl AsRef<str>) -> u63 {
        let str = str.as_ref();

        let (offset, inserted) = self.symbol_table.strings.insert_full(str);

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

impl<'a, 's: 'a> Drop for Transaction<'a, 's> {
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

impl<'s> SymbolTable<'s> {
    pub fn begin_transaction<'a>(&'a mut self) -> Transaction<'a, 's> {
        Transaction::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execute(symbol_table: &mut SymbolTable, input: Diff, expect: &Diff) {
        let diff = symbol_table.apply_diff(input).unwrap();
        let actual = symbol_table.fetch_diff(diff).unwrap();
        assert_eq!(expect, &actual);
    }

    #[test]
    fn test_empty_cases() {
        let arena = Bump::new();
        // Safety: the arena is not touched outside of the symbol table.
        let mut symbol_table = unsafe { SymbolTable::new(&arena) };

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
        let arena = Bump::new();
        // Safety: the arena is not touched outside of the symbol table.
        let mut symbol_table = unsafe { SymbolTable::new(&arena) };

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
