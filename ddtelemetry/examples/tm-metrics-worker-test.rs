// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{error::Error, time::Duration, time::Instant};

use ddcommon::tag;
use ddtelemetry::{data, worker};

macro_rules! timeit {
    ($op_name:literal, $op:block) => {{
        let start = std::time::Instant::now();
        let res = $op;
        let delta = start.elapsed();
        println!(
            concat!($op_name, " took {} ms"),
            delta.as_secs_f64() * 1000.0
        );
        res
    }};
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "tracing")]
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let mut builder = worker::TelemetryWorkerBuilder::new(
        "paul-mac".into(),
        "test_rust".into(),
        "rust".into(),
        "1.56".into(),
        "none".into(),
    );
    builder.config.telemetry_debug_logging_enabled = Some(true);
    builder.config.endpoint = Some(ddcommon_net1::Endpoint::from_slice(
        "file://./tm-metrics-worker-test.output",
    ));
    builder.config.telemetry_hearbeat_interval = Some(Duration::from_secs(1));

    let handle = builder.run_metrics_logs()?;

    let ping_metric = handle.register_metric_context(
        "test_telemetry.ping".into(),
        Vec::new(),
        data::metrics::MetricType::Count,
        false,
        data::metrics::MetricNamespace::Telemetry,
    );

    let dist_metric = handle.register_metric_context(
        "test_telemetry.dist".into(),
        Vec::new(),
        data::metrics::MetricType::Distribution,
        true,
        data::metrics::MetricNamespace::Telemetry,
    );

    handle.send_start().unwrap();

    handle.add_point(1.0, &ping_metric, Vec::new()).unwrap();

    handle.add_point(1.0, &dist_metric, Vec::new()).unwrap();
    handle.add_point(2.0, &dist_metric, Vec::new()).unwrap();

    let tags = vec![tag!("foo", "bar")];
    handle.add_point(2.0, &ping_metric, tags.clone()).unwrap();
    handle.add_point(1.8, &dist_metric, tags).unwrap();

    handle
        .add_log(
            "init.log",
            "Hello there!".into(),
            data::LogLevel::Debug,
            None,
        )
        .unwrap();

    timeit!("sleep", {
        std::thread::sleep(std::time::Duration::from_secs(11));
    });

    handle
        .add_log(
            "init.log",
            "Hello there!".into(),
            data::LogLevel::Debug,
            None,
        )
        .unwrap();
    handle
        .add_log(
            "init.log",
            "Another log, with the same logging identifier".into(),
            data::LogLevel::Debug,
            None,
        )
        .unwrap();
    handle
        .add_log(
            "exception.log",
            "Something really bad happened".into(),
            data::LogLevel::Error,
            Some("At line 56".into()),
        )
        .unwrap();

    handle.add_point(2.0, &ping_metric, Vec::new()).unwrap();
    handle.add_point(2.3, &dist_metric, Vec::new()).unwrap();

    // About 200ms (the time it takes to send a app-closing request)
    timeit!("shutdown", {
        handle.send_stop().unwrap();
        handle.wait_for_shutdown_deadline(Instant::now() + Duration::from_millis(10));
    });

    Ok(())
}
