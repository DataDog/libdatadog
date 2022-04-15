// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

// Simple worker that sends app-started telemetry request to the backend then exits
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut header = Default::default();
    let telemetry = ddtelemetry::build_full(&mut header).await;

    println!(
        "Payload to be sent: {}",
        serde_json::to_string_pretty(&telemetry).unwrap()
    );

    ddtelemetry::push_telemetry(&telemetry).await?;

    println!("Telemetry submitted correctly");
    Ok(())
}
