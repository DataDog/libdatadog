// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_index::ProfileIndex;
use datadog_profiling::api;
use datadog_profiling::internal::Timestamp;
use datadog_profiling_protobuf::prost_impls;
use std::ops::{Add, Sub};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type LabelsAndEndpointInfo<'pprof> = (
    Option<Timestamp>,
    Vec<api::Label<'pprof>>,
    Option<(u64, &'pprof str)>,
);
type SamplesAndEndpointInfo<'pprof> = (
    Vec<(Option<Timestamp>, api::Sample<'pprof>)>,
    Vec<(u64, &'pprof str)>,
);

pub struct Replayer<'pprof> {
    pub profile_index: ProfileIndex<'pprof>,

    pub start_time: SystemTime,
    pub duration: Duration,
    pub end_time: SystemTime, // start_time + duration
    pub sample_types: Vec<api::ValueType<'pprof>>,
    pub period: Option<api::Period<'pprof>>,
    pub endpoints: Vec<(u64, &'pprof str)>,
    pub samples: Vec<(Option<Timestamp>, api::Sample<'pprof>)>,
}

impl<'pprof> Replayer<'pprof> {
    fn system_time_add(system_time: SystemTime, ns: i64) -> SystemTime {
        if ns < 0 {
            let u64 = ns.unsigned_abs();
            system_time.sub(Duration::from_nanos(u64))
        } else {
            let u64 = ns as u64;
            system_time.add(Duration::from_nanos(u64))
        }
    }

    fn start_time(pprof: &prost_impls::Profile) -> SystemTime {
        Self::system_time_add(UNIX_EPOCH, pprof.time_nanos)
    }

    fn duration(pprof: &prost_impls::Profile) -> anyhow::Result<Duration> {
        match u64::try_from(pprof.duration_nanos) {
            Ok(nanos) => Ok(Duration::from_nanos(nanos)),
            Err(_err) => anyhow::bail!(
                "duration of pprof didn't fit in u64: {}",
                pprof.duration_nanos
            ),
        }
    }

    fn sample_types<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
    ) -> anyhow::Result<Vec<api::ValueType<'pprof>>> {
        let mut sample_types = Vec::with_capacity(profile_index.pprof.sample_types.len());
        for sample_type in profile_index.pprof.sample_types.iter() {
            sample_types.push(api::ValueType::new(
                profile_index.get_string(sample_type.r#type)?,
                profile_index.get_string(sample_type.unit)?,
            ))
        }
        Ok(sample_types)
    }

    fn period<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
    ) -> anyhow::Result<Option<api::Period<'pprof>>> {
        let value = profile_index.pprof.period;

        match profile_index.pprof.period_type {
            Some(period_type) => {
                let r#type = api::ValueType::new(
                    profile_index.get_string(period_type.r#type)?,
                    profile_index.get_string(period_type.unit)?,
                );
                Ok(Some(api::Period { r#type, value }))
            }
            None => Ok(None),
        }
    }

    fn sample_labels<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        sample: &'pprof prost_impls::Sample,
    ) -> anyhow::Result<LabelsAndEndpointInfo<'pprof>> {
        let labels: anyhow::Result<Vec<api::Label>> = sample
            .labels
            .iter()
            .map(|label| {
                Ok(api::Label {
                    key: profile_index.get_string(label.key)?,
                    str: profile_index.get_string(label.str)?,
                    num: label.num,
                    num_unit: profile_index.get_string(label.num_unit)?,
                })
            })
            .collect();
        let mut labels = labels?;

        let lrsi = labels
            .iter()
            .find(|label| label.key == "local root span id");

        let endpoint = labels.iter().find(|label| label.key == "trace endpoint");

        let mut endpoint_info = None;
        if let (Some(lsri_label), Some(endpoint_label)) = (lrsi, endpoint) {
            let num: i64 = lsri_label.num;
            #[allow(
                unknown_lints,
                unnecessary_transmutes,
                reason = "i64::cast_unsigned requires MSRV 1.87.0"
            )]
            let local_root_span_id: u64 = unsafe { std::mem::transmute(num) };
            anyhow::ensure!(
                local_root_span_id != 0,
                "local root span ids of zero do not make sense"
            );

            let endpoint_value = endpoint_label.str;
            if endpoint_value.is_empty() {
                anyhow::bail!("expected trace endpoint label value to have a string")
            };

            endpoint_info.replace((local_root_span_id, endpoint_value));
        }

        let timestamp = labels.iter().find_map(|label| {
            if label.key == "end_timestamp_ns" {
                #[allow(clippy::expect_used)]
                Some(Timestamp::try_from(label.num).expect("non-zero timestamp"))
            } else {
                None
            }
        });

        // Keep all labels except "trace endpoint" and "end_timestamp_ns"
        labels.retain(|label| label.key != "trace endpoint" && label.key != "end_timestamp_ns");

        Ok((timestamp, labels, endpoint_info))
    }

    fn get_mapping<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        id: u64,
    ) -> anyhow::Result<api::Mapping<'pprof>> {
        let mapping = profile_index.get_mapping(id)?;
        Ok(api::Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: profile_index.get_string(mapping.filename)?,
            build_id: profile_index.get_string(mapping.build_id)?,
        })
    }

    fn get_line<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        line: &prost_impls::Line,
    ) -> anyhow::Result<api::Line<'pprof>> {
        Ok(api::Line {
            function: Self::get_function(profile_index, line.function_id)?,
            line: line.line,
        })
    }

    fn get_location<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        id: u64,
    ) -> anyhow::Result<api::Location<'pprof>> {
        let location = profile_index.get_location(id)?;
        let mapping = Self::get_mapping(profile_index, location.mapping_id)?;
        let lines = location
            .lines
            .iter()
            .map(|line| Self::get_line(profile_index, line))
            .collect::<Result<Vec<api::Line>, _>>()?;

        anyhow::ensure!(lines.len() == 1, "expected Location to have exactly 1 Line");
        // SAFETY: checked that lines.len() == 1 right above this.
        let line = unsafe { lines.first().unwrap_unchecked() };

        Ok(api::Location {
            mapping,
            function: line.function,
            address: location.address,
            line: line.line,
        })
    }

    fn get_function<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        id: u64,
    ) -> anyhow::Result<api::Function<'pprof>> {
        let function = profile_index.get_function(id)?;
        Ok(api::Function {
            name: profile_index.get_string(function.name)?,
            system_name: profile_index.get_string(function.system_name)?,
            filename: profile_index.get_string(function.filename)?,
        })
    }

    fn sample_locations<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
        sample: &prost_impls::Sample,
    ) -> anyhow::Result<Vec<api::Location<'pprof>>> {
        let mut locations = Vec::with_capacity(sample.location_ids.len());
        for location_id in sample.location_ids.iter() {
            locations.push(Self::get_location(profile_index, *location_id)?);
        }
        Ok(locations)
    }

    fn samples<'a>(
        profile_index: &'a ProfileIndex<'pprof>,
    ) -> anyhow::Result<SamplesAndEndpointInfo<'pprof>> {
        // Find the "local root span id" and "trace endpoint" labels. If
        // they are found, then save them into a vec to replay later, and
        // drop the "trace endpoint" label from sample.
        let mut endpoints = Vec::with_capacity(1);
        let mut samples = Vec::with_capacity(profile_index.pprof.samples.len());

        for sample in profile_index.pprof.samples.iter() {
            let (timestamp, labels, endpoint) = Self::sample_labels(profile_index, sample)?;
            samples.push((
                timestamp,
                api::Sample {
                    locations: Self::sample_locations(profile_index, sample)?,
                    values: &(sample.values),
                    labels,
                },
            ));
            if let Some(endpoint_info) = endpoint {
                endpoints.push(endpoint_info)
            }
        }

        Ok((samples, endpoints))
    }
}

impl<'pprof> TryFrom<&'pprof prost_impls::Profile> for Replayer<'pprof> {
    type Error = anyhow::Error;

    fn try_from(pprof: &'pprof prost_impls::Profile) -> Result<Self, Self::Error> {
        let profile_index = ProfileIndex::try_from(pprof)?;

        let start_time = Self::start_time(pprof);
        let duration = Self::duration(pprof)?;
        let end_time = start_time.add(duration);
        let sample_types = Self::sample_types(&profile_index)?;
        let period = Self::period(&profile_index)?;
        let (samples, endpoints) = Self::samples(&profile_index)?;

        Ok(Self {
            profile_index,
            start_time,
            duration,
            end_time,
            sample_types,
            period,
            endpoints,
            samples,
        })
    }
}
