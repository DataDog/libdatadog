// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(unreachable_code)]
#![allow(unused)]

//! Benchmark for agentless Remote Config fetching.
//!
//! Measures, for three distinct phases, three quantities each:
//!
//! 1. Client init:    `SingleChangesFetcher::new`, which performs the TUF root bootstrap when
//!    running in agentless mode.
//! 2. Initial fetch:  the first call to `fetch_changes` on a freshly built client.
//! 3. Refetch:        the second call to `fetch_changes`, with the client already warm.
//!
//! For each phase we report:
//!
//! * Wall-clock time:   total elapsed time, end-to-end.
//! * Poll/CPU time:     sum of time spent inside `Future::poll` calls on the current thread. This
//!   is an approximation of the active computation time (parsing, TUF verification, request
//!   building, response decoding, ...).
//! * Await/IO time:     `wall - poll`, the time the future spent suspended waiting for IO (DNS,
//!   TCP, TLS handshake, server response, ...).
//!
//! The instrumentation works by polling the benchmarked future manually on a `current_thread`
//! tokio runtime and accumulating the duration of each `poll()` invocation. No additional
//! dependencies are required.
//!
//! Usage:
//!     DD_API_KEY=... DD_SITE=datadoghq.com \
//!         cargo run --release --example remote_config_agentless_bench \
//!             -p libdd-remote-config --features agentless
//!
//! Without `DD_API_KEY` / `DD_SITE`, this example exits — agentless mode is required.

use libdd_common::Endpoint;
use libdd_remote_config::fetch::{ConfigInvariants, ConfigOptions, SingleChangesFetcher};
use libdd_remote_config::file_storage::ParsedFileStorage;
use libdd_remote_config::RemoteConfigProduct::ApmTracing;
use libdd_remote_config::Target;
use std::future::Future;
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

#[cfg(feature = "agentless")]
use libdd_remote_config::fetch::AgentlessConfig;

const RUNTIME_ID: &str = "23e76587-5ae1-410c-a05c-137cae600a10";
const SERVICE: &str = "bench-service";
const ENV: &str = "bench-env";
const VERSION: &str = "1.2.3";

fn get_hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A future wrapper that accumulates the time spent inside each `poll()` call into
/// `*poll_time`. The wall time is the elapsed between calling `Instrumented::new` (or just
/// before `.await`) and the future completing.
struct Instrumented<'a, F> {
    inner: F,
    poll_time: &'a mut Duration,
}

impl<'a, F: Future + Unpin> Instrumented<'a, F> {
    fn new(inner: F, poll_time: &'a mut Duration) -> Self {
        *poll_time = Duration::ZERO;
        Self { inner, poll_time }
    }
}

impl<F: Future + Unpin> Future for Instrumented<'_, F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let start = Instant::now();
        let res = Pin::new(&mut self.inner).poll(cx);
        let elapsed = start.elapsed();
        *self.poll_time += elapsed;
        res
    }
}

#[derive(Default, Clone, Copy)]
struct Sample {
    wall: Duration,
    poll: Duration,
}

impl Sample {
    fn io(&self) -> Duration {
        self.wall.saturating_sub(self.poll)
    }
}

fn print_row(label: &str, s: Sample) {
    let wall_ms = s.wall.as_secs_f64() * 1000.0;
    let poll_ms = s.poll.as_secs_f64() * 1000.0;
    let io_ms = s.io().as_secs_f64() * 1000.0;
    let poll_pct = if s.wall.as_nanos() > 0 {
        100.0 * s.poll.as_secs_f64() / s.wall.as_secs_f64()
    } else {
        0.0
    };
    println!(
        "  {label:<18}  wall = {wall_ms:>9.3} ms   poll/CPU = {poll_ms:>9.3} ms   \
         await/IO = {io_ms:>9.3} ms   ({poll_pct:>5.1}% poll)"
    );
}

