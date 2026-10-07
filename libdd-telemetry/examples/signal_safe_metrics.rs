// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    convert::Infallible,
    future::Future,
    hint::black_box,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    process::ExitCode,
    str,
    time::UNIX_EPOCH,
};

use embedded_io::ErrorType;
use embedded_io_async::{Read, Write};
use libdd_telemetry::{
    data::{
        Application, Host,
        metrics::{MetricNamespace, MetricType},
    },
    signal_safe::{MetricSeriesRef, MetricsRequest, TagRef, send_metrics},
};
use reqwless::client::{HttpConnection, HttpResource};

const AGENT_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";
const SUCCESS_RESPONSE: &[u8] = b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n";

static ALLOCATOR_CALLS: AtomicUsize = AtomicUsize::new(0);
static FORBID_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static FORBIDDEN_CALLS: AtomicUsize = AtomicUsize::new(0);

struct TracingAllocator;

impl TracingAllocator {
    fn record_call() {
        ALLOCATOR_CALLS.fetch_add(1, Ordering::Relaxed);
        if FORBID_ALLOCATIONS.load(Ordering::Relaxed) {
            FORBIDDEN_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for TracingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record_call();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record_call();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::record_call();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record_call();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: TracingAllocator = TracingAllocator;

struct AllocationGuard;

impl AllocationGuard {
    fn new() -> Self {
        FORBID_ALLOCATIONS.store(true, Ordering::Relaxed);
        Self
    }
}

impl Drop for AllocationGuard {
    fn drop(&mut self) {
        FORBID_ALLOCATIONS.store(false, Ordering::Relaxed);
    }
}

struct FixedConnection {
    response: &'static [u8],
    response_offset: usize,
    request: [u8; 4_096],
    request_len: usize,
}

impl FixedConnection {
    const fn new(response: &'static [u8]) -> Self {
        Self {
            response,
            response_offset: 0,
            request: [0; 4_096],
            request_len: 0,
        }
    }

    fn request(&self) -> &[u8] {
        &self.request[..self.request_len]
    }
}

impl ErrorType for FixedConnection {
    type Error = Infallible;
}

impl Read for FixedConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let remaining = &self.response[self.response_offset..];
        let length = remaining.len().min(buffer.len());
        buffer[..length].copy_from_slice(&remaining[..length]);
        self.response_offset += length;
        Ok(length)
    }
}

impl Write for FixedConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let remaining = self.request.len() - self.request_len;
        let length = remaining.min(buffer.len());
        self.request[self.request_len..self.request_len + length]
            .copy_from_slice(&buffer[..length]);
        self.request_len += length;
        Ok(length)
    }
}

fn main() -> ExitCode {
    let allocator_calls_before_probe = ALLOCATOR_CALLS.load(Ordering::Relaxed);
    let probe = Box::new(0_u8);
    black_box(&probe);
    let allocator_traced_probe =
        ALLOCATOR_CALLS.load(Ordering::Relaxed) > allocator_calls_before_probe;
    drop(probe);
    if !allocator_traced_probe {
        eprintln!("the allocation guard did not observe the probe allocation");
        return ExitCode::FAILURE;
    }

    let timestamp = UNIX_EPOCH
        .elapsed()
        .map_or(0, |duration| duration.as_secs());
    let application = Application {
        service_name: "libdd-telemetry-example".to_owned(),
        language_name: "rust".to_owned(),
        language_version: "unknown".to_owned(),
        tracer_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..Application::default()
    };
    let host = Host {
        hostname: "unknown_hostname".to_owned(),
        ..Host::default()
    };
    let tags = match TagRef::new("component", "signal-safe") {
        Ok(tag) => [tag],
        Err(error) => {
            eprintln!("invalid tag: {error}");
            return ExitCode::FAILURE;
        }
    };
    let points = [(timestamp, 1.0)];
    let series = [MetricSeriesRef {
        namespace: MetricNamespace::Telemetry,
        metric: "metrics_submissions",
        points: &points,
        tags: &tags,
        common: false,
        metric_type: MetricType::Count,
        interval: 10,
    }];
    let request = MetricsRequest {
        tracer_time: timestamp,
        runtime_id: "00000000-0000-0000-0000-000000000000",
        seq_id: 0,
        application: &application,
        host: &host,
        origin: None,
        series: &series,
    };
    let mut connection = FixedConnection::new(SUCCESS_RESPONSE);
    let mut body_buffer = [0_u8; 2_048];
    let mut response_buffer = [0_u8; 256];

    let forbidden_calls_before = FORBIDDEN_CALLS.load(Ordering::Relaxed);
    let result = {
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        let guard = AllocationGuard::new();
        let result = block_on(send_metrics(
            &mut resource,
            AGENT_PATH,
            &request,
            &[],
            &mut body_buffer,
            &mut response_buffer,
        ));
        drop(guard);
        result
    };
    let forbidden_calls = FORBIDDEN_CALLS
        .load(Ordering::Relaxed)
        .saturating_sub(forbidden_calls_before);
    if forbidden_calls != 0 {
        eprintln!("telemetry submission made {forbidden_calls} forbidden allocator calls");
        return ExitCode::FAILURE;
    }

    let status = match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("telemetry metric submission failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let written = match str::from_utf8(connection.request()) {
        Ok(written) => written,
        Err(error) => {
            eprintln!("telemetry submission produced invalid UTF-8: {error}");
            return ExitCode::FAILURE;
        }
    };
    if status != 202
        || !written.starts_with("POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n")
        || !written.contains("dd-client-library-language: rust\r\n")
        || !written.ends_with("]}}")
    {
        eprintln!("telemetry submission produced an unexpected HTTP request");
        return ExitCode::FAILURE;
    }

    println!("telemetry metric submitted without allocator calls, status={status}");
    ExitCode::SUCCESS
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}
