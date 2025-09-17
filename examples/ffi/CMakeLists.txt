cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_ffi_examples LANGUAGES C CXX)

find_package(Datadog REQUIRED)

set(VCRUNTIME_LINK_TYPE DLL CACHE STRING "Specify the Runtime Library to use when compiling with MSVC")
set(CMAKE_EXPORT_COMPILE_COMMANDS on)

function(set_vcruntime_link_type binary link_type)
  if (CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
    if (link_type STREQUAL "DLL")
      set_property(TARGET ${binary} PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL")
    else()
      set_property(TARGET ${binary} PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
    endif()
  endif()
endfunction()

# Uncomment to debug build commands
# set(CMAKE_VERBOSE_MAKEFILE ON)

add_executable(exporter exporter.cpp)
# needed for designated initializers
target_compile_features(exporter PRIVATE cxx_std_20)
target_link_libraries(exporter PRIVATE Datadog::Profiling)
set_vcruntime_link_type(exporter ${VCRUNTIME_LINK_TYPE})


if(CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
  target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
endif()

add_executable(profiles profiles.c)
target_link_libraries(profiles PRIVATE Datadog::Profiling)
set_vcruntime_link_type(profiles ${VCRUNTIME_LINK_TYPE})

if(BUILD_SYMBOLIZER)
  add_executable(symbolizer symbolizer.cpp)
  target_link_libraries(symbolizer PRIVATE Datadog::Profiling)
  set_vcruntime_link_type(symbolizer ${VCRUNTIME_LINK_TYPE})
endif()

add_executable(telemetry telemetry.c)
target_link_libraries(telemetry PRIVATE Datadog::Profiling)
set_vcruntime_link_type(telemetry ${VCRUNTIME_LINK_TYPE})

add_executable(telemetry_metrics telemetry_metrics.c)
target_link_libraries(telemetry_metrics PRIVATE Datadog::Profiling)
set_vcruntime_link_type(telemetry_metrics ${VCRUNTIME_LINK_TYPE})

add_executable(trace_exporter trace_exporter.c)
target_link_libraries(trace_exporter PRIVATE Datadog::Profiling)
set_vcruntime_link_type(trace_exporter ${VCRUNTIME_LINK_TYPE})

add_executable(array_queue array_queue.cpp)
target_compile_features(array_queue PRIVATE cxx_std_20)
target_link_libraries(array_queue PRIVATE Datadog::Profiling)
set_vcruntime_link_type(array_queue ${VCRUNTIME_LINK_TYPE})

add_executable(library_config library_config.c)
target_link_libraries(library_config PRIVATE Datadog::Profiling)
set_vcruntime_link_type(library_config ${VCRUNTIME_LINK_TYPE})

add_executable(ddsketch ddsketch.c)
target_link_libraries(ddsketch PRIVATE Datadog::Profiling)
set_vcruntime_link_type(ddsketch ${VCRUNTIME_LINK_TYPE})

# Google Benchmark integration
find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
  pkg_check_modules(BENCHMARK QUIET benchmark)
endif()

# Always try to build with benchmark if the library exists
find_library(BENCHMARK_LIB benchmark)
if(BENCHMARK_LIB)
  add_executable(profiling_benchmark profiling_benchmark.cpp)
  target_compile_features(profiling_benchmark PRIVATE cxx_std_17)
  target_link_libraries(profiling_benchmark PRIVATE Datadog::Profiling ${BENCHMARK_LIB} pthread)
  set_vcruntime_link_type(profiling_benchmark ${VCRUNTIME_LINK_TYPE})
  message(STATUS "Google Benchmark library found at: ${BENCHMARK_LIB}")
  message(STATUS "profiling_benchmark target available")
else()
  message(STATUS "Google Benchmark library not found - to install: apt-get install libbenchmark-dev")
  message(STATUS "Skipping profiling_benchmark target")
endif()
