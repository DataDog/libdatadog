// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use libdd_shared_runtime::{BasicRuntime, BlockingRuntime, SharedRuntime, Worker, WorkerHandle};
use prost::Message;

use crate::KeyValue;
use crate::attribute::normalize_attributes;
use crate::config::{OtlpExporterConfig, Temporality};
use crate::error::{BuildWarning, OtelMetricsError};
use crate::exporter::{DatadogMetricExporter, build_datadog_metric_exporter};
use crate::instrument::{InstrumentDescriptor, InstrumentId, InstrumentKind, ObservableCallback};
use crate::resource::ResourceBuilder;
use crate::wire::{self, AggregationTemporality};

const DEFAULT_HISTOGRAM_BOUNDS: [f64; 15] = [
    0.0, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0, 250.0, 500.0, 750.0, 1_000.0, 2_500.0, 5_000.0,
    7_500.0, 10_000.0,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportCounters {
    pub metrics_export_attempts: u64,
    pub metrics_export_successes: u64,
    pub metrics_export_failures: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) attempts: AtomicU64,
    pub(crate) successes: AtomicU64,
    pub(crate) failures: AtomicU64,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> ExportCounters {
        ExportCounters {
            metrics_export_attempts: self.attempts.load(Ordering::Relaxed),
            metrics_export_successes: self.successes.load(Ordering::Relaxed),
            metrics_export_failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
struct HistogramState {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    bucket_counts: Vec<u64>,
}

impl HistogramState {
    fn new(value: f64) -> Self {
        let mut state = Self {
            count: 0,
            sum: 0.0,
            min: value,
            max: value,
            bucket_counts: vec![0; DEFAULT_HISTOGRAM_BOUNDS.len() + 1],
        };
        state.record(value);
        state
    }

    fn record(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        let bucket = DEFAULT_HISTOGRAM_BOUNDS
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(DEFAULT_HISTOGRAM_BOUNDS.len());
        self.bucket_counts[bucket] = self.bucket_counts[bucket].saturating_add(1);
    }
}

#[derive(Debug, Clone)]
enum Aggregate {
    Sum(f64),
    Gauge(f64),
    Histogram(HistogramState),
}

struct RegisteredInstrument {
    descriptor: InstrumentDescriptor,
    callback: Option<ObservableCallback>,
    points: HashMap<Vec<KeyValue>, Aggregate>,
    previous_observations: HashMap<Vec<KeyValue>, f64>,
}

struct MetricsState {
    resource: Vec<KeyValue>,
    temporality: Temporality,
    instruments: HashMap<InstrumentId, RegisteredInstrument>,
    started_at: u64,
    last_collection: u64,
}

impl MetricsState {
    fn reset_after_fork(&mut self) {
        let now = unix_nano_now();
        self.started_at = now;
        self.last_collection = now;
        for instrument in self.instruments.values_mut() {
            instrument.points.clear();
            instrument.previous_observations.clear();
        }
    }
}

struct MetricsPipeline {
    state: Arc<Mutex<MetricsState>>,
    exporter: Arc<DatadogMetricExporter>,
    export_guard: tokio::sync::Mutex<()>,
}

impl MetricsPipeline {
    async fn collect_and_export(&self) -> Result<(), OtelMetricsError> {
        let _guard = self.export_guard.lock().await;
        let Some(payload) = collect_payload(&self.state) else {
            return Ok(());
        };
        self.exporter.export(payload).await
    }
}

struct MetricsWorker {
    pipeline: Arc<MetricsPipeline>,
    interval: Duration,
}

impl std::fmt::Debug for MetricsWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MetricsWorker")
            .field("interval", &self.interval)
            .finish()
    }
}

#[async_trait]
impl Worker for MetricsWorker {
    async fn run(&mut self) {
        let _ = self.pipeline.collect_and_export().await;
    }

    async fn trigger(&mut self) {
        tokio::time::sleep(self.interval).await;
    }

    fn reset(&mut self) {
        self.pipeline
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reset_after_fork();
    }

    async fn shutdown(&mut self) {
        let _ = self.pipeline.collect_and_export().await;
    }
}

trait RuntimeControl: Send + Sync {
    fn flush(&self, pipeline: &Arc<MetricsPipeline>) -> Result<(), OtelMetricsError>;
    fn stop(&self, worker: WorkerHandle) -> Result<(), OtelMetricsError>;
}

struct RuntimeAdapter<R>(Arc<R>);

impl<R> RuntimeControl for RuntimeAdapter<R>
where
    R: BlockingRuntime + Send + Sync,
{
    fn flush(&self, pipeline: &Arc<MetricsPipeline>) -> Result<(), OtelMetricsError> {
        self.0
            .block_on(pipeline.collect_and_export())
            .map_err(|error| OtelMetricsError(error.to_string()))?
    }

    fn stop(&self, worker: WorkerHandle) -> Result<(), OtelMetricsError> {
        self.0
            .block_on(worker.stop())
            .map_err(|error| OtelMetricsError(error.to_string()))?
            .map_err(|error| OtelMetricsError(error.to_string()))
    }
}

pub struct OtelMetricsAggregatorBuilder {
    resource: ResourceBuilder,
    metrics_exporter: Option<OtlpExporterConfig>,
    temporality: Temporality,
    export_interval: Duration,
}

impl Default for OtelMetricsAggregatorBuilder {
    fn default() -> Self {
        Self {
            resource: ResourceBuilder::new(),
            metrics_exporter: None,
            temporality: Temporality::default(),
            export_interval: Duration::from_secs(60),
        }
    }
}

impl OtelMetricsAggregatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_resource(mut self, resource: ResourceBuilder) -> Self {
        self.resource = resource;
        self
    }

    pub fn with_metrics_exporter(mut self, config: OtlpExporterConfig) -> Self {
        self.metrics_exporter = Some(config);
        self
    }

    pub fn with_metrics_temporality(mut self, temporality: Temporality) -> Self {
        self.temporality = temporality;
        self
    }

    pub fn with_export_interval(mut self, interval: Duration) -> Self {
        self.export_interval = interval;
        self
    }

    pub fn build_with_default_runtime(self) -> (OtelMetricsAggregator, Vec<BuildWarning>) {
        match BasicRuntime::new() {
            Ok(runtime) => self.build(Arc::new(runtime)),
            Err(error) => {
                let now = unix_nano_now();
                (
                    OtelMetricsAggregator::without_runtime(
                        self.resource.build(),
                        self.temporality,
                        now,
                    ),
                    vec![BuildWarning::ExporterInitFailed(error.to_string())],
                )
            }
        }
    }

    pub fn build<R>(self, runtime: Arc<R>) -> (OtelMetricsAggregator, Vec<BuildWarning>)
    where
        R: BlockingRuntime + Send + Sync + 'static,
    {
        let mut warnings = Vec::new();
        let now = unix_nano_now();
        let state = Arc::new(Mutex::new(MetricsState {
            resource: self.resource.build(),
            temporality: self.temporality,
            instruments: HashMap::new(),
            started_at: now,
            last_collection: now,
        }));
        let runtime_control: Arc<dyn RuntimeControl> =
            Arc::new(RuntimeAdapter(Arc::clone(&runtime)));

        let exporter = self.metrics_exporter.as_ref().and_then(|config| {
            match build_datadog_metric_exporter(config, self.temporality) {
                Ok(exporter) => Some(Arc::new(exporter)),
                Err(warning) => {
                    warnings.push(warning);
                    None
                }
            }
        });
        let counters = exporter
            .as_ref()
            .map(|exporter| exporter.counters_handle())
            .unwrap_or_else(|| Arc::new(Counters::default()));
        let pipeline = exporter.map(|exporter| {
            Arc::new(MetricsPipeline {
                state: Arc::clone(&state),
                exporter,
                export_guard: tokio::sync::Mutex::new(()),
            })
        });
        let worker = pipeline.as_ref().and_then(|pipeline| {
            let worker = MetricsWorker {
                pipeline: Arc::clone(pipeline),
                interval: self.export_interval,
            };
            match runtime.spawn_worker(worker, true) {
                Ok(handle) => Some(handle),
                Err(error) => {
                    warnings.push(BuildWarning::ExporterInitFailed(error.to_string()));
                    None
                }
            }
        });

        (
            OtelMetricsAggregator {
                state,
                next_id: AtomicU64::new(1),
                counters,
                pipeline,
                runtime: Some(runtime_control),
                worker,
            },
            warnings,
        )
    }
}

