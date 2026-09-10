// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <cstdint>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

bool check_status(const Status& status, const char* operation) {
    if (status.ok) {
        return true;
    }
    std::cerr << "❌ " << operation << " failed: " << std::string(status.message) << std::endl;
    return false;
}

int main() {
    try {
        std::cout << "=== Datadog Profiling CXX api2 Example ===" << std::endl;

        Period period{
            .value_type = SampleType::WallTime,
            .value = 60,
        };

        std::cout << "Creating ProfilesDictionary..." << std::endl;
        auto dictionary = ProfilesDictionary::create_or_throw();

        // Insert dictionary strings once and reuse opaque ids on every sample.
        auto mapping_filename_result = dictionary->insert_string("/usr/lib/libexample.so");
        if (!check_status(mapping_filename_result.status, "insert_string")) return 1;
        auto mapping_filename = mapping_filename_result.value;

        auto build_id_result = dictionary->insert_string("abc123");
        if (!check_status(build_id_result.status, "insert_string")) return 1;
        auto build_id = build_id_result.value;

        auto hot_function_name_result = dictionary->insert_string("hot_function");
        if (!check_status(hot_function_name_result.status, "insert_string")) return 1;
        auto hot_function_name = hot_function_name_result.value;

        auto hot_function_system_name_result = dictionary->insert_string("_Z12hot_functionv");
        if (!check_status(hot_function_system_name_result.status, "insert_string")) return 1;
        auto hot_function_system_name = hot_function_system_name_result.value;

        auto hot_function_file_result = dictionary->insert_string("/src/hot_path.cpp");
        if (!check_status(hot_function_file_result.status, "insert_string")) return 1;
        auto hot_function_file = hot_function_file_result.value;

        auto handler_function_name_result = dictionary->insert_string("process_request");
        if (!check_status(handler_function_name_result.status, "insert_string")) return 1;
        auto handler_function_name = handler_function_name_result.value;

        auto handler_function_system_name_result = dictionary->insert_string("_Z15process_requestv");
        if (!check_status(handler_function_system_name_result.status, "insert_string")) return 1;
        auto handler_function_system_name = handler_function_system_name_result.value;

        auto handler_function_file_result = dictionary->insert_string("/src/handler.cpp");
        if (!check_status(handler_function_file_result.status, "insert_string")) return 1;
        auto handler_function_file = handler_function_file_result.value;

        auto thread_id_key_result = dictionary->insert_string("thread_id");
        if (!check_status(thread_id_key_result.status, "insert_string")) return 1;
        auto thread_id_key = thread_id_key_result.value;

        auto sample_id_key_result = dictionary->insert_string("sample_id");
        if (!check_status(sample_id_key_result.status, "insert_string")) return 1;
        auto sample_id_key = sample_id_key_result.value;

        auto mapping_result = dictionary->insert_mapping(Mapping2{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = mapping_filename,
            .build_id = build_id,
        });
        if (!check_status(mapping_result.status, "insert_mapping")) return 1;
        auto mapping = mapping_result.value;

        auto hot_function_result = dictionary->insert_function(Function2{
            .name = hot_function_name,
            .system_name = hot_function_system_name,
            .file_name = hot_function_file,
        });
        if (!check_status(hot_function_result.status, "insert_function")) return 1;
        auto hot_function = hot_function_result.value;

        auto handler_function_result = dictionary->insert_function(Function2{
            .name = handler_function_name,
            .system_name = handler_function_system_name,
            .file_name = handler_function_file,
        });
        if (!check_status(handler_function_result.status, "insert_function")) return 1;
        auto handler_function = handler_function_result.value;

        std::cout << "✅ Dictionary populated" << std::endl;

        std::cout << "Creating dictionary-backed Profile..." << std::endl;
        auto profile = Profile::create_with_dictionary_or_throw({SampleType::WallTime}, period, *dictionary);
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
                if (!check_status(profile->add_sample2(sample, 0), "add_sample2")) return 1;
            } else {
                if (!check_status(profile->add_sample2(sample, 42 + i), "add_sample2")) return 1;
            }
        }

        std::cout << "✅ Added 100 api2 samples" << std::endl;

        std::cout << "Adding endpoint data..." << std::endl;
        if (!check_status(profile->add_endpoint(12345, "/api/users"), "add_endpoint")) return 1;
        if (!check_status(profile->add_endpoint_count("/api/users", 100), "add_endpoint_count")) return 1;
        std::cout << "✅ Added endpoint data" << std::endl;

        std::cout << "Serializing profile..." << std::endl;
        auto encoded = profile->serialize_to_vec_or_throw();
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
