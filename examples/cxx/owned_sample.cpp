// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <array>
#include <iostream>
#include <memory>
#include <fstream>
#include <format>
#include <vector>
#ifdef __unix__
#include <time.h>
#endif
#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "Creating Profile using CXX bindings with OwnedSample..." << std::endl;
        
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
        constexpr auto value_offsets = std::array{size_t{0}};
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
        
        std::cout << "Creating SamplePool for efficient sample reuse..." << std::endl;
        
        // Create a pool of reusable samples for Wall time
        auto pool = SamplePool::create({SampleType::Wall}, 10);
        std::cout << "✅ Created SamplePool with capacity " << pool->pool_capacity() << std::endl;
        
        std::cout << "Adding samples..." << std::endl;
        for (int i = 0; i < 100; i++) {
            // Get a sample from the pool (reused if available, freshly allocated if not)
            auto owned_sample = pool->get_sample();
            
            // Set the wall time value
            auto wall_time_value = 1000000 + (i % 1000) * 1000;
            owned_sample->set_value(SampleType::Wall, wall_time_value);
            
            // Set the end time to the current time
            // This is the simplest way to set the endtime
            try {
                owned_sample->set_endtime_ns_now();
            } catch (const rust::Error& e) {
                std::cerr << "Failed to set endtime to now: " << e.what() << std::endl;
            }
            
            // Alternative: set endtime using monotonic time (Unix only)
            // This is useful if you already have a monotonic timestamp
            #ifdef __unix__
            // timespec ts;
            // clock_gettime(CLOCK_MONOTONIC, &ts);
            // auto monotonic_ns = static_cast<int64_t>(ts.tv_sec) * 1'000'000'000LL + ts.tv_nsec;
            // owned_sample->set_endtime_from_monotonic_ns(monotonic_ns);
            #endif
            
            Mapping mapping{
                .memory_start = 0x10000000,
                .memory_limit = 0x20000000,
                .file_offset = 0,
                .filename = "/usr/lib/libexample.so",
                .build_id = "abc123"
            };
            
            // Add locations - OwnedSample copies strings into its arena
            // No need for string storage since OwnedSample owns the data!
            owned_sample->add_location(Location{
                .mapping = mapping,
                .function = Function{
                    .name = std::format("hot_function_{}", i % 3),
                    .system_name = std::format("_Z12hot_function{}v", i % 3),
                    .filename = "/src/hot_path.cpp"
                },
                .address = uint64_t(0x10003000 + (i % 3) * 0x100),
                .line = 100 + (i % 3) * 10
            });
            
            owned_sample->add_location(Location{
                .mapping = mapping,
                .function = Function{
                    .name = std::format("process_request_{}", i % 5),
                    .system_name = std::format("_Z15process_request{}v", i % 5),
                    .filename = "/src/handler.cpp"
                },
                .address = uint64_t(0x10002000 + (i % 5) * 0x80),
                .line = 50 + (i % 5) * 5
            });
            
            owned_sample->add_location(Location{
                .mapping = mapping,
                .function = Function{
                    .name = "main",
                    .system_name = "main",
                    .filename = "/src/main.cpp"
                },
                .address = 0x10001000,
                .line = 42
            });
            
            // Add an extra location for some samples
            if (i % 7 == 0) {
                owned_sample->add_location(Location{
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
            
            // Demonstrate reverse_locations feature - reverse the stack trace for some samples
            // In profiling, you might want leaf-first (normal) or root-first (reversed) order
            if (i % 13 == 0) {
                owned_sample->set_reverse_locations(true);
            }
            
            // Add labels
            owned_sample->add_label(Label{
                .key = "thread_id",
                .str = "",
                .num = int64_t(i % 4),
                .num_unit = ""
            });
            
            owned_sample->add_label(Label{
                .key = "sample_id",
                .str = "",
                .num = int64_t(i),
                .num_unit = ""
            });
            
            // Add OwnedSample directly to profile
            profile->add_owned_sample(*owned_sample);
            
            // Return sample to pool for reuse (automatically resets it)
            pool->return_sample(std::move(owned_sample));
        }
        
        std::cout << "✅ Added 100 samples using SamplePool" << std::endl;
        std::cout << "   Pool now contains " << pool->pool_len() << " reusable samples" << std::endl;
        
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
        
        std::ofstream out("profile_owned_sample.pprof", std::ios::binary);
        out.write(reinterpret_cast<const char*>(serialized.data()), serialized.size());
        out.close();
        std::cout << "✅ Profile written to profile_owned_sample.pprof" << std::endl;
        
        std::cout << "Resetting profile..." << std::endl;
        profile->reset();
        std::cout << "✅ Profile reset" << std::endl;
        
        std::cout << "\n✅ Success! OwnedSample demonstrates efficient sample reuse with arena allocation." << std::endl;
        return 0;
        
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}