pub struct OtelMetricsAggregator {
    state: Arc<Mutex<MetricsState>>,
    next_id: AtomicU64,
    counters: Arc<Counters>,
    pipeline: Option<Arc<MetricsPipeline>>,
    runtime: Option<Arc<dyn RuntimeControl>>,
    worker: Option<WorkerHandle>,
}

impl OtelMetricsAggregator {
    fn without_runtime(resource: Vec<KeyValue>, temporality: Temporality, now: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(MetricsState {
                resource,
                temporality,
                instruments: HashMap::new(),
                started_at: now,
                last_collection: now,
            })),
            next_id: AtomicU64::new(1),
            counters: Arc::new(Counters::default()),
            pipeline: None,
            runtime: None,
            worker: None,
        }
    }

    pub fn register_instrument(&self, descriptor: InstrumentDescriptor) -> InstrumentId {
        self.insert_instrument(descriptor, None)
    }

    pub fn register_observable_instrument(
        &self,
        descriptor: InstrumentDescriptor,
        callback: ObservableCallback,
    ) -> InstrumentId {
        self.insert_instrument(descriptor, Some(callback))
    }

    fn insert_instrument(
        &self,
        descriptor: InstrumentDescriptor,
        callback: Option<ObservableCallback>,
    ) -> InstrumentId {
        let id = InstrumentId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .instruments
            .insert(
                id,
                RegisteredInstrument {
                    descriptor,
                    callback,
                    points: HashMap::new(),
                    previous_observations: HashMap::new(),
                },
            );
        id
    }

    pub fn record(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(instrument) = state.instruments.get_mut(&id) else {
            return;
        };
        record(instrument, value, attributes);
    }

    pub fn record_counter(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        self.record(id, value, attributes);
    }

    pub fn record_up_down_counter(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        self.record(id, value, attributes);
    }

    pub fn record_histogram(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        self.record(id, value, attributes);
    }

    pub fn observe_gauge(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        self.record(id, value, attributes);
    }

    pub fn observe_counter(&self, id: InstrumentId, value: f64, attributes: &[KeyValue]) {
        self.record(id, value, attributes);
    }

    pub fn export_counters(&self) -> ExportCounters {
        self.counters.snapshot()
    }

    pub fn force_flush(&self) -> Result<(), OtelMetricsError> {
        match (&self.runtime, &self.pipeline) {
            (Some(runtime), Some(pipeline)) => runtime.flush(pipeline),
            _ => Ok(()),
        }
    }

    pub fn shutdown(mut self) -> Result<(), OtelMetricsError> {
        match (
            self.runtime.take(),
            self.worker.take(),
            self.pipeline.take(),
        ) {
            (Some(runtime), Some(worker), _) => runtime.stop(worker),
            (Some(runtime), None, Some(pipeline)) => runtime.flush(&pipeline),
            _ => Ok(()),
        }
    }
}

