// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod generational_ids;
pub use generational_ids::*;

use crate::api::ManagedStringId;
use crate::collections::identifiable::{Dedup, StringId};
use crate::internal::{
    Function, FunctionId, Label, LabelId, LabelSet, LabelSetId, Location, LocationId, Mapping,
    MappingId, Profile, Sample, StackTrace, StackTraceId, Timestamp,
};
use std::sync::atomic::Ordering::SeqCst;

impl Profile {
    pub fn intern_function(
        &mut self,
        name: GenerationalId<StringId>,
        system_name: GenerationalId<StringId>,
        filename: GenerationalId<StringId>,
    ) -> anyhow::Result<GenerationalId<FunctionId>> {
        let function = Function {
            name: name.get(self.generation)?,
            system_name: system_name.get(self.generation)?,
            filename: filename.get(self.generation)?,
        };
        let id = self.functions.dedup(function);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_label_num(
        &mut self,
        key: GenerationalId<StringId>,
        val: i64,
        unit: GenerationalId<StringId>,
    ) -> anyhow::Result<GenerationalId<LabelId>> {
        let key = key.get(self.generation)?;
        let unit = unit.get(self.generation)?;
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

    pub fn intern_labelset(
        &mut self,
        labels: &[GenerationalId<LabelId>],
    ) -> anyhow::Result<GenerationalId<LabelSetId>> {
        let labels = labels
            .iter()
            .map(|l| l.get(self.generation))
            .collect::<anyhow::Result<Box<_>>>()?;
        let labels = LabelSet::new(labels);
        let id = self.label_sets.dedup(labels);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_location(
        &mut self,
        mapping_id: Option<GenerationalId<MappingId>>,
        function_id: GenerationalId<FunctionId>,
        address: u64,
        line: i64,
    ) -> anyhow::Result<GenerationalId<LocationId>> {
        let location = Location {
            mapping_id: mapping_id.map(|id| id.get(self.generation)).transpose()?,
            function_id: function_id.get(self.generation)?,
            address,
            line,
        };
        let id = self.locations.dedup(location);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_managed_string(
        &mut self,
        s: ManagedStringId,
    ) -> anyhow::Result<GenerationalId<StringId>> {
        let id = self.resolve(s)?;
        Ok(GenerationalId::new(id, self.generation))
    }

    pub fn intern_managed_strings(
        &mut self,
        s: &[ManagedStringId],
        out: &mut [GenerationalId<StringId>],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(s.len() == out.len());
        for i in 0..s.len() {
            out[i] = self.intern_managed_string(s[i])?;
        }
        Ok(())
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
        values: &[i64],
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
        locations: &[GenerationalId<LocationId>],
    ) -> anyhow::Result<GenerationalId<StackTraceId>> {
        let locations = locations
            .iter()
            .map(|l| l.get(self.generation))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let stacktrace = StackTrace { locations };
        let id = self.stack_traces.dedup(stacktrace);
        Ok(GenerationalId::new(id, self.generation))
    }

    pub const INTERNED_EMPTY_STRING: GenerationalId<StringId> =
        GenerationalId::new_immortal(StringId::ZERO);

    pub fn intern_string(&mut self, s: &str) -> anyhow::Result<GenerationalId<StringId>> {
        if s.is_empty() {
            Ok(Self::INTERNED_EMPTY_STRING)
        } else {
            Ok(GenerationalId::new(self.try_intern(s)?, self.generation))
        }
    }

    pub fn intern_strings(
        &mut self,
        s: &[&str],
        out: &mut [GenerationalId<StringId>],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(s.len() == out.len());
        for i in 0..s.len() {
            out[i] = self.intern_string(s[i])?;
        }
        Ok(())
    }

    // Simple synchronization between samples and profile rotation/export.
    // Interning a sample may require several calls to the profiler to intern intermediate values,
    // which are not inherently atomic.  Since these intermediate values are tied to a particular
    // profiler generation, and are invalidated when the generation changes, some coordination must
    // occur between sampling and profile rotation/export.
    // When the generation changes, one of three things can happen:
    // 1. The sample can be dropped.
    // 2. The sample can be recreated and interned into the new profile.
    // 3. The profile rotation should wait until the sampling operation is complete.
    //
    // This API provides a mechanism for samples to pause rotation until they complete, and
    // for samples to be notified that a rotation is in progress so they can wait to begin.
    // There are probably better ways, and maybe we should have a notification mechanism.
    // But for now this should be enough.
    const FLAG: u64 = u32::MAX as u64;

    /// Prevent any new samples from starting.
    /// Returns the number of remaining samples.
    pub fn sample_block(&mut self) -> anyhow::Result<u64> {
        let current = self.active_samples.fetch_add(Self::FLAG, SeqCst);
        if current >= Self::FLAG {
            self.active_samples.fetch_sub(Self::FLAG, SeqCst);
        }
        Ok(current % Self::FLAG)
    }

    pub fn sample_end(&mut self) -> anyhow::Result<()> {
        self.active_samples.fetch_sub(1, SeqCst);
        Ok(())
    }

    pub fn sample_start(&mut self) -> anyhow::Result<()> {
        let old = self.active_samples.fetch_add(1, SeqCst);
        if old >= Self::FLAG {
            self.active_samples.fetch_sub(1, SeqCst);
            anyhow::bail!("Can't start sample, export in progress");
        }
        Ok(())
    }

    pub fn samples_active(&mut self) -> anyhow::Result<u64> {
        let current = self.active_samples.load(SeqCst);
        Ok(current % Self::FLAG)
    }

    pub fn samples_are_blocked(&mut self) -> anyhow::Result<bool> {
        let current = self.active_samples.load(SeqCst);
        Ok(current >= Self::FLAG)
    }

    pub fn samples_are_drained(&mut self) -> anyhow::Result<bool> {
        let current = self.active_samples.load(SeqCst);
        Ok(current % Self::FLAG == 0)
    }
}
