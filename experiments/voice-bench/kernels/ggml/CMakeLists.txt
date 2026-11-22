# Builds bench.c against the ggml that transcribe-cpp-sys ships, with the options transcribe.cpp
# uses on Windows and Linux: cmake -S . -B build -DGGML_DIR=<transcribe-cpp-sys>/ggml
cmake_minimum_required(VERSION 3.16)
project(kernel_bench_ggml C CXX)
set(GGML_DIR "" CACHE PATH "the ggml source folder")
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(GGML_BLAS OFF CACHE BOOL "" FORCE)
set(GGML_LLAMAFILE ON CACHE BOOL "" FORCE)
set(GGML_OPENMP OFF CACHE BOOL "" FORCE)
set(GGML_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(GGML_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
add_subdirectory(${GGML_DIR} ggml EXCLUDE_FROM_ALL)
add_executable(kernel-bench-ggml bench.c)
target_link_libraries(kernel-bench-ggml PRIVATE ggml)