fn record(instrument: &mut RegisteredInstrument, value: f64, attributes: &[KeyValue]) {
    let attributes = normalize_attributes(attributes);
    match instrument.descriptor.kind {
        InstrumentKind::Counter | InstrumentKind::ObservableCounter
            if value >= 0.0 || value.is_nan() =>
        {
            add_sum(&mut instrument.points, attributes, value);
        }
        InstrumentKind::UpDownCounter | InstrumentKind::ObservableUpDownCounter => {
            add_sum(&mut instrument.points, attributes, value);
        }
        InstrumentKind::Histogram if value >= 0.0 || value.is_nan() => {
            match instrument.points.entry(attributes) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if let Aggregate::Histogram(histogram) = entry.get_mut() {
                        histogram.record(value);
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Aggregate::Histogram(HistogramState::new(value)));
                }
            }
        }
        InstrumentKind::ObservableGauge => {
            instrument
                .points
                .insert(attributes, Aggregate::Gauge(value));
        }
        _ => {}
    }
}

fn add_sum(points: &mut HashMap<Vec<KeyValue>, Aggregate>, attributes: Vec<KeyValue>, value: f64) {
    match points.entry(attributes) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if let Aggregate::Sum(sum) = entry.get_mut() {
                *sum += value;
            }
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Aggregate::Sum(value));
        }
    }
}

type ScopeIdentity = (String, Option<String>, Option<String>, Vec<KeyValue>);

