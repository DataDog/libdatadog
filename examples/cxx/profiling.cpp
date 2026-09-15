// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <array>
#include <cstdint>
#include <iostream>
#include <fstream>
#include <optional>
#include <string>
#include <vector>
#include <cstdlib>
#include "datadog/profiling.hpp"

using namespace datadog::profiling;

namespace {

std::optional<rust::Box<ProfileExporter>> create_exporter(const char* agent_url, const char* api_key) {
    if (api_key) {
        // Agentless mode - send directly to Datadog intake
        const char* site = std::getenv("DD_SITE");
        std::string dd_site = site ? site : "datadoghq.com";
        std::cout << "Creating agentless exporter (site: " << dd_site << ")..." << std::endl;
        auto exporter_result = ProfileExporter::create_agentless_exporter(
            "dd-trace-cpp", "1.0.0", "native",
            {
                Tag{.key = "service", .value = "profiling-example"},
                Tag{.key = "env", .value = "dev"},
                Tag{.key = "example", .value = "cxx"}
            },
            dd_site.c_str(), api_key, 10000, false
        );
        if (!exporter_result->check_and_print()) return std::nullopt;
        return exporter_result->take_value();
    }

    if (agent_url) {
        // Agent mode - send to local Datadog agent
        std::cout << "Creating agent exporter (url: " << agent_url << ")..." << std::endl;
        auto exporter_result = ProfileExporter::create_agent_exporter(
            "dd-trace-cpp", "1.0.0", "native",
            {
                Tag{.key = "service", .value = "profiling-example"},
                Tag{.key = "env", .value = "dev"},
                Tag{.key = "example", .value = "cxx"}
            },
            agent_url, 10000, false
        );
        if (!exporter_result->check_and_print()) return std::nullopt;
        return exporter_result->take_value();
    }

    // File mode - dump HTTP request for debugging/testing
    std::cout << "Creating file exporter (profile_dump.txt)..." << std::endl;
    auto exporter_result = ProfileExporter::create_file_exporter(
        "dd-trace-cpp", "1.0.0", "native",
        {
            Tag{.key = "service", .value = "profiling-example"},
            Tag{.key = "env", .value = "dev"},
            Tag{.key = "example", .value = "cxx"}
        },
        "profile_dump.txt"
    );
    if (!exporter_result->check_and_print()) return std::nullopt;
    return exporter_result->take_value();
}

}  // namespace

