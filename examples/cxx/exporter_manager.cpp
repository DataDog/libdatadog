// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <iostream>
#include <memory>
#include <unistd.h>
#include <sys/wait.h>
#include "libdd-profiling/src/cxx.rs.h"

using namespace datadog::profiling;

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
        ValueType wall_time{
            .type_ = "wall-time",
            .unit = "nanoseconds"
        };

        Period period{
            .value_type = wall_time,
            .value = 60
        };

        auto profile = Profile::create({wall_time}, period);
        std::cout << "✓ Created profile" << std::endl;

        // Add some sample data
        Mapping mapping{
            .memory_start = 0x10000000,
            .memory_limit = 0x20000000,
            .file_offset = 0,
            .filename = "/usr/lib/libexample.so",
            .build_id = "abc123"
        };

        profile->add_sample(Sample{
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
        });

        std::cout << "✓ Added sample to profile" << std::endl;

        // Create exporter
        auto exporter = api_key 
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

        std::cout << "✓ Created exporter" << std::endl;

        // Create ExporterManager
        auto manager = ExporterManager::new_manager(std::move(exporter));
        std::cout << "✓ Created ExporterManager with background worker thread" << std::endl;

        // Queue the profile (this resets the profile and queues the previous data)
        manager->queue_profile(
            *profile,
            {},    // files_to_compress
            {},    // additional_tags
            "",    // process_tags
            "",    // internal_metadata
            ""     // info
        );

        std::cout << "✓ Queued profile for async sending" << std::endl;

        // Give worker thread time to process
        sleep(1);

        // Abort the manager (stops worker thread)
        manager->abort();
        std::cout << "✓ Aborted manager (worker thread stopped)" << std::endl;

        std::cout << std::endl;

        // ============================================================================
        // Example 2: Fork-safe usage
        // ============================================================================
        
        std::cout << "=== Example 2: Fork-Safe ExporterManager Usage ===" << std::endl;

        // Create a new profile and exporter for the fork example
        auto profile2 = Profile::create({wall_time}, period);
        
        profile2->add_sample(Sample{
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
        });

        auto exporter2 = ProfileExporter::create_file_exporter(
            "libdatadog-example-fork",
            "1.0.0",
            "native",
            {
                Tag{.key = "service", .value = "fork-example"},
                Tag{.key = "env", .value = "dev"}
            },
            "/tmp/exporter_manager_fork_cxx.txt"
        );

        auto manager2 = ExporterManager::new_manager(std::move(exporter2));
        std::cout << "✓ Created ExporterManager for fork example" << std::endl;

        // Queue a profile before forking
        manager2->queue_profile(*profile2, {}, {}, "", "", "");
        std::cout << "✓ Queued profile (may be inflight during fork)" << std::endl;

        // Call prefork before forking
        manager2->prefork();
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
            manager2->postfork_child();
            std::cout << "[CHILD] ✓ Restarted manager (inflight requests discarded)" << std::endl;

            // Child can now use the manager independently
            // Add another sample in the child
            profile2->add_sample(Sample{
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
            });

            manager2->queue_profile(*profile2, {}, {}, "", "", "");
            std::cout << "[CHILD] ✓ Queued child-specific profile" << std::endl;

            sleep(1);

            manager2->abort();
            std::cout << "[CHILD] ✓ Cleaned up and exiting" << std::endl;
            
            exit(0);
        } else {
            // Parent process
            std::cout << "[PARENT] ✓ In parent process (PID: " << getpid() 
                      << ", child PID: " << pid << ")" << std::endl;

            // Call postfork_parent to restart the manager with inflight requests
            manager2->postfork_parent();
            std::cout << "[PARENT] ✓ Restarted manager (inflight requests re-queued)" << std::endl;

            // Parent continues profiling
            profile2->add_sample(Sample{
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
            });

            manager2->queue_profile(*profile2, {}, {}, "", "", "");
            std::cout << "[PARENT] ✓ Queued parent-specific profile" << std::endl;

            // Wait for child to finish
            int status;
            waitpid(pid, &status, 0);
            std::cout << "[PARENT] ✓ Child process finished" << std::endl;

            sleep(1);

            manager2->abort();
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