fn collect_payload(state: &Arc<Mutex<MetricsState>>) -> Option<Vec<u8>> {
    let callbacks = {
        let state = state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .instruments
            .iter()
            .filter_map(|(id, instrument)| {
                instrument
                    .callback
                    .as_ref()
                    .map(|callback| (*id, Arc::clone(callback)))
            })
            .collect::<Vec<_>>()
    };
    let observations = callbacks
        .into_iter()
        .map(|(id, callback)| (id, callback()))
        .collect::<HashMap<_, _>>();

    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
    let end_time = unix_nano_now();
    let start_time = match state.temporality {
        Temporality::Delta => state.last_collection,
        Temporality::Cumulative => state.started_at,
    };
    let temporality = match state.temporality {
        Temporality::Delta => AggregationTemporality::Delta as i32,
        Temporality::Cumulative => AggregationTemporality::Cumulative as i32,
    };
    let selected_temporality = state.temporality;
    let mut scopes: HashMap<ScopeIdentity, Vec<wire::Metric>> = HashMap::new();

    for (id, instrument) in &mut state.instruments {
        let metric = if instrument.callback.is_some() {
            observable_metric(
                instrument,
                observations.get(id).map(Vec::as_slice).unwrap_or_default(),
                selected_temporality,
                temporality,
                start_time,
                end_time,
            )
        } else {
            synchronous_metric(
                instrument,
                selected_temporality,
                temporality,
                start_time,
                end_time,
            )
        };
        let Some(metric) = metric else {
            continue;
        };
        let descriptor = &instrument.descriptor;
        scopes
            .entry((
                descriptor.meter_name.clone(),
                descriptor.meter_version.clone(),
                descriptor.meter_schema_url.clone(),
                descriptor.meter_attributes.clone(),
            ))
            .or_default()
            .push(metric);
    }
    state.last_collection = end_time;
    if scopes.is_empty() {
        return None;
    }

    let scope_metrics = scopes
        .into_iter()
        .map(
            |((name, version, schema_url, attributes), metrics)| wire::ScopeMetrics {
                scope: Some(wire::instrumentation_scope(name, version, &attributes)),
                metrics,
                schema_url: schema_url.unwrap_or_default(),
            },
        )
        .collect();
    let request = wire::ExportMetricsServiceRequest {
        resource_metrics: vec![wire::ResourceMetrics {
            resource: Some(wire::resource(&state.resource)),
            scope_metrics,
            schema_url: String::new(),
        }],
    };
    Some(request.encode_to_vec())
}

fn synchronous_metric(
    instrument: &mut RegisteredInstrument,
    selected_temporality: Temporality,
    temporality: i32,
    start_time: u64,
    end_time: u64,
) -> Option<wire::Metric> {
    let points = instrument.points.clone();
    if selected_temporality == Temporality::Delta {
        match instrument.descriptor.kind {
            InstrumentKind::Counter | InstrumentKind::UpDownCounter | InstrumentKind::Histogram => {
                instrument.points.clear()
            }
            _ => {}
        }
    }
    metric_from_points(
        &instrument.descriptor,
        points,
        temporality,
        start_time,
        end_time,
    )
}

fn observable_metric(
    instrument: &mut RegisteredInstrument,
    observations: &[crate::ObservableMeasurement],
    selected_temporality: Temporality,
    temporality: i32,
    start_time: u64,
    end_time: u64,
) -> Option<wire::Metric> {
    let mut points = HashMap::new();
    for observation in observations {
        let attributes = normalize_attributes(&observation.attributes);
        let value = match instrument.descriptor.kind {
            InstrumentKind::ObservableCounter | InstrumentKind::ObservableUpDownCounter
                if selected_temporality == Temporality::Delta =>
            {
                let previous = instrument
                    .previous_observations
                    .insert(attributes.clone(), observation.value)
                    .unwrap_or(0.0);
                observation.value - previous
            }
            _ => observation.value,
        };
        let aggregate = match instrument.descriptor.kind {
            InstrumentKind::ObservableGauge => Aggregate::Gauge(value),
            _ => Aggregate::Sum(value),
        };
        points.insert(attributes, aggregate);
    }
    metric_from_points(
        &instrument.descriptor,
        points,
        temporality,
        start_time,
        end_time,
    )
}

