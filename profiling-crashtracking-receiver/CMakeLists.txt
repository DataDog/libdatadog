cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_crashtracking_reciever LANGUAGES C CXX)

find_package(Datadog REQUIRED)

add_executable(ddog-crashtracking-receiver profiling-crashtracking-receiver.c)
target_link_libraries(ddog-crashtracking-receiver PRIVATE Datadog::Profiling)
