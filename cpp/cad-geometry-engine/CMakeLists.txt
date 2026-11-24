cmake_minimum_required(VERSION 3.22)
project(cad_geometry_engine VERSION 1.0.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

option(CAD_GEOMD_WITH_OCCT "Enable optional OCCT 7.9+ B-Rep kernel plugin" OFF)
option(CAD_GEOMD_WARNINGS_AS_ERRORS "Treat compiler warnings as errors" OFF)

find_package(Threads REQUIRED)

if(CAD_GEOMD_WITH_OCCT)
    set(OCCT_PREBUILT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../occt-prebuilt" CACHE PATH "Path to prebuilt OCCT")
    list(PREPEND CMAKE_PREFIX_PATH "${OCCT_PREBUILT_DIR}" "${OCCT_PREBUILT_DIR}/Library")
    find_package(OpenCASCADE CONFIG REQUIRED COMPONENTS
        FoundationClasses ModelingData ModelingAlgorithms
        ApplicationFramework DataExchange)
    if(OpenCASCADE_MAJOR_VERSION LESS 7 OR
       (OpenCASCADE_MAJOR_VERSION EQUAL 7 AND OpenCASCADE_MINOR_VERSION LESS 9))
        message(FATAL_ERROR
            "cad-geometry-engine requires OCCT 7.9+ when CAD_GEOMD_WITH_OCCT=ON (found ${OpenCASCADE_MAJOR_VERSION}.${OpenCASCADE_MINOR_VERSION})")
    endif()
endif()

function(cad_geomd_apply_warnings target_name)
    if(MSVC)
        target_compile_options(${target_name} PRIVATE /W4 /permissive- /utf-8)
        if(CAD_GEOMD_WARNINGS_AS_ERRORS)
            target_compile_options(${target_name} PRIVATE /WX)
        endif()
        target_compile_definitions(${target_name} PRIVATE NOMINMAX _CRT_SECURE_NO_WARNINGS)
    else()
        target_compile_options(${target_name} PRIVATE -Wall -Wextra -Wpedantic -Wshadow)
        if(CAD_GEOMD_WARNINGS_AS_ERRORS)
            target_compile_options(${target_name} PRIVATE -Werror)
        endif()
    endif()
endfunction()

add_library(cad_geometry_engine STATIC
    src/reference_analytic_kernel.cpp
    src/occt_kernel_plugin.cpp
    src/format_adapters.cpp
    src/worker_pool.cpp
    src/geomd_protocol.cpp
    src/geomd_service.cpp
)

target_include_directories(cad_geometry_engine
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
)

target_link_libraries(cad_geometry_engine
    PUBLIC
        Threads::Threads
)

cad_geomd_apply_warnings(cad_geometry_engine)

if(CAD_GEOMD_WITH_OCCT)
    target_compile_definitions(cad_geometry_engine PUBLIC CAD_GEOMD_HAS_OCCT=1)
    target_include_directories(cad_geometry_engine SYSTEM PRIVATE ${OpenCASCADE_INCLUDE_DIR})
    target_link_libraries(cad_geometry_engine PRIVATE
        TKernel TKMath TKBRep TKGeomBase TKG3d TKGeomAlgo TKTopAlgo TKPrim TKBO
        TKMesh TKCDF TKLCAF TKXCAF TKXSBase TKDESTEP TKDEIGES)
else()
    target_compile_definitions(cad_geometry_engine PUBLIC CAD_GEOMD_HAS_OCCT=0)
endif()

add_executable(cad-geomd
    src/geomd_main.cpp
)

target_link_libraries(cad-geomd
    PRIVATE
        cad_geometry_engine
)

cad_geomd_apply_warnings(cad-geomd)

include(CTest)
if(BUILD_TESTING)
    add_executable(geomd_service_tests
        tests/geomd_service_tests.cpp
    )
    target_link_libraries(geomd_service_tests
        PRIVATE
            cad_geometry_engine
            Threads::Threads
    )
    cad_geomd_apply_warnings(geomd_service_tests)
    add_test(
        NAME geomd_service_tests
        COMMAND geomd_service_tests $<TARGET_FILE:cad-geomd>
    )
endif()
