cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_crashtracking_receiver LANGUAGES C CXX)

find_package(Datadog REQUIRED)

add_executable(libdatadog-crashtracking-receiver libdatadog-crashtracking-receiver.c)
install(TARGETS libdatadog-crashtracking-receiver RUNTIME)
target_link_libraries(libdatadog-crashtracking-receiver PRIVATE Datadog::Profiling)

if(APPLE)
  set_target_properties(libdatadog-crashtracking-receiver PROPERTIES
    INSTALL_RPATH "@loader_path/../lib"
    BUILD_WITH_INSTALL_RPATH TRUE
  )
endif()
