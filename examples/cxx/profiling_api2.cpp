// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <cstdint>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "=== Datadog Profiling CXX api2 Example ===" << std::endl;

        Period period{
            .value_type = SampleType::WallTime,
            .value = 60,
        };

        std::cout << "Creating ProfilesDictionary..." << std::endl;
        auto dictionary = ProfilesDictionary::create();

        // Insert dictionary strings once and reuse opaque ids on every sample.
        auto mapping_filename = dictionary->insert_string("/usr/lib/libexample.so");
        auto build_id = dictionary->insert_string("abc123");
        auto hot_function_name = dictionary->insert_string("hot_function");
        auto hot_function_system_name = dictionary->insert_string("_Z12hot_functionv");
        auto hot_function_file = dictionary->insert_string("/src/hot_path.cpp");
        auto handler_function_name = dictionary->insert_string("process_request");
        auto handler_function_system_name = dictionary->insert_string("_Z15process_requestv");
        auto handler_function_file = dictionary->insert_string("/src/handler.cpp");
        auto thread_id_key = dictionary->insert_string("thread_id");
        auto sample_id_key = dictionary->insert_string("sample_id");

        auto mapping = dictionary->insert_mapping(Mapping2{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = mapping_filename,
            .build_id = build_id,
        });

        auto hot_function = dictionary->insert_function(Function2{
            .name = hot_function_name,
            .system_name = hot_function_system_name,
            .file_name = hot_function_file,
        });

        auto handler_function = dictionary->insert_function(Function2{
            .name = handler_function_name,
            .system_name = handler_function_system_name,
            .file_name = handler_function_file,
        });

        std::cout << "✅ Dictionary populated" << std::endl;

        std::cout << "Creating dictionary-backed Profile..." << std::endl;
        auto profile = Profile::create_with_dictionary({SampleType::WallTime}, period, *dictionary);
        std::cout << "✅ Profile created" << std::endl;

        std::cout << "Adding api2 samples..." << std::endl;
        for (int i = 0; i < 100; i++) {
            std::vector<Location2> locations{
                Location2{
                    .mapping = mapping,
                    .function = hot_function,
                    .address = static_cast<std::uint64_t>(0x10003000 + (i % 3) * 0x100),
                    .line = static_cast<std::int64_t>(100 + (i % 3) * 10),
                },
                Location2{
                    .mapping = mapping,
                    .function = handler_function,
                    .address = static_cast<std::uint64_t>(0x10002000 + (i % 5) * 0x80),
                    .line = static_cast<std::int64_t>(50 + (i % 5) * 5),
                },
            };

            std::vector<std::int64_t> values{
                static_cast<std::int64_t>(1000000 + (i % 1000) * 1000),
            };

            std::vector<Label2> labels{
                Label2{
                    .key = thread_id_key,
                    .str = "",
                    .num = static_cast<std::int64_t>(i % 4),
                    .num_unit = "",
                },
                Label2{
                    .key = sample_id_key,
                    .str = "",
                    .num = static_cast<std::int64_t>(i),
                    .num_unit = "",
                },
            };

            Sample2 sample{
                .locations = {locations.data(), locations.size()},
                .values = {values.data(), values.size()},
                .labels = {labels.data(), labels.size()},
            };

            // Exercise both timestamp modes:
            // - 0 means no timestamp, matching ddog_prof_Profile_add2.
            // - nonzero records an end_timestamp_ns label.
            if (i % 2 == 0) {
                profile->add_sample2(sample, 0);
            } else {
                profile->add_sample2(sample, 42 + i);
            }
        }

        std::cout << "✅ Added 100 api2 samples" << std::endl;

        std::cout << "Adding endpoint data..." << std::endl;
        profile->add_endpoint(12345, "/api/users");
        profile->add_endpoint_count("/api/users", 100);
        std::cout << "✅ Added endpoint data" << std::endl;

        std::cout << "Serializing profile..." << std::endl;
        auto encoded = profile->serialize_to_vec();
        if (encoded.size() == 0) {
            throw std::runtime_error("serialized profile was empty");
        }

        std::ofstream out("profile_api2.pprof", std::ios::binary);
        out.write(reinterpret_cast<const char*>(encoded.data()), static_cast<std::streamsize>(encoded.size()));
        out.close();

        std::cout << "✅ Serialized " << encoded.size() << " bytes to profile_api2.pprof" << std::endl;
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
