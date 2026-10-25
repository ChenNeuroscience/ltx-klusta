cmake_minimum_required(VERSION 3.20)
project(prekk LANGUAGES CXX)

if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
    message(FATAL_ERROR "prekk requires a 64-bit target. On Windows, configure with -A x64.")
endif()

add_executable(prekk prekk.cpp pca.cpp utils.cpp)
target_compile_features(prekk PRIVATE cxx_std_11)
target_compile_definitions(prekk PRIVATE ARMA_64BIT_WORD ARMA_USE_CXX11 NOMINMAX)

if(MSVC)
    # Use the runtime built into the executable; ship the matching x64 BLAS/LAPACK DLLs.
    set_property(TARGET prekk PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
    target_link_libraries(prekk PRIVATE
        "${CMAKE_CURRENT_SOURCE_DIR}/blas_win64_MT.lib"
        "${CMAKE_CURRENT_SOURCE_DIR}/lapack_win64_MT.lib")
    foreach(dll blas_win64_MT.dll lapack_win64_MT.dll)
        add_custom_command(TARGET prekk POST_BUILD COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${CMAKE_CURRENT_SOURCE_DIR}/${dll}" "$<TARGET_FILE_DIR:prekk>")
    endforeach()
elseif(APPLE)
    find_library(ACCELERATE_FRAMEWORK Accelerate REQUIRED)
    target_link_libraries(prekk PRIVATE "${ACCELERATE_FRAMEWORK}")
else()
    find_package(BLAS REQUIRED)
    find_package(LAPACK REQUIRED)
    target_link_libraries(prekk PRIVATE ${LAPACK_LIBRARIES} ${BLAS_LIBRARIES})
endif()

include(CTest)
if(BUILD_TESTING)
    add_executable(size_limits tests/size_limits.cpp)
    target_compile_features(size_limits PRIVATE cxx_std_11)
    target_include_directories(size_limits PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
    add_test(NAME size_limits COMMAND size_limits)
    find_package(Python3 REQUIRED COMPONENTS Interpreter)
    add_test(NAME features COMMAND "${Python3_EXECUTABLE}"
        "${CMAKE_CURRENT_SOURCE_DIR}/tests/check_features.py" --exe "$<TARGET_FILE:prekk>")
endif()
