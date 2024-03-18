cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_ffi_examples LANGUAGES C CXX)

find_package(Datadog REQUIRED)

# Uncomment to debug build commands
# set(CMAKE_VERBOSE_MAKEFILE ON)

add_executable(exporter exporter.cpp)
# needed for designated initializers
target_compile_features(exporter PRIVATE cxx_std_20)
target_link_libraries(exporter PRIVATE Datadog::Profiling)

add_executable(crashinfo crashinfo.cpp)
# needed for designated initializers
target_compile_features(crashinfo PRIVATE cxx_std_20)
target_link_libraries(crashinfo PRIVATE Datadog::Profiling)


if(CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
  target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
endif()

add_executable(profiles profiles.c)
target_link_libraries(profiles PRIVATE Datadog::Profiling)

if(BUILD_SYMBOLIZER)
  add_executable(symbolizer symbolizer.cpp)
  target_link_libraries(symbolizer PRIVATE Datadog::Profiling)
endif()

add_executable(telemetry telemetry.c)
target_link_libraries(telemetry PRIVATE Datadog::Profiling)

add_executable(telemetry_metrics telemetry_metrics.c)
target_link_libraries(telemetry_metrics PRIVATE Datadog::Profiling)

if(NOT WIN32)
  add_executable(crashtracking crashtracking.c)
  target_link_libraries(crashtracking PRIVATE Datadog::Profiling)
endif()
