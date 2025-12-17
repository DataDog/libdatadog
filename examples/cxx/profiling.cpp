// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <iostream>
#include <memory>
#include <fstream>
#include <format>
#include <vector>
#include <cstdlib>
#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "=== Datadog Profiling CXX Bindings Example ===" << std::endl;
        std::cout << "\nCreating Profile..." << std::endl;
        
        ValueType wall_time{
            .type_ = "wall-time",
            .unit = "nanoseconds"
        };
        
        Period period{
            .value_type = wall_time,
            .value = 60
        };
        
        auto profile = Profile::create({wall_time}, period);
        std::cout << "✅ Profile created" << std::endl;
        
        std::cout << "Adding upscaling rules..." << std::endl;
        
        // Poisson upscaling for sampled data
        std::vector<size_t> value_offsets = {0};
        profile->add_upscaling_rule_poisson(
            {value_offsets.data(), value_offsets.size()},
            "thread_id",
            "0",
            0,
            0,
            1000000
        );
        
        // Proportional upscaling (scale by factor)
        profile->add_upscaling_rule_proportional(
            {value_offsets.data(), value_offsets.size()},
            "thread_id",
            "1",
            100.0
        );
        
        std::cout << "✅ Added upscaling rules" << std::endl;
        
        std::cout << "Adding samples..." << std::endl;
        for (int i = 0; i < 100; i++) {
            // String storage must outlive add_sample() call for the profile to intern them
            std::vector<std::string> string_storage;
            string_storage.push_back(std::format("hot_function_{}", i % 3));
            string_storage.push_back(std::format("_Z12hot_function{}v", i % 3));
            string_storage.push_back(std::format("process_request_{}", i % 5));
            string_storage.push_back(std::format("_Z15process_request{}v", i % 5));
            
            Mapping mapping{
                .memory_start = 0x10000000,
                .memory_limit = 0x20000000,
                .file_offset = 0,
                .filename = "/usr/lib/libexample.so",
                .build_id = "abc123"
            };
            
            auto wall_time_value = 1000000 + (i % 1000) * 1000;
            
            if (i % 7 == 0) {
                profile->add_sample(Sample{
                    .locations = {
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
                        Location{
                            .mapping = mapping,
                            .function = Function{
                                .name = "worker_loop",
                                .system_name = "_Z11worker_loopv",
                                .filename = "/src/worker.cpp"
                            },
                            .address = 0x10000500,
                            .line = 25
                        }
                    },
                    .values = {wall_time_value},
                    .labels = {
                        Label{.key = "thread_id", .str = "", .num = int64_t(i % 4), .num_unit = ""},
                        Label{.key = "sample_id", .str = "", .num = int64_t(i), .num_unit = ""}
                    }
                });
            } else {
                profile->add_sample(Sample{
                    .locations = {
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
                        }
                    },
                    .values = {wall_time_value},
                    .labels = {
                        Label{.key = "thread_id", .str = "", .num = int64_t(i % 4), .num_unit = ""},
                        Label{.key = "sample_id", .str = "", .num = int64_t(i), .num_unit = ""}
                    }
                });
            }
        }
        
        std::cout << "✅ Added 100 samples" << std::endl;
        
        std::cout << "Adding endpoint mappings..." << std::endl;
        profile->add_endpoint(12345, "/api/users");
        profile->add_endpoint(67890, "/api/orders");
        profile->add_endpoint(11111, "/api/products");
        
        profile->add_endpoint_count("/api/users", 150);
        profile->add_endpoint_count("/api/orders", 75);
        profile->add_endpoint_count("/api/products", 200);
        std::cout << "✅ Added endpoint mappings and counts" << std::endl;
        
        // Check if we should export to Datadog or save to file
        const char* agent_url = std::getenv("DD_AGENT_URL");
        const char* api_key = std::getenv("DD_API_KEY");
        
        if (agent_url || api_key) {
            // Export to Datadog
            std::cout << "\n=== Exporting to Datadog ===" << std::endl;
            
            try {
                // Example: Create an additional file to attach (e.g., application metadata)
                std::string app_metadata = R"({
    "app_version": "1.2.3",
    "build_id": "abc123",
    "profiling_mode": "continuous",
    "sample_count": 100
})";
                std::vector<uint8_t> metadata_bytes(app_metadata.begin(), app_metadata.end());
                
                if (api_key) {
                    // Agentless mode - send directly to Datadog intake
                    const char* site = std::getenv("DD_SITE");
                    std::string dd_site = site ? site : "datadoghq.com";
                    
                    std::cout << "Creating agentless exporter (site: " << dd_site << ")..." << std::endl;
                    auto exporter = ProfileExporter::create_agentless_exporter(
                        "dd-trace-cpp",
                        "1.0.0",
                        "native",
                        {
                            Tag{.key = "service", .value = "profiling-example"},
                            Tag{.key = "env", .value = "dev"},
                            Tag{.key = "example", .value = "cxx"}
                        },
                        dd_site.c_str(),
                        api_key,
                        10000  // 10 second timeout (0 = use default)
                    );
                    std::cout << "✅ Exporter created" << std::endl;
                    
                    std::cout << "Exporting profile to Datadog with additional metadata..." << std::endl;
                    
                    exporter->send_profile(
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
                        R"({"os": "macos", "arch": "arm64", "cores": 8})"
                    );
                    std::cout << "✅ Profile exported successfully!" << std::endl;
                } else {
                    // Agent mode - send to local Datadog agent
                    std::cout << "Creating agent exporter (url: " << agent_url << ")..." << std::endl;
                    auto exporter = ProfileExporter::create_agent_exporter(
                        "dd-trace-cpp",
                        "1.0.0",
                        "native",
                        {
                            Tag{.key = "service", .value = "profiling-example"},
                            Tag{.key = "env", .value = "dev"},
                            Tag{.key = "example", .value = "cxx"}
                        },
                        agent_url,
                        10000  // 10 second timeout (0 = use default)
                    );
                    std::cout << "✅ Exporter created" << std::endl;
                    
                    std::cout << "Exporting profile to Datadog with additional metadata..." << std::endl;
                    
                    exporter->send_profile(
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
                        R"({"os": "macos", "arch": "arm64", "cores": 8})"
                    );
                    std::cout << "✅ Profile exported successfully!" << std::endl;
                }
                
            } catch (const std::exception& e) {
                std::cerr << "⚠️  Failed to export profile: " << e.what() << std::endl;
                std::cerr << "   Falling back to file export..." << std::endl;
                
                // Fall back to file export on error
                auto serialized = profile->serialize_to_vec();
                std::ofstream out("profile.pprof", std::ios::binary);
                out.write(reinterpret_cast<const char*>(serialized.data()), serialized.size());
                out.close();
                std::cout << "✅ Profile written to profile.pprof" << std::endl;
            }
        } else {
            // Save to file
            std::cout << "\n=== Saving to File ===" << std::endl;
            std::cout << "Serializing profile..." << std::endl;
            auto serialized = profile->serialize_to_vec();
            std::cout << "✅ Profile serialized to " << serialized.size() << " bytes" << std::endl;
            
            std::ofstream out("profile.pprof", std::ios::binary);
            out.write(reinterpret_cast<const char*>(serialized.data()), serialized.size());
            out.close();
            std::cout << "✅ Profile written to profile.pprof" << std::endl;
            
            std::cout << "\nℹ️  To export to Datadog instead, set environment variables:" << std::endl;
            std::cout << "   Agent mode:      DD_AGENT_URL=http://localhost:8126" << std::endl;
            std::cout << "   Agentless mode:  DD_API_KEY=<your-api-key> [DD_SITE=datadoghq.com]" << std::endl;
        }
        
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
        
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
