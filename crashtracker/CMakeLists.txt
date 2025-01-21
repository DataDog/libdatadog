cmake_minimum_required(VERSION 3.19)
project(datadog_profiling_crashtracking_receiver LANGUAGES C CXX)

find_package(Datadog REQUIRED)

# Add the executable
add_executable(libdatadog-crashtracking-receiver libdatadog-crashtracking-receiver.c)

# Link against Datadog Profiling
target_link_libraries(libdatadog-crashtracking-receiver PRIVATE Datadog::Profiling)

# Set the RPATH to look for libraries relative to the binary location
set_target_properties(libdatadog-crashtracking-receiver PROPERTIES
    INSTALL_RPATH "$ORIGIN/../lib"
    BUILD_WITH_INSTALL_RPATH TRUE
)

# Install the executable
install(TARGETS libdatadog-crashtracking-receiver RUNTIME)
