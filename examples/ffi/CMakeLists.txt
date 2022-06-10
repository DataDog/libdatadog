project(ffi_examples)
cmake_minimum_required(VERSION 3.19)

find_package(DDProf)

add_executable(exporter exporter.cpp)

add_executable(profiles profiles.c)

set(EXTERNAL_LIBS DDProf::FFI)

if (CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
    set(EXTERNAL_LIBS "${EXTERNAL_LIBS};NtDll;UserEnv;Bcrypt;crypt32;wsock32;ws2_32;shlwapi")
endif()

if (CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
    target_compile_features(exporter PRIVATE cxx_std_20) # needed for designated initializers
    target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
else()
    target_compile_features(exporter PRIVATE cxx_std_11)
endif()

target_link_libraries(exporter ${EXTERNAL_LIBS})

target_link_libraries(profiles ${EXTERNAL_LIBS})
