cmake_minimum_required(VERSION 3.22)
project(occt_bridge LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Locate prebuilt OCCT 7.9+ from scripts/fetch-occt-prebuilt or system OpenCASCADE
set(OCCT_PREBUILT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../occt-prebuilt" CACHE PATH "Path to prebuilt OCCT")
list(PREPEND CMAKE_PREFIX_PATH "${OCCT_PREBUILT_DIR}" "${OCCT_PREBUILT_DIR}/Library")
option(OCCT_BRIDGE_WITH_OCCT "Build the real OCCT backend" ON)
if(OCCT_BRIDGE_WITH_OCCT)
    # OpenCASCADE's package components are modules, not toolkit names.
    find_package(OpenCASCADE CONFIG REQUIRED COMPONENTS
        FoundationClasses ModelingData ModelingAlgorithms Visualization
        ApplicationFramework DataExchange)
    if(OpenCASCADE_MAJOR_VERSION LESS 7 OR
       (OpenCASCADE_MAJOR_VERSION EQUAL 7 AND OpenCASCADE_MINOR_VERSION LESS 9))
        message(FATAL_ERROR
            "occt_bridge requires OCCT 7.9 or newer (found ${OpenCASCADE_MAJOR_VERSION}.${OpenCASCADE_MINOR_VERSION}; occt_xcaf.cpp requires DESTEP_Parameters.hxx from OCCT 7.9+)")
    endif()
endif()

# LGPL-2.1 compliance: build as a shared library dynamically linked against OCCT
add_library(occt_bridge SHARED
    src/occt_bridge.cpp
    src/occt_construct.cpp
    src/occt_freeform.cpp
    src/occt_xcaf.cpp
)

# Keep the registry and kernel lock private. The unity group includes these
# sources in target order so the constructor, freeform and XCAF entry points
# can share them.
set_target_properties(occt_bridge PROPERTIES UNITY_BUILD ON UNITY_BUILD_MODE GROUP)
set_source_files_properties(src/occt_bridge.cpp src/occt_construct.cpp src/occt_freeform.cpp
    src/occt_xcaf.cpp PROPERTIES UNITY_GROUP bridge_registry)

# Ensure occt_bridge uses portable relative library search paths in installers and portable bundles.
if(APPLE)
    set_target_properties(occt_bridge PROPERTIES
        MACOSX_RPATH ON
        INSTALL_NAME_DIR "@rpath"
        BUILD_WITH_INSTALL_NAME_DIR ON
        BUILD_RPATH "@loader_path;${OCCT_PREBUILT_DIR}/lib"
        INSTALL_RPATH "@loader_path"
    )
elseif(UNIX)
    set_target_properties(occt_bridge PROPERTIES
        BUILD_RPATH "\$ORIGIN;${OCCT_PREBUILT_DIR}/lib"
        INSTALL_RPATH "\$ORIGIN"
    )
endif()

target_compile_definitions(occt_bridge PRIVATE OCCT_BRIDGE_EXPORTS)
if(WIN32)
    target_compile_definitions(occt_bridge PRIVATE NOMINMAX)
endif()

target_include_directories(occt_bridge PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>)

if(OCCT_BRIDGE_WITH_OCCT)
    target_sources(occt_bridge PRIVATE src/occt_hlr.cpp)
    target_sources(occt_bridge PRIVATE src/occt_topology.cpp)
    target_compile_definitions(occt_bridge PRIVATE AI_NATIVE_CAD_HAS_OCCT)
    target_include_directories(occt_bridge SYSTEM PRIVATE ${OpenCASCADE_INCLUDE_DIR})
    target_link_libraries(occt_bridge PRIVATE
        TKernel TKMath TKBRep TKGeomBase TKG3d TKGeomAlgo TKTopAlgo TKPrim TKBO
        TKFillet TKOffset TKShHealing TKMesh TKHLR TKCDF TKLCAF TKXCAF TKXSBase TKDESTEP TKDEIGES)
else()
    target_compile_definitions(occt_bridge PRIVATE AI_NATIVE_CAD_NO_OCCT)
endif()

include(CTest)
if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/last_error_abi.cpp")
    find_package(Threads REQUIRED)
    add_executable(occt_bridge_last_error_abi_test tests/last_error_abi.cpp)
    target_include_directories(occt_bridge_last_error_abi_test PRIVATE src)
    target_link_libraries(occt_bridge_last_error_abi_test PRIVATE occt_bridge Threads::Threads)
    add_test(NAME occt_bridge_last_error_abi COMMAND occt_bridge_last_error_abi_test)
    if(OCCT_BRIDGE_WITH_OCCT)
        target_compile_definitions(occt_bridge_last_error_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        if(WIN32)
            set_property(TEST occt_bridge_last_error_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()

if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/hlr_abi.cpp")
    add_executable(occt_bridge_hlr_abi_test tests/hlr_abi.cpp)
    target_link_libraries(occt_bridge_hlr_abi_test PRIVATE occt_bridge)
    add_test(NAME occt_bridge_hlr_abi COMMAND occt_bridge_hlr_abi_test)
    if(OCCT_BRIDGE_WITH_OCCT)
        target_compile_definitions(occt_bridge_hlr_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        if(WIN32)
            set_property(TEST occt_bridge_hlr_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()

if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/freeform_abi.cpp")
    add_executable(occt_bridge_freeform_abi_test tests/freeform_abi.cpp)
    target_link_libraries(occt_bridge_freeform_abi_test PRIVATE occt_bridge)
    add_test(NAME occt_bridge_freeform_abi COMMAND occt_bridge_freeform_abi_test)
    if(OCCT_BRIDGE_WITH_OCCT)
        target_compile_definitions(occt_bridge_freeform_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        if(WIN32)
            set_property(TEST occt_bridge_freeform_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()

if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/import_abi.cpp")
    add_executable(occt_bridge_import_abi_test tests/import_abi.cpp)
    target_link_libraries(occt_bridge_import_abi_test PRIVATE occt_bridge)
    add_test(NAME occt_bridge_import_abi COMMAND occt_bridge_import_abi_test)
    if(MSVC)
        target_compile_options(occt_bridge_import_abi_test PRIVATE /utf-8)
    endif()
    if(OCCT_BRIDGE_WITH_OCCT)
        find_package(Threads REQUIRED)
        target_compile_definitions(occt_bridge_import_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        target_include_directories(occt_bridge_import_abi_test SYSTEM PRIVATE ${OpenCASCADE_INCLUDE_DIR})
        target_link_libraries(occt_bridge_import_abi_test PRIVATE Threads::Threads
            TKernel TKMath TKBRep TKTopAlgo TKPrim TKCDF TKLCAF TKXCAF TKXSBase TKDESTEP TKDEIGES)
        if(WIN32)
            set_property(TEST occt_bridge_import_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()

if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/kernel_ops_abi.cpp")
    # Links every exported modelling/exchange symbol so a missing export fails the build.
    add_executable(occt_bridge_kernel_ops_abi_test tests/kernel_ops_abi.cpp)
    target_link_libraries(occt_bridge_kernel_ops_abi_test PRIVATE occt_bridge)
    add_test(NAME occt_bridge_kernel_ops_abi COMMAND occt_bridge_kernel_ops_abi_test)
    if(OCCT_BRIDGE_WITH_OCCT)
        target_compile_definitions(occt_bridge_kernel_ops_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        if(WIN32)
            set_property(TEST occt_bridge_kernel_ops_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()

if(BUILD_TESTING AND EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/tests/xcaf_abi.cpp")
    add_executable(occt_bridge_xcaf_abi_test tests/xcaf_abi.cpp)
    target_include_directories(occt_bridge_xcaf_abi_test PRIVATE src)
    target_link_libraries(occt_bridge_xcaf_abi_test PRIVATE occt_bridge)
    add_test(NAME occt_bridge_xcaf_abi COMMAND occt_bridge_xcaf_abi_test)
    if(OCCT_BRIDGE_WITH_OCCT)
        target_compile_definitions(occt_bridge_xcaf_abi_test PRIVATE AI_NATIVE_CAD_HAS_OCCT)
        target_include_directories(occt_bridge_xcaf_abi_test SYSTEM PRIVATE ${OpenCASCADE_INCLUDE_DIR})
        target_link_libraries(occt_bridge_xcaf_abi_test PRIVATE
            TKernel TKMath TKBRep TKTopAlgo TKPrim TKCDF TKLCAF TKXCAF TKXSBase TKDESTEP)
        if(WIN32)
            set_property(TEST occt_bridge_xcaf_abi PROPERTY ENVIRONMENT_MODIFICATION
                "PATH=path_list_prepend:$<TARGET_FILE_DIR:TKernel>")
        endif()
    endif()
endif()