fn metric_from_points(
    descriptor: &InstrumentDescriptor,
    points: HashMap<Vec<KeyValue>, Aggregate>,
    temporality: i32,
    start_time: u64,
    end_time: u64,
) -> Option<wire::Metric> {
    if points.is_empty() {
        return None;
    }
    let data = match descriptor.kind {
        InstrumentKind::Counter | InstrumentKind::ObservableCounter => {
            wire::metric::Data::Sum(wire::Sum {
                data_points: number_points(points, start_time, end_time),
                aggregation_temporality: temporality,
                is_monotonic: true,
            })
        }
        InstrumentKind::UpDownCounter | InstrumentKind::ObservableUpDownCounter => {
            wire::metric::Data::Sum(wire::Sum {
                data_points: number_points(points, start_time, end_time),
                aggregation_temporality: temporality,
                is_monotonic: false,
            })
        }
        InstrumentKind::ObservableGauge => wire::metric::Data::Gauge(wire::Gauge {
            data_points: number_points(points, 0, end_time),
        }),
        InstrumentKind::Histogram => wire::metric::Data::Histogram(wire::Histogram {
            data_points: histogram_points(points, start_time, end_time),
            aggregation_temporality: temporality,
        }),
    };
    Some(wire::Metric {
        name: descriptor.name.clone(),
        description: descriptor.description.clone().unwrap_or_default(),
        unit: descriptor.unit.clone().unwrap_or_default(),
        data: Some(data),
    })
}

fn number_points(
    points: HashMap<Vec<KeyValue>, Aggregate>,
    start_time: u64,
    end_time: u64,
) -> Vec<wire::NumberDataPoint> {
    points
        .into_iter()
        .filter_map(|(attributes, aggregate)| {
            let value = match aggregate {
                Aggregate::Sum(value) | Aggregate::Gauge(value) => value,
                Aggregate::Histogram(_) => return None,
            };
            Some(wire::NumberDataPoint {
                attributes: wire::key_values(&attributes),
                start_time_unix_nano: start_time,
                time_unix_nano: end_time,
                flags: 0,
                value: Some(wire::number_data_point::Value::AsDouble(value)),
            })
        })
        .collect()
}

fn histogram_points(
    points: HashMap<Vec<KeyValue>, Aggregate>,
    start_time: u64,
    end_time: u64,
) -> Vec<wire::HistogramDataPoint> {
    points
        .into_iter()
        .filter_map(|(attributes, aggregate)| {
            let Aggregate::Histogram(histogram) = aggregate else {
                return None;
            };
            Some(wire::HistogramDataPoint {
                attributes: wire::key_values(&attributes),
                start_time_unix_nano: start_time,
                time_unix_nano: end_time,
                count: histogram.count,
                sum: Some(histogram.sum),
                bucket_counts: histogram.bucket_counts,
                explicit_bounds: DEFAULT_HISTOGRAM_BOUNDS.to_vec(),
                flags: 0,
                min: Some(histogram.min),
                max: Some(histogram.max),
            })
        })
        .collect()
}

fn unix_nano_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstrumentDescriptor, InstrumentKind, ResourceBuilder};

    #[test]
    fn aggregates_and_encodes_without_opentelemetry_sdk() {
        let now = unix_nano_now();
        let state = Arc::new(Mutex::new(MetricsState {
            resource: ResourceBuilder::new().with_service("checkout").build(),
            temporality: Temporality::Delta,
            instruments: HashMap::new(),
            started_at: now,
            last_collection: now,
        }));
        let id = InstrumentId(1);
        state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .instruments
            .insert(
                id,
                RegisteredInstrument {
                    descriptor: InstrumentDescriptor::new("requests", InstrumentKind::Counter),
                    callback: None,
                    points: HashMap::new(),
                    previous_observations: HashMap::new(),
                },
            );
        {
            let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
            let instrument = locked
                .instruments
                .get_mut(&id)
                .expect("registered instrument");
            record(instrument, 2.0, &[KeyValue::new("route", "/health")]);
            record(instrument, 3.0, &[KeyValue::new("route", "/health")]);
        }

        let payload = collect_payload(&state).expect("encoded payload");
        let request = wire::ExportMetricsServiceRequest::decode(payload.as_slice())
            .expect("valid OTLP metrics protobuf");
        let metric = &request.resource_metrics[0].scope_metrics[0].metrics[0];
        let Some(wire::metric::Data::Sum(sum)) = &metric.data else {
            panic!("expected sum");
        };
        assert_eq!(sum.data_points.len(), 1);
        assert_eq!(
            sum.data_points[0].value,
            Some(wire::number_data_point::Value::AsDouble(5.0))
        );
    }
}
