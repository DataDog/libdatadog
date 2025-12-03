#include <iostream>
#include <memory>
#include "libdd-crashtracker/src/crash_info/cxx.rs.h"

using namespace datadog::crashtracker;

int main() {
    try {
        std::cout << "Creating CrashInfo using CXX bindings..." << std::endl;
        
        auto builder = CrashInfoBuilder::create();
        
        builder->set_kind(CxxErrorKind::Panic);
        builder->with_message("Example crash message");
        builder->with_counter("my_counter", 42);
        builder->with_log_message("This is a log message", true);
        builder->with_fingerprint("test-fingerprint-123");
        builder->with_incomplete(false);
        
        // Set metadata
        Metadata metadata;
        metadata.library_name = "libdatadog";
        metadata.library_version = "1.0.0";
        metadata.family = "rust";
        metadata.tags.push_back("service:example");
        metadata.tags.push_back("env:dev");
        builder->set_metadata(metadata);
        
        // Set process info
        ProcInfo proc_info;
        proc_info.pid = 12345;
        builder->set_proc_info(proc_info);
        
        // Set OS info
        OsInfo os_info;
        os_info.architecture = "x86_64";
        os_info.bitness = "64";
        os_info.os_type = "Linux";
        os_info.version = "5.15.0";
        builder->set_os_info(os_info);
        
        // Create a stack trace
        auto stacktrace = StackTrace::create();
        
        // Pass 'true' for incomplete to allow adding more frames
        for (int i = 0; i < 5; ++i) {
            auto frame = StackFrame::create();
            frame->with_function("function_" + std::to_string(i));
            frame->with_file("/path/to/file_" + std::to_string(i) + ".rs");
            frame->with_line(100 + i);
            frame->with_column(10 + i);
            stacktrace->add_frame(std::move(frame), true);
        }
        
        // Add a frame with address info (Windows style)
        auto win_frame = StackFrame::create();
        win_frame->with_ip(0xDEADBEEF);
        win_frame->with_module_base_address(0xABBABABA);
        win_frame->with_build_id("abcdef123456");
        win_frame->build_id_type(CxxBuildIdType::PDB);
        win_frame->file_type(CxxFileType::PE);
        win_frame->with_path("C:/Program Files/example.exe");
        win_frame->with_relative_address(0xBABEF00D);
        stacktrace->add_frame(std::move(win_frame), true);
        
        // Add a frame with ELF info
        auto elf_frame = StackFrame::create();
        elf_frame->with_ip(0xCAFEBABE);
        elf_frame->with_build_id("fedcba987654321");
        elf_frame->build_id_type(CxxBuildIdType::GNU);
        elf_frame->file_type(CxxFileType::ELF);
        elf_frame->with_path("/usr/lib/libexample.so");
        elf_frame->with_relative_address(0xF00DFACE);
        stacktrace->add_frame(std::move(elf_frame), true);
        
        stacktrace->mark_complete();
        builder->add_stack(std::move(stacktrace));
        builder->with_timestamp_now();
        
        auto crash_info = crashinfo_build(std::move(builder));
        auto json = crash_info->to_json();
        std::cout << "\nCrashInfo JSON:\n" << std::string(json) << std::endl;
        
        std::cout << "\n✅ Success!" << std::endl;
        return 0;
        
    } catch (const std::exception& e) {
        std::cerr << "❌ Exception: " << e.what() << std::endl;
        return 1;
    }
}
