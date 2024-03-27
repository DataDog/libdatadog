cmake_minimum_required(VERSION 3.19)
project(data_pipeline_ffi_examples LANGUAGES C)

find_package(DataPipeline REQUIRED)

# Uncomment to debug build commands
# set(CMAKE_VERBOSE_MAKEFILE ON)

add_executable(trace_exporter trace_exporter.c)
target_link_libraries(trace_exporter PRIVATE Datadog::DataPipeline)

if(CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
  target_compile_definitions(trace_exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
endif()

