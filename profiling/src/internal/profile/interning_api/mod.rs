// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod generational_ids;
pub use generational_ids::*;

use crate::collections::identifiable::{Dedup, StringId};
use crate::internal::{
    Function, FunctionId, Label, LabelId, LabelSet, LabelSetId, Location, LocationId, Mapping,
    MappingId, Profile, Sample, StackTrace, StackTraceId, Timestamp,
};

impl Profile {
    pub fn intern_function(
        &mut self,
        name: GenerationalId<StringId>,
        system_name: GenerationalId<StringId>,
        filename: GenerationalId<StringId>,
        start_line: i64,
    ) -> anyhow::Result<GenerationalId<FunctionId>> {
        let function = Function {
            name: name.get(self.generation)?,
            system_name: system_name.get(self.generation)?,
            filename: filename.get(self.generation)?,
            start_line,
        };
        let id = self.functions.dedup(function);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_label_num(
        &mut self,
        key: GenerationalId<StringId>,
        val: i64,
        unit: Option<GenerationalId<StringId>>,
    ) -> anyhow::Result<GenerationalId<LabelId>> {
        let key = key.get(self.generation)?;
        let unit = unit.map(|u| u.get(self.generation)).transpose()?;
        let id = self.labels.dedup(Label::num(key, val, unit));
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_label_str(
        &mut self,
        key: GenerationalId<StringId>,
        val: GenerationalId<StringId>,
    ) -> anyhow::Result<GenerationalId<LabelId>> {
        let key = key.get(self.generation)?;
        let val = val.get(self.generation)?;
        let id = self.labels.dedup(Label::str(key, val));
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_label_set(
        &mut self,
        labels: Vec<GenerationalId<LabelId>>,
    ) -> anyhow::Result<GenerationalId<LabelSetId>> {
        let labels = labels
            .into_iter()
            .map(|l| l.get(self.generation))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let labels = LabelSet::new(labels);
        let id = self.label_sets.dedup(labels);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_location(
        &mut self,
        mapping_id: GenerationalId<MappingId>,
        function_id: GenerationalId<FunctionId>,
        address: u64,
        line: i64,
    ) -> anyhow::Result<GenerationalId<LocationId>> {
        let location = Location {
            mapping_id: mapping_id.get(self.generation)?,
            function_id: function_id.get(self.generation)?,
            address,
            line,
        };
        let id = self.locations.dedup(location);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_mapping(
        &mut self,
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: GenerationalId<StringId>,
        build_id: GenerationalId<StringId>,
    ) -> anyhow::Result<GenerationalId<MappingId>> {
        let mapping = Mapping {
            memory_start,
            memory_limit,
            file_offset,
            filename: filename.get(self.generation)?,
            build_id: build_id.get(self.generation)?,
        };
        let id = self.mappings.dedup(mapping);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_sample(
        &mut self,
        stacktrace: GenerationalId<StackTraceId>,
        values: Vec<i64>,
        labels: GenerationalId<LabelSetId>,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        // TODO: validate sample labels? Or should we do that when we make the label set?
        anyhow::ensure!(
            values.len() == self.sample_types.len(),
            "expected {} sample types, but sample had {} sample types",
            self.sample_types.len(),
            values.len(),
        );
        let stacktrace = stacktrace.get(self.generation)?;
        let labels = labels.get(self.generation)?;

        self.observations
            .add(Sample::new(labels, stacktrace), timestamp, values)
    }

    pub fn intern_stacktrace(
        &mut self,
        locations: Vec<GenerationalId<LocationId>>,
    ) -> anyhow::Result<GenerationalId<StackTraceId>> {
        let locations = locations
            .into_iter()
            .map(|l| l.get(self.generation))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let stacktrace = StackTrace { locations };
        let id = self.stack_traces.dedup(stacktrace);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_string(&mut self, s: &str) -> anyhow::Result<GenerationalId<StringId>> {
        Ok(GenerationalId::new(self.intern(s), self.generation))
    }
}
