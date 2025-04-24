// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use datadog_profiling::pprof::{Function, Location, Mapping, Profile};
use std::collections::HashMap;

pub struct ProfileIndex<'pprof> {
    pub pprof: &'pprof Profile,
    mappings: HashMap<u64, &'pprof Mapping>,
    locations: HashMap<u64, &'pprof Location>,
    functions: HashMap<u64, &'pprof Function>,
}

impl<'pprof> TryFrom<&'pprof Profile> for ProfileIndex<'pprof> {
    type Error = anyhow::Error;

    fn try_from(pprof: &'pprof Profile) -> Result<Self> {
        let mut mappings = HashMap::with_capacity(pprof.mappings.len());
        for v in pprof.mappings.iter() {
            let k = v.id;
            if mappings.insert(k, v).is_some() {
                anyhow::bail!("multiple pprof mappings were found with id {k} ")
            }
        }

        let mut locations = HashMap::with_capacity(pprof.locations.len());
        for v in pprof.locations.iter() {
            let k = v.id;
            if locations.insert(k, v).is_some() {
                anyhow::bail!("multiple pprof locations were found with id {k} ")
            }
        }

        let mut functions = HashMap::with_capacity(pprof.functions.len());
        for v in pprof.functions.iter() {
            let k = v.id;
            if functions.insert(k, v).is_some() {
                anyhow::bail!("multiple pprof functions were found with id {k} ")
            }
        }

        Ok(Self {
            pprof,
            mappings,
            locations,
            functions,
        })
    }
}

impl<'pprof> ProfileIndex<'pprof> {
    pub fn get_string(&self, id: i64) -> Result<&'pprof str> {
        match usize::try_from(id) {
            Ok(index) => match self.pprof.string_table.get(index) {
                Some(str) => Ok(str),
                None => anyhow::bail!("pprof did not contain string index {index}"),
            },
            Err(err) => {
                anyhow::bail!("index to pprof string table {id} failed to convert to usize: {err}")
            }
        }
    }

    pub fn get_mapping(&'pprof self, id: u64) -> Result<&'pprof Mapping> {
        match self.mappings.get(&id) {
            None => anyhow::bail!("pprof did not contain mapping id {id}"),
            Some(item) => Ok(*item),
        }
    }

    pub fn get_location(&'pprof self, id: u64) -> Result<&'pprof Location> {
        match self.locations.get(&id) {
            None => anyhow::bail!("pprof did not contain location id {id}"),
            Some(item) => Ok(*item),
        }
    }

    pub fn get_function(&'pprof self, id: u64) -> Result<&'pprof Function> {
        match self.functions.get(&id) {
            None => anyhow::bail!("pprof did not contain function id {id}"),
            Some(item) => Ok(*item),
        }
    }
}
