use std::io::Result;

#[cfg(feature = "generate-protobuf")]
use {
    std::env,
    std::fs::File,
    std::io::{Read, Write},
    std::path::Path,
};

// to re-generate protobuf structs, run cargo build --features generate-protobuf
fn main() -> Result<()> {
    #[cfg(feature = "generate-protobuf")]
    {
        // protoc is required to compile proto files. This uses protobuf_src to compile protoc
        // from the source, setting the env var to tell prost_build where to find it.
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());

        // compiles the .proto files into rust structs
        generate_protobuf();
    }
    Ok(())
}

#[cfg(feature = "generate-protobuf")]
fn generate_protobuf() {
    let mut config = prost_build::Config::new();

    let cur_working_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let current_working_dir_path = Path::new(&cur_working_dir);
    let output_path = current_working_dir_path.join(Path::new("src"));

    config.out_dir(output_path.clone());

    // The following prost_build config changes modify the protobuf generated structs in
    // in the following ways:

    // - annotate generated structs to use PascalCase, expected in the trace stats intake.
    //   deserialization will result in an empty stats payload otherwise (though will not explicitly fail).

    // - annotate certain Span fields so serde will use the default value of a field's type if the field
    //   doesn't exist during deserialization.

    // - handle edge case struct field names that the trace stats intake expects.
    //   example: the trace intake expects the name ContainerID rather than the PascalCase ContainerId

    config.type_attribute("TracerPayload", "#[derive(Deserialize, Serialize)]");
    config.type_attribute("TraceChunk", "#[derive(Deserialize, Serialize)]");

    config.type_attribute("Span", "#[derive(Deserialize, Serialize)]");
    config.field_attribute(
        ".pb.Span",
        "#[serde(default)] #[serde(deserialize_with = \"deserialize_null_into_default\")]",
    );

    config.type_attribute("StatsPayload", "#[derive(Deserialize, Serialize)]");
    config.type_attribute("StatsPayload", "#[serde(rename_all = \"PascalCase\")]");

    config.type_attribute("ClientStatsPayload", "#[derive(Deserialize, Serialize)]");
    config.type_attribute(
        "ClientStatsPayload",
        "#[serde(rename_all = \"PascalCase\")]",
    );
    config.type_attribute("ClientStatsBucket", "#[derive(Deserialize, Serialize)]");
    config.type_attribute("ClientStatsBucket", "#[serde(rename_all = \"PascalCase\")]");
    config.type_attribute("ClientGroupedStats", "#[derive(Deserialize, Serialize)]");
    config.type_attribute(
        "ClientGroupedStats",
        "#[serde(rename_all = \"PascalCase\")]",
    );

    config.field_attribute(".pb.ClientStatsPayload", "#[serde(default)]");
    config.field_attribute("ClientStatsBucket.agentTimeShift", "#[serde(default)]");
    config.field_attribute("ClientGroupedStats.DB_type", "#[serde(default)]");
    config.field_attribute("ClientGroupedStats.type", "#[serde(default)]");
    config.field_attribute("ClientGroupedStats.peer_service", "#[serde(default)]");
    config.field_attribute("ClientGroupedStats.span_kind", "#[serde(default)]");

    config.field_attribute(
        "ClientGroupedStats.okSummary",
        "#[serde(with = \"serde_bytes\")]",
    );
    config.field_attribute(
        "ClientGroupedStats.errorSummary",
        "#[serde(with = \"serde_bytes\")]",
    );

    config.field_attribute(
        "ClientStatsPayload.runtimeID",
        "#[serde(rename = \"RuntimeID\")]",
    );
    config.field_attribute(
        "ClientStatsPayload.containerID",
        "#[serde(rename = \"ContainerID\")]",
    );
    config.field_attribute(
        "ClientGroupedStats.HTTP_status_code",
        "#[serde(rename = \"HTTPStatusCode\")]",
    );
    config.field_attribute(
        "ClientGroupedStats.DB_type",
        "#[serde(rename = \"DBType\")]",
    );

    config
        .compile_protos(
            &[
                "src/pb/agent_payload.proto",
                "src/pb/tracer_payload.proto",
                "src/pb/span.proto",
                "src/pb/stats.proto",
            ],
            &["src/pb/"],
        )
        .unwrap();

    // add license, serde imports, custom deserializer code to the top of the protobuf rust structs file
    let add_to_top =
        "// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_null_into_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

"
        .as_bytes();

    prepend_to_file(add_to_top, &output_path.join("pb.rs"));
}

#[cfg(feature = "generate-protobuf")]
fn prepend_to_file(data: &[u8], file_path: &Path) {
    let mut f = File::open(file_path).unwrap();
    let mut content = data.to_owned();
    f.read_to_end(&mut content).unwrap();

    let mut f = File::create(file_path).unwrap();
    f.write_all(content.as_slice()).unwrap();
}
