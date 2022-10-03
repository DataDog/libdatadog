project(datadog_profiling_ffi_examples LANGUAGES C CXX)
cmake_minimum_required(VERSION 3.19)

find_package(Datadog REQUIRED)

add_executable(exporter exporter.cpp)
# needed for designated initializers
target_compile_features(exporter PRIVATE cxx_std_20)
target_link_libraries(exporter PRIVATE Datadog::Profiling)

if(CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
  target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
endif()

add_executable(profiles profiles.c)
target_link_libraries(profiles PRIVATE Datadog::Profiling)

add_executable(telemetry telemetry.cpp)
target_link_libraries(telemetry PRIVATE Datadog::Profiling)
