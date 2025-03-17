// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use env_logger::Builder;
use log::{debug, error, info};
use std::{env, str::FromStr, sync::Arc, sync::Mutex};
use tokio::{
    sync::Mutex as TokioMutex,
    time::{interval, Duration},
};
use tracing_subscriber::EnvFilter;

use datadog_trace_mini_agent::{
    aggregator::TraceAggregator,
    config, env_verifier, mini_agent, stats_flusher, stats_processor,
    trace_flusher::{self, TraceFlusher},
    trace_processor,
};

use dogstatsd::{
    aggregator::Aggregator as MetricsAggregator,
    constants::CONTEXTS,
    datadog::{MetricsIntakeUrlPrefix, Site},
    dogstatsd::{DogStatsD, DogStatsDConfig},
    flusher::{Flusher, FlusherConfig},
};

use dogstatsd::metric::EMPTY_TAGS;
use tokio_util::sync::CancellationToken;

const DOGSTATSD_FLUSH_INTERVAL: u64 = 10;
const DOGSTATSD_TIMEOUT_DURATION: Duration = Duration::from_secs(5);
const DEFAULT_DOGSTATSD_PORT: u16 = 8125;
const AGENT_HOST: &str = "0.0.0.0";

#[tokio::main]
pub async fn main() {
    let log_level = env::var("DD_LOG_LEVEL")
        .map(|val| val.to_lowercase())
        .unwrap_or("info".to_string());
    let level_filter = log::LevelFilter::from_str(&log_level).unwrap_or(log::LevelFilter::Info);
    Builder::new().filter_level(level_filter).init();

    let dd_api_key: Option<String> = env::var("DD_API_KEY").ok();
    let dd_dogstatsd_port: u16 = env::var("DD_DOGSTATSD_PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(DEFAULT_DOGSTATSD_PORT);
    let dd_site = env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".to_string());
    let dd_use_dogstatsd = env::var("DD_USE_DOGSTATSD")
        .map(|val| val.to_lowercase() != "false")
        .unwrap_or(true);

    let https_proxy = env::var("DD_PROXY_HTTPS")
        .or_else(|_| env::var("HTTPS_PROXY"))
        .ok();
    debug!("Starting serverless trace mini agent");

    let env_filter = format!("h2=off,hyper=off,rustls=off,{}", log_level);

    #[allow(clippy::expect_used)]
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_new(env_filter).expect("could not parse log level in configuration"),
        )
        .with_level(true)
        .with_thread_names(false)
        .with_thread_ids(false)
        .with_line_number(false)
        .with_file(false)
        .with_target(false)
        .without_time()
        .finish();

    #[allow(clippy::expect_used)]
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    debug!("Logging subsystem enabled");

    let env_verifier = Arc::new(env_verifier::ServerlessEnvVerifier::default());

    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {});

    let stats_flusher = Arc::new(stats_flusher::ServerlessStatsFlusher {});
    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    let config = match config::Config::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            error!("Error creating config on serverless trace mini agent startup: {e}");
            return;
        }
    };

    let trace_aggregator = Arc::new(TokioMutex::new(TraceAggregator::default()));
    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher::new(
        trace_aggregator,
        Arc::clone(&config),
    ));

    let mini_agent = Box::new(mini_agent::MiniAgent {
        config: Arc::clone(&config),
        env_verifier,
        trace_processor,
        trace_flusher,
        stats_processor,
        stats_flusher,
    });

    tokio::spawn(async move {
        let res = mini_agent.start_mini_agent().await;
        if let Err(e) = res {
            error!("Error when starting serverless trace mini agent: {e:?}");
        }
    });

    let mut metrics_flusher = if dd_use_dogstatsd {
        debug!("Starting dogstatsd");
        let (_, metrics_flusher) =
            start_dogstatsd(dd_dogstatsd_port, dd_api_key, dd_site, https_proxy).await;
        info!("dogstatsd-udp: starting to listen on port {dd_dogstatsd_port}");
        metrics_flusher
    } else {
        info!("dogstatsd disabled");
        None
    };

    let mut flush_interval = interval(Duration::from_secs(DOGSTATSD_FLUSH_INTERVAL));
    flush_interval.tick().await; // discard first tick, which is instantaneous

    loop {
        flush_interval.tick().await;

        if let Some(metrics_flusher) = metrics_flusher.as_mut() {
            debug!("Flushing dogstatsd metrics");
            metrics_flusher.flush().await;
        }
    }
}

async fn start_dogstatsd(
    port: u16,
    dd_api_key: Option<String>,
    dd_site: String,
    https_proxy: Option<String>,
) -> (CancellationToken, Option<Flusher>) {
    #[allow(clippy::expect_used)]
    let metrics_aggr = Arc::new(Mutex::new(
        MetricsAggregator::new(EMPTY_TAGS, CONTEXTS).expect("Failed to create metrics aggregator"),
    ));

    let dogstatsd_config = DogStatsDConfig {
        host: AGENT_HOST.to_string(),
        port,
    };
    let dogstatsd_cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstatsd_client = DogStatsD::new(
        &dogstatsd_config,
        Arc::clone(&metrics_aggr),
        dogstatsd_cancel_token.clone(),
    )
    .await;

    tokio::spawn(async move {
        dogstatsd_client.spin().await;
    });

    let metrics_flusher = match dd_api_key {
        Some(dd_api_key) => {
            #[allow(clippy::expect_used)]
            let metrics_flusher = Flusher::new(FlusherConfig {
                api_key: dd_api_key,
                aggregator: Arc::clone(&metrics_aggr),
                metrics_intake_url_prefix: MetricsIntakeUrlPrefix::new(
                    Some(Site::new(dd_site).expect("Failed to parse site")),
                    None,
                )
                .expect("Failed to create intake URL prefix"),
                https_proxy,
                timeout: DOGSTATSD_TIMEOUT_DURATION,
            });
            Some(metrics_flusher)
        }
        None => {
            error!("DD_API_KEY not set, won't flush metrics");
            None
        }
    };

    (dogstatsd_cancel_token, metrics_flusher)
}
