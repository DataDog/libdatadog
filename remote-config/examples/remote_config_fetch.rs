// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_remote_config::fetch::{ConfigInvariants, SingleChangesFetcher};
use datadog_remote_config::file_change_tracker::{Change, FilePath};
use datadog_remote_config::file_storage::ParsedFileStorage;
use datadog_remote_config::RemoteConfigProduct::ApmTracing;
use datadog_remote_config::{RemoteConfigData, Target};
use ddcommon::tag::Tag;
use ddcommon_net1::Endpoint;
use std::time::Duration;
use tokio::time::sleep;

const RUNTIME_ID: &str = "23e76587-5ae1-410c-a05c-137cae600a10";
const SERVICE: &str = "testservice";
const ENV: &str = "testenv";
const VERSION: &str = "1.2.3";

#[tokio::main(flavor = "current_thread")]
async fn main() {
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
        Target {
            service: SERVICE.to_string(),
            env: ENV.to_string(),
            app_version: VERSION.to_string(),
            tags: vec![Tag::new("test", "value").unwrap()],
        },
        RUNTIME_ID.to_string(),
        ConfigInvariants {
            language: "awesomelang".to_string(),
            tracer_version: "99.10.5".to_string(),
            endpoint: Endpoint {
                url: hyper::Uri::from_static("http://localhost:8126"),
                api_key: None,
                timeout_ms: 5000, // custom timeout, defaults to 3 seconds
                test_token: None,
            },
            products: vec![ApmTracing],
            capabilities: vec![],
        },
    );

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

        sleep(Duration::from_secs(1)).await;
    }
}

fn print_file_contents(contents: &anyhow::Result<RemoteConfigData>) {
    // Note: these contents may be large. Do not actually print it fully in a non-dev env.
    match contents {
        Ok(data) => {
            println!("File contents: {:?}", data);
        }
        Err(e) => {
            println!("Failed parsing file: {:?}", e);
        }
    }
}
