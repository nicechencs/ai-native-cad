cmake_minimum_required(VERSION 3.22)
project(occt_bridge LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Locate prebuilt OCCT 7.8+ from scripts/fetch-occt-prebuilt or system OpenCASCADE
set(OCCT_PREBUILT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../occt-prebuilt" CACHE PATH "Path to prebuilt OCCT")
if(EXISTS "${OCCT_PREBUILT_DIR}/lib/cmake/opencascade")
    list(PREPEND CMAKE_PREFIX_PATH "${OCCT_PREBUILT_DIR}")
elseif(EXISTS "${OCCT_PREBUILT_DIR}/Library/lib/cmake/opencascade")
    list(PREPEND CMAKE_PREFIX_PATH "${OCCT_PREBUILT_DIR}/Library")
endif()

find_package(OpenCASCADE REQUIRED COMPONENTS
    TKernel
    TKMath
    TKBRep
    TKGeomBase
    TKGeomAlgo
    TKTopAlgo
    TKPrim
    TKBO
    TKMesh
    TKLCAF
    TKXCAF
    TKSTEP
    TKXDESTEP
)

# LGPL-2.1 compliance: build as a shared library dynamically linked against OCCT
add_library(occt_bridge SHARED
    src/occt_bridge.cpp
)

target_compile_definitions(occt_bridge PRIVATE OCCT_BRIDGE_EXPORTS)

target_include_directories(occt_bridge
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        ${OpenCASCADE_INCLUDE_DIR}
)

target_link_libraries(occt_bridge
    PRIVATE
        TKernel
        TKMath
        TKBRep
        TKGeomBase
        TKGeomAlgo
        TKTopAlgo
        TKPrim
        TKBO
        TKMesh
        TKLCAF
        TKXCAF
        TKSTEP
        TKXDESTEP
)
