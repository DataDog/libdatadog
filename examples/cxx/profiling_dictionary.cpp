// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <cstdint>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

#include "datadog/profiling.hpp"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "=== Datadog Profiling CXX Dictionary Example ===" << std::endl;

        Period period{
            .value_type = SampleType::WallTime,
            .value = 60,
        };

        std::cout << "Creating ProfileDictionary..." << std::endl;
        auto dictionary_result = ProfileDictionary::create();
        if (!dictionary_result->check_and_print()) return 1;
        auto dictionary = dictionary_result->take_value();
        dictionary->set_error_policy(ErrorPolicy::PrintImmediately);

        // Intern dictionary strings once and reuse opaque ids on every sample.
        DictionaryStringId mapping_filename{};
        if (!dictionary->intern_string("/usr/lib/libexample.so", mapping_filename)) return 1;

        DictionaryStringId build_id{};
        if (!dictionary->intern_string("abc123", build_id)) return 1;

        DictionaryStringId hot_function_name{};
        if (!dictionary->intern_string("hot_function", hot_function_name)) return 1;

        DictionaryStringId hot_function_system_name{};
        if (!dictionary->intern_string("_Z12hot_functionv", hot_function_system_name)) return 1;

        DictionaryStringId hot_function_file{};
        if (!dictionary->intern_string("/src/hot_path.cpp", hot_function_file)) return 1;

        DictionaryStringId handler_function_name{};
        if (!dictionary->intern_string("process_request", handler_function_name)) return 1;

        DictionaryStringId handler_function_system_name{};
        if (!dictionary->intern_string("_Z15process_requestv", handler_function_system_name)) return 1;

        DictionaryStringId handler_function_file{};
        if (!dictionary->intern_string("/src/handler.cpp", handler_function_file)) return 1;

        DictionaryStringId thread_id_key{};
        if (!dictionary->intern_string("thread_id", thread_id_key)) return 1;

        DictionaryStringId sample_id_key{};
        if (!dictionary->intern_string("sample_id", sample_id_key)) return 1;

        DictionaryMappingId mapping{};
        if (!dictionary->intern_mapping(DictionaryMapping{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = mapping_filename,
            .build_id = build_id,
        }, mapping)) return 1;

        DictionaryFunctionId hot_function{};
        if (!dictionary->intern_function(DictionaryFunction{
            .name = hot_function_name,
            .system_name = hot_function_system_name,
            .filename = hot_function_file,
        }, hot_function)) return 1;

        DictionaryFunctionId handler_function{};
        if (!dictionary->intern_function(DictionaryFunction{
            .name = handler_function_name,
            .system_name = handler_function_system_name,
            .filename = handler_function_file,
        }, handler_function)) return 1;

        std::cout << "✅ Dictionary populated" << std::endl;

        std::cout << "Creating dictionary-backed Profile..." << std::endl;
        auto profile_result = Profile::create_with_dictionary({SampleType::WallTime}, period, *dictionary);
        if (!profile_result->check_and_print()) return 1;
        auto profile = profile_result->take_value();
        profile->set_error_policy(ErrorPolicy::PrintImmediately);
        std::cout << "✅ Profile created" << std::endl;

        std::cout << "Adding dictionary-backed samples..." << std::endl;
        for (int i = 0; i < 100; i++) {
            std::vector<DictionaryLocation> locations{
                DictionaryLocation{
                    .mapping = mapping,
                    .function = hot_function,
                    .address = static_cast<std::uint64_t>(0x10003000 + (i % 3) * 0x100),
                    .line = static_cast<std::int64_t>(100 + (i % 3) * 10),
                },
                DictionaryLocation{
                    .mapping = mapping,
                    .function = handler_function,
                    .address = static_cast<std::uint64_t>(0x10002000 + (i % 5) * 0x80),
                    .line = static_cast<std::int64_t>(50 + (i % 5) * 5),
                },
            };

            std::vector<std::int64_t> values{
                static_cast<std::int64_t>(1000000 + (i % 1000) * 1000),
            };

            std::vector<DictionaryLabel> labels{
                DictionaryLabel{
                    .key = thread_id_key,
                    .str_bytes = strings::bytes(""),
                    .num = static_cast<std::int64_t>(i % 4),
                    .num_unit = strings::bytes(""),
                },
                DictionaryLabel{
                    .key = sample_id_key,
                    .str_bytes = strings::bytes(""),
                    .num = static_cast<std::int64_t>(i),
                    .num_unit = strings::bytes(""),
                },
            };

            auto sample = views::dictionary_sample(locations, values, labels);

            // Exercise both overloads: without an end timestamp, and with one.
            if (i % 2 == 0) {
                if (!profile->add_dictionary_sample(sample)) return 1;
            } else {
                if (!profile->add_dictionary_sample(sample, 42 + i)) return 1;
            }
        }

        std::cout << "✅ Added 100 dictionary-backed samples" << std::endl;

        std::cout << "Adding endpoint data..." << std::endl;
        if (!profile->add_endpoint(12345, strings::bytes("/api/users"))) return 1;
        if (!profile->add_endpoint_count(strings::bytes("/api/users"), 100)) return 1;
        std::cout << "✅ Added endpoint data" << std::endl;

        std::cout << "Serializing profile..." << std::endl;
        auto encoded_result = profile->serialize();
        if (!encoded_result->check_and_print()) return 1;
        auto encoded_profile = encoded_result->take_value();
        auto encoded = encoded_profile->bytes();
        if (encoded.size() == 0) {
            throw std::runtime_error("serialized profile was empty");
        }

        std::ofstream out("profile_dictionary.pprof", std::ios::binary);
        out.write(reinterpret_cast<const char*>(encoded.data()), static_cast<std::streamsize>(encoded.size()));
        out.close();

        std::cout << "✅ Serialized " << encoded.size() << " bytes to profile_dictionary.pprof" << std::endl;
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