async fn run_one_iteration(
    iter: usize,
    endpoint: Endpoint,
    #[cfg(feature = "agentless")] agentless: Option<AgentlessConfig>,
    #[cfg(not(feature = "agentless"))] agentless: Option<std::convert::Infallible>,
) -> anyhow::Result<(Sample, Sample, Sample)> {
    let target = Target::new(
        SERVICE.to_string(),
        ENV.to_string(),
        VERSION.to_string(),
        vec!["bench:true".to_string()],
        vec![],
    );

    let options = ConfigOptions {
        invariants: ConfigInvariants {
            language: "benchlang".to_string(),
            tracer_version: "0.0.1".to_string(),
            endpoint,
            agentless,
        },
        products: vec![ApmTracing],
        capabilities: vec![],
    };

    // --- 1. Client init: TUF root bootstrap in agentless mode ---
    let mut init_poll = Duration::ZERO;
    let init_wall_start = Instant::now();
    let fetcher_fut = Box::pin(SingleChangesFetcher::new(
        ParsedFileStorage::default(),
        target,
        RUNTIME_ID.to_string(),
        options,
    ));
    let mut fetcher = Instrumented::new(fetcher_fut, &mut init_poll).await?;
    let init = Sample {
        wall: init_wall_start.elapsed(),
        poll: init_poll,
    };

    // --- 2. Initial fetch: first call to fetch_changes on a fresh client ---
    let mut first_poll = Duration::ZERO;
    let first_wall_start = Instant::now();
    // R is inferred from `ParsedFileStorage`'s `UpdatedFiles` impl.
    let first_fut = Box::pin(fetcher.fetch_changes());
    let first_changes: Vec<_> = Instrumented::new(first_fut, &mut first_poll).await?;
    let first = Sample {
        wall: first_wall_start.elapsed(),
        poll: first_poll,
    };

    // --- 3. Refetch: second call to fetch_changes (warm client) ---
    let mut refetch_poll = Duration::ZERO;
    let refetch_wall_start = Instant::now();
    let refetch_fut = Box::pin(fetcher.fetch_changes());
    let refetch_changes: Vec<_> = Instrumented::new(refetch_fut, &mut refetch_poll).await?;
    let refetch = Sample {
        wall: refetch_wall_start.elapsed(),
        poll: refetch_poll,
    };

    println!(
        "Iteration #{iter}: initial fetch returned {} change(s), refetch returned {} change(s)",
        first_changes.len(),
        refetch_changes.len(),
    );

    Ok((init, first, refetch))
}

#[cfg(feature = "agentless")]
async fn agentless_main() -> anyhow::Result<()> {
    let hostname = get_hostname();
    println!("Hostname: {hostname}");

    let dd_api_key = std::env::var("DD_API_KEY").ok();
    let dd_site = std::env::var("DD_SITE").ok();
    let iterations: usize = std::env::var("BENCH_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    let (endpoint, agentless): (Endpoint, Option<_>) = match (dd_api_key, dd_site) {
        #[cfg(feature = "agentless")]
        (Some(api_key), Some(site)) => {
            println!("Agentless mode enabled (site: {site})");
            let endpoint = Endpoint::agentless(&site, api_key)
                .expect("Failed to build agentless endpoint from DD_SITE");
            (
                endpoint,
                Some(AgentlessConfig {
                    hostname: hostname.clone(),
                    ..Default::default()
                }),
            )
        }
        #[cfg(not(feature = "agentless"))]
        (Some(_), Some(_)) => {
            eprintln!(
                "This benchmark requires the `agentless` feature. \
                 Re-run with: --features agentless"
            );
            std::process::exit(1);
        }
        _ => {
            eprintln!(
                "DD_API_KEY and DD_SITE are required for the agentless benchmark.\n\
                 Example:\n  DD_API_KEY=... DD_SITE=datadoghq.com \\\n    \
                 cargo run --release --example remote_config_agentless_bench \\\n    \
                 -p libdd-remote-config --features agentless"
            );
            std::process::exit(1);
        }
    };

    println!("Running {iterations} iteration(s)\n");

    let mut inits = Vec::with_capacity(iterations);
    let mut firsts = Vec::with_capacity(iterations);
    let mut refetches = Vec::with_capacity(iterations);

    for i in 0..iterations {
        match run_one_iteration(i, endpoint.clone(), agentless.clone()).await {
            Ok((init, first, refetch)) => {
                print_row("  client init", init);
                print_row("  initial fetch", first);
                print_row("  refetch", refetch);
                println!();
                inits.push(init);
                firsts.push(first);
                refetches.push(refetch);
            }
            Err(e) => {
                eprintln!("Iteration {i} failed: {e:?}");
            }
        }
    }

    if inits.is_empty() {
        anyhow::bail!("All iterations failed");
    }

    println!(
        "=== Summary over {} successful iteration(s) ===",
        inits.len()
    );
    print_summary("client init", &inits);
    print_summary("initial fetch", &firsts);
    print_summary("refetch", &refetches);

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "agentless")]
    agentless_main().await?;
    Ok(())
}

fn print_summary(label: &str, samples: &[Sample]) {
    let n = samples.len() as u32;
    let sum_wall: Duration = samples.iter().map(|s| s.wall).sum();
    let sum_poll: Duration = samples.iter().map(|s| s.poll).sum();
    let avg = Sample {
        wall: sum_wall / n,
        poll: sum_poll / n,
    };

    let min_wall = samples.iter().map(|s| s.wall).min().unwrap();
    let max_wall = samples.iter().map(|s| s.wall).max().unwrap();
    let min_poll = samples.iter().map(|s| s.poll).min().unwrap();
    let max_poll = samples.iter().map(|s| s.poll).max().unwrap();

    println!("{label}:");
    print_row("avg", avg);
    print_row(
        "min",
        Sample {
            wall: min_wall,
            poll: min_poll,
        },
    );
    print_row(
        "max",
        Sample {
            wall: max_wall,
            poll: max_poll,
        },
    );
}
