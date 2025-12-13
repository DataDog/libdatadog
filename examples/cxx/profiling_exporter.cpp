#include <iostream>
#include <memory>
#include <vector>
#include "libdd-profiling/src/exporter/cxx.rs.h"

using namespace datadog::profiling;

int main() {
    try {
        std::cout << "=== Datadog Profiling Exporter CXX Bindings Example ===" << std::endl;
        
        // Example 1: Create an exporter that writes to a file (for debugging)
        std::cout << "\n1. Creating file-based exporter..." << std::endl;
        auto file_exporter = ProfileExporter::create_file(
            "dd-trace-cxx",      // library name
            "1.0.0",             // library version
            "cpp",               // language family
            {                    // tags
                "env:development",
                "service:my-service",
                "version:1.0.0"
            },
            "/tmp/profile_export.http"  // output file path
        );
        std::cout << "✓ File exporter created" << std::endl;
        
        // Example 2: Create an agent-based exporter
        std::cout << "\n2. Creating agent-based exporter..." << std::endl;
        auto agent_exporter = ProfileExporter::create_agent(
            "dd-trace-cxx",      // library name
            "1.0.0",             // library version
            "cpp",               // language family
            {                    // tags
                "env:production",
                "service:my-service",
                "host:web-server-01"
            },
            "http://localhost:8126"  // agent URL
        );
        std::cout << "✓ Agent exporter created" << std::endl;
        
        // Example 3: Create an agentless (direct intake) exporter
        std::cout << "\n3. Creating agentless exporter..." << std::endl;
        auto agentless_exporter = ProfileExporter::create_agentless(
            "dd-trace-cxx",      // library name
            "1.0.0",             // library version
            "cpp",               // language family
            {                    // tags
                "env:staging",
                "service:my-service"
            },
            "datadoghq.com",     // site
            "YOUR_API_KEY_HERE"  // API key (not a real key)
        );
        std::cout << "✓ Agentless exporter created" << std::endl;
        
        // Example 4: Create exporter using generic config
        std::cout << "\n4. Creating exporter from config struct..." << std::endl;
        auto config_exporter = ProfileExporter::create(ExporterConfig{
            .profiling_library_name = "dd-trace-cxx",
            .profiling_library_version = "2.0.0",
            .family = "cpp",
            .tags = {"env:test", "region:us-east-1"},
            .endpoint_url = "file:///tmp/profile_debug.http",
            .api_key = "",  // not needed for file endpoint
            .timeout_ms = 10000  // 10 seconds
        });
        std::cout << "✓ Config-based exporter created" << std::endl;
        
        // Example 5: Send a test profile
        std::cout << "\n5. Sending a test profile..." << std::endl;
        auto profile = EncodedProfile::create_test_profile();
        
        // Prepare additional files (empty for this example)
        std::vector<ExporterFile> additional_files;
        
        // Prepare additional tags (profile-specific)
        std::vector<std::string> additional_tags = {
            "profile_type:cpu",
            "duration_seconds:60"
        };
        
        // Send the profile (this blocks until complete)
        auto status_code = file_exporter->send_blocking(
            std::move(profile),
            additional_files,
            additional_tags
        );
        
        std::cout << "✓ Profile sent successfully! HTTP status: " << status_code << std::endl;
        std::cout << "  Check /tmp/profile_export_*.http for the dumped request" << std::endl;
        
        // Example 6: Sending with custom files
        std::cout << "\n6. Sending profile with additional files..." << std::endl;
        auto profile2 = EncodedProfile::create_test_profile();
        
        additional_files.push_back(ExporterFile{
            .name = "metadata.json",
            .bytes = {'{', '"', 'k', 'e', 'y', '"', ':', '"', 'v', 'a', 'l', 'u', 'e', '"', '}'}
        });
        
        auto status_code2 = file_exporter->send_blocking(
            std::move(profile2),
            additional_files,
            {}  // no additional tags
        );
        
        std::cout << "✓ Profile with attachments sent! HTTP status: " << status_code2 << std::endl;
        
        // Example 7: Sending with cancellation token
        std::cout << "\n7. Demonstrating cancellation support..." << std::endl;
        auto profile3 = EncodedProfile::create_test_profile();
        auto cancel_token = CancellationToken::create();
        
        // In a real application, you might cancel from another thread:
        // std::thread([&cancel_token]() {
        //     std::this_thread::sleep_for(std::chrono::milliseconds(100));
        //     cancel_token->cancel();
        // }).detach();
        
        // Check if already cancelled (it's not)
        std::cout << "  Token cancelled? " << (cancel_token->is_cancelled() ? "yes" : "no") << std::endl;
        
        // Send with the cancellation token
        auto status_code3 = file_exporter->send_blocking_with_cancel(
            std::move(profile3),
            {},  // no additional files
            {},  // no additional tags
            *cancel_token
        );
        
        std::cout << "✓ Profile sent with cancellation support! HTTP status: " << status_code3 << std::endl;
        
        std::cout << "\n=== All examples completed successfully! ===" << std::endl;
        std::cout << "\nUsage patterns demonstrated:" << std::endl;
        std::cout << "  • File-based export (for debugging)" << std::endl;
        std::cout << "  • Agent-based export (standard Datadog agent)" << std::endl;
        std::cout << "  • Agentless export (direct to Datadog intake)" << std::endl;
        std::cout << "  • Custom configuration" << std::endl;
        std::cout << "  • Sending profiles with tags and attachments" << std::endl;
        std::cout << "  • Cancellable operations" << std::endl;
        
        return 0;
        
    } catch (const rust::Error& e) {
        std::cerr << "❌ Rust error: " << e.what() << std::endl;
        return 1;
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}

