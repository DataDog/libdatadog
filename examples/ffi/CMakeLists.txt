project(ffi_examples)
cmake_minimum_required(VERSION 3.19)

find_package(DDProf)

add_executable(exporter exporter.cpp)

add_executable(profiles profiles.c)

if (CMAKE_CXX_COMPILER_ID MATCHES "MSVC")
    target_compile_features(exporter PRIVATE cxx_std_20) # needed for designated initializers
    target_compile_definitions(exporter PUBLIC _CRT_SECURE_NO_WARNINGS)
    target_link_libraries(exporter
        NtDll
        UserEnv
        Bcrypt
        crypt32
        wsock32
        ws2_32
        shlwapi)

    target_link_libraries(profiles
        NtDll
        UserEnv
        Bcrypt
        crypt32
        wsock32
        ws2_32
        shlwapi)
else()
    target_compile_features(exporter PRIVATE cxx_std_11)
endif()

target_link_libraries(exporter DDProf::FFI)

target_link_libraries(profiles DDProf::FFI)
