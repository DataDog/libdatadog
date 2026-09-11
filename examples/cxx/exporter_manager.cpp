// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <iostream>
#include <memory>
#include <unistd.h>
#include <sys/wait.h>
#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

bool check_status(const Status& status, const char* operation) {
    if (status.ok) {
        return true;
    }
    std::cerr << "Error: " << operation << " failed: " << std::string(status.message) << std::endl;
    return false;
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

        auto profile = Profile::create_or_throw({SampleType::WallTime}, period);
        std::cout << "✓ Created profile" << std::endl;

        // Add some sample data
        Mapping mapping{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = "/usr/lib/libexample.so",
            .build_id = "abc123"
        };

        if (!check_status(profile->add_sample(Sample{
            .locations = {
                Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "main",
                        .system_name = "main",
                        .filename = "example.cpp"
                    },
                    .address = 0x10001000,
                    .line = 42
                }
            },
            .values = {1000000},
            .labels = {
                Label{.key = "thread_id", .str = "", .num = 1, .num_unit = ""}
            }
        }), "add_sample")) return 1;

        std::cout << "✓ Added sample to profile" << std::endl;

        // Create exporter
        auto exporter = api_key 
            ? ProfileExporter::create_agentless_exporter_or_throw(
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
            : ProfileExporter::create_file_exporter_or_throw(
                "libdatadog-example",
                "1.0.0",
                "native",
                {
                    Tag{.key = "service", .value = service.c_str()},
                    Tag{.key = "env", .value = "dev"}
                },
                "/tmp/exporter_manager_example_cxx.txt"
              );

        std::cout << "✓ Created exporter" << std::endl;

        // Create ExporterManager
        auto manager = ExporterManager::new_manager_or_throw(std::move(exporter));
        std::cout << "✓ Created ExporterManager with background worker thread" << std::endl;

        // Queue the profile (this resets the profile and queues the previous data)
        if (!check_status(manager->queue_profile(
            *profile,
            {},    // files_to_compress
            {},    // additional_tags
            "",    // process_tags
            "",    // internal_metadata
            ""     // info
        ), "queue_profile")) return 1;

        std::cout << "✓ Queued profile for async sending" << std::endl;

        // Give worker thread time to process
        sleep(1);

        // Abort the manager (stops worker thread)
        if (!check_status(manager->abort(), "abort")) return 1;
        std::cout << "✓ Aborted manager (worker thread stopped)" << std::endl;

        std::cout << std::endl;

        // ============================================================================
        // Example 2: Fork-safe usage
        // ============================================================================
        
        std::cout << "=== Example 2: Fork-Safe ExporterManager Usage ===" << std::endl;

        // Create a new profile and exporter for the fork example
        auto profile2 = Profile::create_or_throw({SampleType::WallTime}, period);
        
        if (!check_status(profile2->add_sample(Sample{
            .locations = {
                Location{
                    .mapping = mapping,
                    .function = Function{
                        .name = "worker",
                        .system_name = "worker",
                        .filename = "worker.cpp"
                    },
                    .address = 0x10002000,
                    .line = 100
                }
            },
            .values = {2000000},
            .labels = {
                Label{.key = "thread_id", .str = "", .num = 2, .num_unit = ""}
            }
        }), "add_sample")) return 1;

        auto exporter2 = ProfileExporter::create_file_exporter_or_throw(
            "libdatadog-example-fork",
            "1.0.0",
            "native",
            {
                Tag{.key = "service", .value = "fork-example"},
                Tag{.key = "env", .value = "dev"}
            },
            "/tmp/exporter_manager_fork_cxx.txt"
        );

        auto manager2 = ExporterManager::new_manager_or_throw(std::move(exporter2));
        std::cout << "✓ Created ExporterManager for fork example" << std::endl;

        // Queue a profile before forking
        if (!check_status(manager2->queue_profile(*profile2, {}, {}, "", "", ""), "queue_profile")) return 1;
        std::cout << "✓ Queued profile (may be inflight during fork)" << std::endl;

        // Call prefork before forking
        if (!check_status(manager2->prefork(), "prefork")) return 1;
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
            if (!check_status(manager2->postfork_child(), "postfork_child")) return 1;
            std::cout << "[CHILD] ✓ Restarted manager (inflight requests discarded)" << std::endl;

            // Child can now use the manager independently
            // Add another sample in the child
            if (!check_status(profile2->add_sample(Sample{
                .locations = {
                    Location{
                        .mapping = mapping,
                        .function = Function{
                            .name = "child_func",
                            .system_name = "child_func",
                            .filename = "child.cpp"
                        },
                        .address = 0x10003000,
                        .line = 200
                    }
                },
                .values = {3000000},
                .labels = {
                    Label{.key = "process", .str = "child", .num = 0, .num_unit = ""}
                }
            }), "add_sample")) return 1;

            if (!check_status(manager2->queue_profile(*profile2, {}, {}, "", "", ""), "queue_profile")) return 1;
            std::cout << "[CHILD] ✓ Queued child-specific profile" << std::endl;

            sleep(1);

            if (!check_status(manager2->abort(), "abort")) return 1;
            std::cout << "[CHILD] ✓ Cleaned up and exiting" << std::endl;
            
            exit(0);
        } else {
            // Parent process
            std::cout << "[PARENT] ✓ In parent process (PID: " << getpid() 
                      << ", child PID: " << pid << ")" << std::endl;

            // Call postfork_parent to restart the manager with inflight requests
            if (!check_status(manager2->postfork_parent(), "postfork_parent")) return 1;
            std::cout << "[PARENT] ✓ Restarted manager (inflight requests re-queued)" << std::endl;

            // Parent continues profiling
            if (!check_status(profile2->add_sample(Sample{
                .locations = {
                    Location{
                        .mapping = mapping,
                        .function = Function{
                            .name = "parent_func",
                            .system_name = "parent_func",
                            .filename = "parent.cpp"
                        },
                        .address = 0x10004000,
                        .line = 300
                    }
                },
                .values = {4000000},
                .labels = {
                    Label{.key = "process", .str = "parent", .num = 0, .num_unit = ""}
                }
            }), "add_sample")) return 1;

            if (!check_status(manager2->queue_profile(*profile2, {}, {}, "", "", ""), "queue_profile")) return 1;
            std::cout << "[PARENT] ✓ Queued parent-specific profile" << std::endl;

            // Wait for child to finish
            int status;
            waitpid(pid, &status, 0);
            std::cout << "[PARENT] ✓ Child process finished" << std::endl;

            sleep(1);

            if (!check_status(manager2->abort(), "abort")) return 1;
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

