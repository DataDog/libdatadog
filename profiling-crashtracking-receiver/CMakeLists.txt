cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_crashtracking_reciever LANGUAGES C CXX)

find_package(Datadog REQUIRED)

add_executable(profiles profiling-crashtracking-receiver.c)
target_link_libraries(profiles PRIVATE Datadog::Profiling)
