// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <iostream>
#include <memory>
#include <fstream>
#include <format>
#include <vector>
#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "Creating Profile using CXX bindings..." << std::endl;
        
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
            rust::Slice<const size_t>(value_offsets.data(), value_offsets.size()),
            "thread_id",
            "0",
            0,
            0,
            1000000
        );
        
        // Proportional upscaling (scale by factor)
        profile->add_upscaling_rule_proportional(
            rust::Slice<const size_t>(value_offsets.data(), value_offsets.size()),
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
        
        std::cout << "Serializing profile..." << std::endl;
        auto serialized = profile->serialize_to_vec();
        std::cout << "✅ Profile serialized to " << serialized.size() << " bytes" << std::endl;
        
        std::ofstream out("profile.pprof", std::ios::binary);
        out.write(reinterpret_cast<const char*>(serialized.data()), serialized.size());
        out.close();
        std::cout << "✅ Profile written to profile.pprof" << std::endl;
        
        std::cout << "Resetting profile..." << std::endl;
        profile->reset();
        std::cout << "✅ Profile reset" << std::endl;
        
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
        
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
