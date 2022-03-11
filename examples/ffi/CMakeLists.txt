project(ffi_examples)
cmake_minimum_required(VERSION 3.19)

find_package(DDProf)

add_executable(exporter exporter.cpp)
target_compile_features(exporter PRIVATE cxx_std_11)
target_link_libraries(exporter DDProf::FFI)
add_executable(profiles profiles.c)
target_link_libraries(profiles DDProf::FFI)
