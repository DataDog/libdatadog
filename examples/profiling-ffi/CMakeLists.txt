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

add_executable(crashinfo crashinfo.cpp)
# needed for designated initializers
target_compile_features(crashinfo PRIVATE cxx_std_20)
target_link_libraries(crashinfo PRIVATE Datadog::Profiling)


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

if(NOT WIN32)
  add_executable(crashtracking crashtracking.c)
  target_link_libraries(crashtracking PRIVATE Datadog::Profiling)
endif()

add_executable(trace_exporter trace_exporter.c)
target_link_libraries(trace_exporter PRIVATE Datadog::Profiling)
set_vcruntime_link_type(trace_exporter ${VCRUNTIME_LINK_TYPE})

add_executable(array_queue array_queue.cpp)
target_compile_features(array_queue PRIVATE cxx_std_20)
target_link_libraries(array_queue PRIVATE Datadog::Profiling)
set_vcruntime_link_type(array_queue ${VCRUNTIME_LINK_TYPE})
