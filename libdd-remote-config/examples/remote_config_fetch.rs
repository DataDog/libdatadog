// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::Endpoint;
use libdd_remote_config::fetch::{ConfigInvariants, ConfigOptions, SingleChangesFetcher};
use libdd_remote_config::file_change_tracker::{Change, FilePath};
use libdd_remote_config::file_storage::ParsedFileStorage;
use libdd_remote_config::RemoteConfigProduct::ApmTracing;
use libdd_remote_config::{RemoteConfigParsed, Target};
use std::process::Command;
use tokio::time::sleep;

const RUNTIME_ID: &str = "23e76587-5ae1-410c-a05c-137cae600a10";
const SERVICE: &str = "testservice";
const ENV: &str = "testenv";
const VERSION: &str = "1.2.3";

fn get_hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let hostname = get_hostname();
    println!("Hostname: {hostname}");

    let dd_api_key = std::env::var("DD_API_KEY").ok();
    let dd_site = std::env::var("DD_SITE").ok();

    let (endpoint, agentless) = match (dd_api_key, dd_site) {
        (Some(api_key), Some(site)) => {
            #[cfg(feature = "agentless")]
            {
                use libdd_remote_config::fetch::AgentlessConfig;
                println!("DD_API_KEY and DD_SITE are set — enabling agentless mode (site: {site})");
                let endpoint = Endpoint::agentless(&site, api_key)
                    .expect("Failed to build agentless endpoint from DD_SITE");
                (
                    endpoint,
                    Some(AgentlessConfig {
                        hostname,
                        ..Default::default()
                    }),
                )
            }
            #[cfg(not(feature = "agentless"))]
            {
                let _ = (api_key, site);
                println!("DD_API_KEY and DD_SITE are set but agentless feature not enabled");
                (
                    Endpoint {
                        url: http::Uri::from_static("http://localhost:8126"),
                        api_key: None,
                        timeout_ms: 5000, // custom timeout, defaults to 3 seconds
                        test_token: None,
                        ..Default::default()
                    },
                    None,
                )
            }
        }
        _ => {
            println!("DD_API_KEY / DD_SITE not set — connecting to local agent");
            (
                Endpoint {
                    url: http::Uri::from_static("http://localhost:8126"),
                    api_key: None,
                    timeout_ms: 5000, // custom timeout, defaults to 3 seconds
                    test_token: None,
                    ..Default::default()
                },
                None,
            )
        }
    };

    // SingleChangesFetcher is ideal for a single static (runtime_id, service, env, version) tuple
    // Otherwise a SharedFetcher (or even a MultiTargetFetcher for a potentially high number of
    // targets) for multiple targets is needed. These can be manually wired together with a
    // ChangeTracker to keep track of changes. The SingleChangesTracker does it for you.
    let mut fetcher = SingleChangesFetcher::new(
        // Use SimpleFileStorage if you desire just the raw, unparsed contents
        // (e.g. to do processing directly in your language)
        // For more complicated use cases, like needing to store data in shared memory, a custom
        // FileStorage implementation is recommended
        ParsedFileStorage::default(),
        Target::new(
            SERVICE.to_string(),
            ENV.to_string(),
            VERSION.to_string(),
            vec!["test:value".to_string()],
            vec![],
        ),
        RUNTIME_ID.to_string(),
        ConfigOptions {
            invariants: ConfigInvariants {
                language: "awesomelang".to_string(),
                tracer_version: "99.10.5".to_string(),
                endpoint,
                agentless,
            },
            products: vec![ApmTracing],
            capabilities: vec![],
        },
    )
    .await
    .expect("Failed to create SingleChangesFetcher");

    loop {
        match fetcher.fetch_changes().await {
            Ok(changes) => {
                println!("Got {} changes:", changes.len());
                for change in changes {
                    match change {
                        Change::Add(file) => {
                            println!("Added file: {} (version: {})", file.path(), file.version());
                            print_file_contents(&file.contents());
                        }
                        Change::Update(file, _) => {
                            println!(
                                "Got update for file: {} (version: {})",
                                file.path(),
                                file.version()
                            );
                            print_file_contents(&file.contents());
                        }
                        Change::Remove(file) => {
                            println!("Removing file {}", file.path());
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Fetch failed with {e}");
            }
        }

        sleep(fetcher.get_refresh_interval()).await;
    }
}

fn print_file_contents(contents: &anyhow::Result<Option<RemoteConfigParsed>>) {
    // Note: these contents may be large. Do not actually print it fully in a non-dev env.
    match contents {
        Ok(Some(data)) => {
            println!("File contents: {data:?}");
        }
        Ok(None) => {
            println!("Unregistered product, no parsed data");
        }
        Err(e) => {
            println!("Failed parsing file: {e:?}");
        }
    }
}
