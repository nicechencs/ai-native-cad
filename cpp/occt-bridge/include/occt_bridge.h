#pragma once

#include <cstdint>

#ifdef _WIN32
#  ifdef OCCT_BRIDGE_EXPORTS
#    define OCCT_BRIDGE_API __declspec(dllexport)
#  else
#    define OCCT_BRIDGE_API __declspec(dllimport)
#  endif
#else
#  define OCCT_BRIDGE_API __attribute__((visibility("default")))
#endif

extern "C" {

struct OcctBridgeBBox {
    double min[3];
    double max[3];
};

struct OcctBridgeMetrics {
    int32_t watertight;
    double volume_mm3;
    OcctBridgeBBox bbox;
    int32_t face_count;
    int32_t edge_count;
};

OCCT_BRIDGE_API const char* occt_bridge_version();

OCCT_BRIDGE_API uint64_t occt_bridge_make_box(
    double dx,
    double dy,
    double dz,
    double ox,
    double oy,
    double oz);

OCCT_BRIDGE_API uint64_t occt_bridge_make_cylinder(
    double diameter,
    double height,
    double ox,
    double oy,
    double oz);

OCCT_BRIDGE_API uint64_t occt_bridge_boolean_cut(
    uint64_t base_handle,
    uint64_t tool_handle);

OCCT_BRIDGE_API int32_t occt_bridge_check(
    uint64_t handle,
    OcctBridgeMetrics* out_metrics);

OCCT_BRIDGE_API int32_t occt_bridge_export_step(
    uint64_t handle,
    const char* path_utf8);

OCCT_BRIDGE_API void occt_bridge_free(uint64_t handle);

} // extern "C"