int main() {
    try {
        std::cout << "=== Datadog Profiling CXX Bindings Example ===" << std::endl;
        std::cout << "\nCreating Profile..." << std::endl;
        
        Period period{
            .value_type = SampleType::WallTime,
            .value = 60
        };
        
        // Create profile with predefined sample types. For prototyping a type
        // not yet in SampleType, use Custom1..Custom5 and configure the slot
        // with Profile::set_custom_sample_type before serialization.
        auto profile_result = Profile::create({SampleType::WallTime}, period);
        if (!profile_result->check_and_print()) return 1;
        auto profile = profile_result->take_value();
        profile->set_error_policy(ErrorPolicy::PrintImmediately);
        std::cout << "✅ Profile created" << std::endl;
        
        std::cout << "Adding upscaling rules..." << std::endl;
        
        // Poisson upscaling for sampled data
        std::vector<size_t> value_offsets = {0};
        if (!profile->add_upscaling_rule_poisson(
            {value_offsets.data(), value_offsets.size()},
            "thread_id",
            "0",
            0,
            0,
            1000000
        )) return 1;
        
        // Proportional upscaling (scale by factor)
        if (!profile->add_upscaling_rule_proportional(
            {value_offsets.data(), value_offsets.size()},
            "thread_id",
            "1",
            100.0
        )) return 1;
        
        std::cout << "✅ Added upscaling rules" << std::endl;
        
        std::cout << "Adding samples..." << std::endl;
        for (int i = 0; i < 100; i++) {
            // String storage must outlive add_sample() call for the profile to intern them
            std::vector<std::string> string_storage;
            string_storage.push_back("hot_function_" + std::to_string(i % 3));
            string_storage.push_back("_Z12hot_function" + std::to_string(i % 3) + "v");
            string_storage.push_back("process_request_" + std::to_string(i % 5));
            string_storage.push_back("_Z15process_request" + std::to_string(i % 5) + "v");
            
            Mapping mapping{
                .memory_start = 0x10000000,
                .memory_limit = 0x20000000,
                .file_offset = 0,
                .filename = "/usr/lib/libexample.so",
                .build_id = "abc123"
            };
            
            auto wall_time_value = 1000000 + (i % 1000) * 1000;
            
            std::vector<Location> locations{
                Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = string_storage[0],
                        .system_name = string_storage[1],
                        .filename = "/src/hot_path.cpp"
                    },
                    .address = uint64_t(0x10003000 + (i % 3) * 0x100),
                    .line = 100 + (i % 3) * 10
                },
                Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = string_storage[2],
                        .system_name = string_storage[3],
                        .filename = "/src/handler.cpp"
                    },
                    .address = uint64_t(0x10002000 + (i % 5) * 0x80),
                    .line = 50 + (i % 5) * 5
                },
                Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "main",
                        .system_name = "main",
                        .filename = "/src/main.cpp"
                    },
                    .address = 0x10001000,
                    .line = 42
                },
            };
            if (i % 7 == 0) {
                locations.push_back(Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "worker_loop",
                        .system_name = "_Z11worker_loopv",
                        .filename = "/src/worker.cpp"
                    },
                    .address = 0x10000500,
                    .line = 25
                });
            }

            std::array<int64_t, 1> values{wall_time_value};
            std::array<Label, 2> labels{
                Label{.key = "thread_id", .str = "", .num = int64_t(i % 4), .num_unit = ""},
                Label{.key = "sample_id", .str = "", .num = int64_t(i), .num_unit = ""},
            };
            if (!profile->add_sample(views::sample(locations, values, labels))) return 1;
        }
        
        std::cout << "✅ Added 100 samples" << std::endl;
        
        std::cout << "Adding endpoint mappings..." << std::endl;
        if (!profile->add_endpoint(12345, "/api/users")) return 1;
        if (!profile->add_endpoint(67890, "/api/orders")) return 1;
        if (!profile->add_endpoint(11111, "/api/products")) return 1;
        
        if (!profile->add_endpoint_count("/api/users", 150)) return 1;
        if (!profile->add_endpoint_count("/api/orders", 75)) return 1;
        if (!profile->add_endpoint_count("/api/products", 200)) return 1;
        std::cout << "✅ Added endpoint mappings and counts" << std::endl;
        
        // Create exporter based on environment variables
        const char* agent_url = std::getenv("DD_AGENT_URL");
        const char* api_key = std::getenv("DD_API_KEY");
        
        std::cout << "\n=== Creating Exporter ===" << std::endl;
        
        // Create appropriate exporter based on configuration
        try {
            auto exporter = create_exporter(agent_url, api_key);
            if (!exporter) return 1;
            std::cout << "✅ Exporter created" << std::endl;
            
            // Create a cancellation token for the export
            // In a real application, you could clone this and cancel from another thread
            // Example: auto token_clone = cancel_token->clone(); token_clone->cancel();
            auto cancel_token = CancellationToken::create();
            
            // Prepare metadata (same for all export modes)
            std::string app_metadata = R"({
    "app_version": "1.2.3",
    "build_id": "abc123",
    "profiling_mode": "continuous",
    "sample_count": 100
})";
            std::vector<uint8_t> metadata_bytes(app_metadata.begin(), app_metadata.end());
            
            // Export the profile (unified code path)
            std::cout << "Exporting profile with additional metadata..." << std::endl;
            if (!(*exporter)->send_profile_with_cancellation(
                *profile,
                // Files to compress and attach
                {AttachmentFile{
                    .name = "app_metadata.json",
                    .data = {metadata_bytes.data(), metadata_bytes.size()}
                }},
                // Additional per-profile tags
                {
                    Tag{.key = "export_id", .value = "12345"},
                    Tag{.key = "host", .value = "example-host"}
                },
                // Process-level tags (comma-separated)
                "language:cpp,profiler_version:1.0,runtime:native",
                // Internal metadata (JSON string)
                R"({"profiler_version": "1.0", "custom_field": "demo"})",
                // System info (JSON string)
                R"({"os": "macos", "arch": "arm64", "cores": 8})",
                *cancel_token
            ).check_and_print()) return 1;
            std::cout << "✅ Profile exported successfully!" << std::endl;

            // Split serialize/send flow: useful when callers need to reset the
            // profile under their own lock, release that lock, and upload the
            // already-encoded profile later.
            std::cout << "Exporting a second profile with split serialize/send..." << std::endl;
            auto split_profile_result = Profile::create({SampleType::WallTime}, period);
            if (!split_profile_result->check_and_print()) return 1;
            auto split_profile = split_profile_result->take_value();
            split_profile->set_error_policy(ErrorPolicy::PrintImmediately);
            Mapping split_mapping{
                .memory_start = 0x30000000,
                .memory_limit = 0x40000000,
                .file_offset = 0,
                .filename = "/usr/lib/libsplit-example.so",
                .build_id = "split-build-id"
            };
            std::array<Location, 1> split_locations{Location{
                .mapping = split_mapping,
                .function = Function{
                    .name = "split_export_function",
                    .system_name = "_Z21split_export_functionv",
                    .filename = "/src/split_export.cpp"
                },
                .address = 0x30001234,
                .line = 77
            }};
            std::array<int64_t, 1> split_values{42'000'000};
            std::array<Label, 1> split_labels{Label{.key = "thread_id", .str = "", .num = 1, .num_unit = ""}};
            if (!split_profile->add_sample(views::sample(split_locations, split_values, split_labels))) return 1;
            if (!split_profile->add_endpoint_count("/api/split-export", 1)) return 1;

            auto encoded_profile_result = split_profile->serialize();
            if (!encoded_profile_result->ok()) return 1;
            auto encoded_profile = encoded_profile_result->take_value();
            auto encoded_bytes = encoded_profile->bytes();
            std::cout << "ℹ️  Split profile serialized to " << encoded_bytes.size() << " compressed bytes" << std::endl;

            if (!(*exporter)->send_encoded_profile(
                std::move(encoded_profile),
                {},
                {Tag{.key = "export_flow", .value = "split"}},
                "language:cpp,profiler_version:1.0,runtime:native",
                R"({"profiler_version": "1.0", "export_flow": "split"})",
                R"({"os": "macos", "arch": "arm64", "cores": 8})"
            ).check_and_print()) return 1;
            std::cout << "✅ Split profile exported successfully!" << std::endl;
            
            // Print mode-specific info
            if (!agent_url && !api_key) {
                std::cout << "ℹ️  HTTP request written to profile_dump.txt" << std::endl;
                std::cout << "ℹ️  Use the utils in libdd-profiling to parse the HTTP dump" << std::endl;
                std::cout << "\nℹ️  To export to Datadog instead, set environment variables:" << std::endl;
                std::cout << "   Agent mode:      DD_AGENT_URL=http://localhost:8126" << std::endl;
                std::cout << "   Agentless mode:  DD_API_KEY=<your-api-key> [DD_SITE=datadoghq.com]" << std::endl;
            }
        } catch (const std::exception& e) {
            std::cerr << "⚠️  Failed to export profile: " << e.what() << std::endl;
        }
        
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
        
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
