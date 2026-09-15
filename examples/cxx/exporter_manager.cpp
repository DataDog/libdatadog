// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <cstdint>
#include <iostream>
#include <memory>
#include <unistd.h>
#include <sys/wait.h>
#include <vector>
#include "datadog/profiling.hpp"

using namespace datadog::profiling;

bool add_sample(Profile& profile, std::vector<Location> locations, std::vector<std::int64_t> values, std::vector<Label> labels) {
    return profile.add_sample(views::sample(locations, values, labels));
}

int main(int argc, char *argv[]) {
    try {
        const char *api_key = std::getenv("DD_API_KEY");
        if (!api_key) {
            std::cout << "DD_API_KEY not set, using file endpoint for demonstration" << std::endl;
        }

        // Default service name for automated testing
        std::string service = (argc >= 2) ? argv[1] : "libdatadog-test";

        // ============================================================================
        // Example 1: Basic ExporterManager usage
        // ============================================================================

        std::cout << "=== Example 1: Basic ExporterManager Usage ===" << std::endl;

        // Create a profile
        Period period{
            .value_type = SampleType::WallTime,
            .value = 60
        };

        auto profile_result = Profile::create({SampleType::WallTime}, period);
        if (!profile_result->check_and_print()) return 1;
        auto profile = profile_result->take();
        profile->set_error_policy(ErrorPolicy::PrintImmediately);
        std::cout << "✓ Created profile" << std::endl;

        // Add some sample data
        Mapping mapping{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = "/usr/lib/libexample.so",
            .build_id = "abc123"
        };

        if (!add_sample(
            *profile,
            {Location{
                .mapping = mapping,
                .function = Function{
                    .name = "main",
                    .system_name = "main",
                    .filename = "example.cpp"
                },
                .address = 0x10001000,
                .line = 42
            }},
            {1000000},
            {Label{.key = "thread_id", .str = "", .num = 1, .num_unit = ""}}
        )) return 1;

        std::cout << "✓ Added sample to profile" << std::endl;

        // Create exporter
        auto exporter_result = api_key
            ? ProfileExporter::create_agentless_exporter(
                "libdatadog-example",
                "1.0.0",
                "native",
                {
                    Tag{.key = "service", .value = service.c_str()},
                    Tag{.key = "env", .value = "dev"}
                },
                "datadoghq.com",
                api_key,
                10000,
                false
              )
            : ProfileExporter::create_file_exporter(
                "libdatadog-example",
                "1.0.0",
                "native",
                {
                    Tag{.key = "service", .value = service.c_str()},
                    Tag{.key = "env", .value = "dev"}
                },
                "/tmp/exporter_manager_example_cxx.txt"
              );
        if (!exporter_result->check_and_print()) return 1;
        auto exporter = exporter_result->take();

        std::cout << "✓ Created exporter" << std::endl;

        // Create ExporterManager
        auto manager_result = ExporterManager::new_manager(std::move(exporter));
        if (!manager_result->check_and_print()) return 1;
        auto manager = manager_result->take();
        std::cout << "✓ Created ExporterManager with background worker thread" << std::endl;

        // Queue the profile (this resets the profile and queues the previous data)
        if (!manager->queue_profile(
            *profile,
            {},    // files_to_compress
            {},    // additional_tags
            "",    // process_tags
            "",    // internal_metadata
            ""     // info
        ).check_and_print()) return 1;

        std::cout << "✓ Queued profile for async sending" << std::endl;

        // Give worker thread time to process
        sleep(1);

        // Abort the manager (stops worker thread)
        if (!manager->abort().check_and_print()) return 1;
        std::cout << "✓ Aborted manager (worker thread stopped)" << std::endl;

        std::cout << std::endl;

        // ============================================================================
        // Example 2: Fork-safe usage
        // ============================================================================

        std::cout << "=== Example 2: Fork-Safe ExporterManager Usage ===" << std::endl;

        // Create a new profile and exporter for the fork example
        auto profile2_result = Profile::create({SampleType::WallTime}, period);
        if (!profile2_result->check_and_print()) return 1;
        auto profile2 = profile2_result->take();
        profile2->set_error_policy(ErrorPolicy::PrintImmediately);

        if (!add_sample(
            *profile2,
            {Location{
                .mapping = mapping,
                .function = Function{
                    .name = "worker",
                    .system_name = "worker",
                    .filename = "worker.cpp"
                },
                .address = 0x10002000,
                .line = 100
            }},
            {2000000},
            {Label{.key = "thread_id", .str = "", .num = 2, .num_unit = ""}}
        )) return 1;

        auto exporter2_result = ProfileExporter::create_file_exporter(
            "libdatadog-example-fork",
            "1.0.0",
            "native",
            {
                Tag{.key = "service", .value = "fork-example"},
                Tag{.key = "env", .value = "dev"}
            },
            "/tmp/exporter_manager_fork_cxx.txt"
        );
        if (!exporter2_result->check_and_print()) return 1;
        auto exporter2 = exporter2_result->take();

        auto manager2_result = ExporterManager::new_manager(std::move(exporter2));
        if (!manager2_result->check_and_print()) return 1;
        auto manager2 = manager2_result->take();
        std::cout << "✓ Created ExporterManager for fork example" << std::endl;

        // Queue a profile before forking
        if (!manager2->queue_profile(*profile2, {}, {}, "", "", "").check_and_print()) return 1;
        std::cout << "✓ Queued profile (may be inflight during fork)" << std::endl;

        // Call prefork before forking
        if (!manager2->prefork().check_and_print()) return 1;
        std::cout << "✓ Called prefork (worker thread stopped, ready to fork)" << std::endl;

        pid_t pid = fork();

        if (pid < 0) {
            std::cerr << "Failed to fork" << std::endl;
            return 1;
        }

        if (pid == 0) {
            // Child process
            std::cout << "[CHILD] ✓ In child process (PID: " << getpid() << ")" << std::endl;

            // Call postfork_child to restart the manager
            if (!manager2->postfork_child().check_and_print()) return 1;
            std::cout << "[CHILD] ✓ Restarted manager (inflight requests discarded)" << std::endl;

            // Child can now use the manager independently
            // Add another sample in the child
            if (!add_sample(
                *profile2,
                {Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "child_func",
                        .system_name = "child_func",
                        .filename = "child.cpp"
                    },
                    .address = 0x10003000,
                    .line = 200
                }},
                {3000000},
                {Label{.key = "process", .str = "child", .num = 0, .num_unit = ""}}
            )) return 1;

            if (!manager2->queue_profile(*profile2, {}, {}, "", "", "").check_and_print()) return 1;
            std::cout << "[CHILD] ✓ Queued child-specific profile" << std::endl;

            sleep(1);

            if (!manager2->abort().check_and_print()) return 1;
            std::cout << "[CHILD] ✓ Cleaned up and exiting" << std::endl;

            exit(0);
        } else {
            // Parent process
            std::cout << "[PARENT] ✓ In parent process (PID: " << getpid()
                      << ", child PID: " << pid << ")" << std::endl;

            // Call postfork_parent to restart the manager with inflight requests
            if (!manager2->postfork_parent().check_and_print()) return 1;
            std::cout << "[PARENT] ✓ Restarted manager (inflight requests re-queued)" << std::endl;

            // Parent continues profiling
            if (!add_sample(
                *profile2,
                {Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "parent_func",
                        .system_name = "parent_func",
                        .filename = "parent.cpp"
                    },
                    .address = 0x10004000,
                    .line = 300
                }},
                {4000000},
                {Label{.key = "process", .str = "parent", .num = 0, .num_unit = ""}}
            )) return 1;

            if (!manager2->queue_profile(*profile2, {}, {}, "", "", "").check_and_print()) return 1;
            std::cout << "[PARENT] ✓ Queued parent-specific profile" << std::endl;

            // Wait for child to finish
            int status;
            waitpid(pid, &status, 0);
            std::cout << "[PARENT] ✓ Child process finished" << std::endl;

            sleep(1);

            if (!manager2->abort().check_and_print()) return 1;
            std::cout << "[PARENT] ✓ Cleaned up" << std::endl;
        }

        std::cout << std::endl;
        std::cout << "=== All examples completed successfully ===" << std::endl;

        return 0;

    } catch (const std::exception& e) {
        std::cerr << "Error: " << e.what() << std::endl;
        return 1;
    }
}

