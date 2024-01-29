cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_crashtracking_reciever LANGUAGES C CXX)

find_package(Datadog REQUIRED)

add_executable(libdatadog-crashtracking-receiver libdatadog-crashtracking-receiver.c)
target_link_libraries(libdatadog-crashtracking-receiver PRIVATE Datadog::Profiling)
