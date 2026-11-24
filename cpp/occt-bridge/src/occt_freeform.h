#pragma once

#include <BRepCheck_Analyzer.hxx>
#include <TopoDS_Shape.hxx>

#include <cstdint>
#include <mutex>

// This private adapter follows occt_bridge.cpp in the CMake unity group.
// It uses the same lock and handle registry as the other bridge operations.
namespace {
namespace occt_freeform {

template <typename Build>
uint64_t run(const BridgeError& error, Build build) {
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto result = build();
        if (g_last_error.status_code != OCCT_OK) return result;
        return error.handle_result(result, "OCCT did not produce a valid shape");
    } catch (...) {
        return error.handle_exception();
    }
}

inline uint64_t store_result(const TopoDS_Shape& shape, bool require_solid) {
    if (shape.IsNull()) {
        return 0;
    }
    if (require_solid) {
        double volume = 0.0;
        if (!is_watertight(shape) || !measure_brep_volume(shape, volume) || volume <= 0.0) {
            return 0;
        }
    } else if (!BRepCheck_Analyzer(shape).IsValid()) {
        return 0;
    }
    return store_shape(shape);
}

} // namespace occt_freeform
} // namespace
