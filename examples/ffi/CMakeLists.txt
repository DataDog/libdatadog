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

if(CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
  target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
endif()

add_executable(profiles profile_generator.c)
target_link_libraries(profiles PRIVATE Datadog::Profiling)
set_vcruntime_link_type(profiles ${VCRUNTIME_LINK_TYPE})
