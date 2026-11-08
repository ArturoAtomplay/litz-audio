cmake_minimum_required(VERSION 3.22.1)
project(litz_android LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(oboe REQUIRED CONFIG)

if(NOT DEFINED LITZ_RUST_LIB_DIR)
    message(FATAL_ERROR "LITZ_RUST_LIB_DIR must point at the Gradle-built Rust static libraries")
endif()
set(LITZ_RUST_STATICLIB "${LITZ_RUST_LIB_DIR}/${ANDROID_ABI}/liblitz_android.a")
if(NOT EXISTS "${LITZ_RUST_STATICLIB}")
    message(FATAL_ERROR "Rust static library not found: ${LITZ_RUST_STATICLIB}")
endif()

add_library(litz_android SHARED oboe_adapter.cpp)
target_compile_options(litz_android PRIVATE -Wall -Wextra -Werror)

# The Rust archive contains the JNI entry points and calls the C++ adapter's
# symbols. Keep the whole archive so JNI exports cannot be dropped, then let
# --gc-sections remove the unused Rust standard-library and codegen sections.
target_link_libraries(litz_android PRIVATE
    "-Wl,--whole-archive"
    "${LITZ_RUST_STATICLIB}"
    "-Wl,--no-whole-archive"
    oboe::oboe
    log
)
target_link_options(litz_android PRIVATE
    "-Wl,--gc-sections"
    "-Wl,-z,max-page-size=16384,-z,common-page-size=16384"
)
