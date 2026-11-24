// Native OCCT sketch-feature and primitive constructors: arbitrary-axis
// cylinders, polyline sweeps and polygon lofts.
//
// This private translation unit follows occt_bridge.cpp in the CMake unity
// group and reuses its handle registry, kernel lock and error reporting.
// Every constructor validates its inputs, builds exact B-Rep geometry and
// stores only a closed solid with positive volume; anything else is an
// explicit failure (handle 0 plus a thread-local error), never a fallback.
#include "occt_bridge.h"

#if defined(AI_NATIVE_CAD_HAS_OCCT)

#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_TransitionMode.hxx>
#include <BRepOffsetAPI_MakePipeShell.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <Precision.hxx>
#include <TopoDS_Wire.hxx>
#include <gp_Ax2.hxx>
#include <gp_Dir.hxx>
#include <gp_Pnt.hxx>
#include <gp_Vec.hxx>

#include <cmath>
#include <cstddef>
#include <limits>
#include <vector>

namespace {
namespace occt_construct {

// Upper bound on vertex counts accepted through the ABI. It keeps index
// arithmetic far from overflow and rejects corrupted counts early.
constexpr int32_t kMaxPoints = 1 << 20;

bool finite3(const double* xyz) {
    return xyz && std::isfinite(xyz[0]) && std::isfinite(xyz[1]) && std::isfinite(xyz[2]);
}

// Closed polygon wire through `points`; rejects repeated consecutive points.
TopoDS_Wire closed_wire(const std::vector<gp_Pnt>& points) {
    if (points.size() < 3) return {};
    BRepBuilderAPI_MakePolygon polygon;
    for (std::size_t i = 0; i < points.size(); ++i) {
        const gp_Pnt& next = points[(i + 1) % points.size()];
        if (points[i].IsEqual(next, Precision::Confusion())) return {};
        polygon.Add(points[i]);
    }
    polygon.Close();
    return polygon.IsDone() ? polygon.Wire() : TopoDS_Wire{};
}

// Drop an explicitly repeated closing vertex.
int32_t open_count(const double* xy_pairs, int32_t count) {
    if (count > 3 && xy_pairs[0] == xy_pairs[2 * (count - 1)] &&
        xy_pairs[1] == xy_pairs[2 * (count - 1) + 1]) {
        return count - 1;
    }
    return count;
}

} // namespace occt_construct
} // namespace

extern "C" {

uint64_t occt_bridge_make_cylinder_axis(double diameter, double height, const double origin[3],
                                        const double axis[3]) {
    const BridgeError error("make_cylinder_axis");
    if (!std::isfinite(diameter) || !std::isfinite(height) || diameter <= 0.0 || height <= 0.0) {
        return error.no_handle(-1, "Cylinder diameter and height must be finite and positive");
    }
    if (!occt_construct::finite3(origin) || !occt_construct::finite3(axis)) {
        return error.no_handle(-1, "Cylinder origin and axis must contain three finite values");
    }
    const double length = std::hypot(axis[0], axis[1], axis[2]);
    if (!std::isfinite(length) || length <= 1.0e-12) {
        return error.no_handle(-1, "Cylinder axis must have a finite nonzero length");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const gp_Dir direction(axis[0] / length, axis[1] / length, axis[2] / length);
        // Deterministic seam: u = normalize(r x axis), r = |axis.z| < 0.9 ? +Z : +X,
        // the same frame the sketch/preview layer uses for this axis.
        const gp_Dir reference = std::abs(direction.Z()) < 0.9 ? gp_Dir(0.0, 0.0, 1.0)
                                                               : gp_Dir(1.0, 0.0, 0.0);
        const gp_Ax2 frame(gp_Pnt(origin[0], origin[1], origin[2]), direction,
                           reference.Crossed(direction));
        BRepPrimAPI_MakeCylinder builder(frame, 0.5 * diameter, height);
        builder.Build();
        if (!builder.IsDone()) {
            return error.no_handle(-3, "OCCT cylinder builder did not complete");
        }
        return error.handle_result(store_valid_solid(builder.Shape(), MeshOptions{}),
                                   "OCCT cylinder is not a valid closed solid");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_sweep_polygon(const double* profile_xy, int32_t profile_count,
                                   const double* path_xyz, int32_t path_count) {
    const BridgeError error("sweep_polygon");
    if (!profile_xy || !path_xyz || profile_count < 3 || path_count < 2 ||
        profile_count > occt_construct::kMaxPoints || path_count > occt_construct::kMaxPoints) {
        return error.no_handle(-1, "Sweep needs a profile of at least 3 points and a path of at least 2");
    }
    const int32_t vertices = occt_construct::open_count(profile_xy, profile_count);
    if (!is_valid_polygon_params(0, profile_xy, vertices, 1.0)) {
        return error.no_handle(-1, "Sweep profile must be a finite polygon with nonzero area");
    }
    std::vector<gp_Pnt> path;
    path.reserve(static_cast<std::size_t>(path_count));
    for (int32_t i = 0; i < path_count; ++i) {
        const double* p = path_xyz + 3 * static_cast<std::size_t>(i);
        if (!occt_construct::finite3(p)) {
            return error.no_handle(-1, "Sweep path points must be finite");
        }
        path.emplace_back(p[0], p[1], p[2]);
        if (i > 0 && path[i].IsEqual(path[i - 1], Precision::Confusion())) {
            return error.no_handle(-1, "Sweep path has repeated consecutive points");
        }
    }
    if (path.size() > 2 && path.front().IsEqual(path.back(), Precision::Confusion())) {
        return error.no_handle(-1, "Closed sweep paths are not supported");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const gp_Dir tangent(gp_Vec(path[0], path[1]));
        const gp_Dir reference = std::abs(tangent.Z()) < 0.9 ? gp_Dir(0.0, 0.0, 1.0)
                                                             : gp_Dir(1.0, 0.0, 0.0);
        const gp_Dir u = reference.Crossed(tangent);
        const gp_Dir v = tangent.Crossed(u);
        std::vector<gp_Pnt> profile;
        profile.reserve(static_cast<std::size_t>(vertices));
        for (int32_t i = 0; i < vertices; ++i) {
            const double x = profile_xy[2 * static_cast<std::size_t>(i)];
            const double y = profile_xy[2 * static_cast<std::size_t>(i) + 1];
            profile.push_back(path[0].Translated(gp_Vec(u) * x + gp_Vec(v) * y));
        }
        const TopoDS_Wire section = occt_construct::closed_wire(profile);
        if (section.IsNull()) return error.no_handle(-1, "Sweep profile has repeated consecutive points");

        BRepBuilderAPI_MakePolygon spine;
        for (const auto& point : path) spine.Add(point);
        if (!spine.IsDone()) return error.no_handle(-3, "OCCT could not build the sweep spine");

        BRepOffsetAPI_MakePipeShell pipe(spine.Wire());
        pipe.SetTransitionMode(BRepBuilderAPI_RightCorner);
        pipe.Add(section, Standard_False, Standard_False);
        pipe.Build();
        if (!pipe.IsDone()) return error.no_handle(-3, "OCCT pipe-shell sweep did not complete");
        if (!pipe.MakeSolid()) return error.no_handle(-3, "OCCT could not close the sweep into a solid");
        return error.handle_result(store_valid_solid(pipe.Shape(), MeshOptions{}),
                                   "Sweep did not produce a valid closed solid (self-intersecting path?)");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_loft_polygons(int32_t section_count, const int32_t* section_point_counts,
                                   const int32_t* plane_codes, const double* offsets,
                                   const double* xy_pairs, int32_t ruled) {
    const BridgeError error("loft_polygons");
    if (section_count < 2 || section_count > occt_construct::kMaxPoints || !section_point_counts ||
        !plane_codes || !offsets || !xy_pairs) {
        return error.no_handle(-1, "Loft needs at least two sections and non-null buffers");
    }
    std::vector<std::vector<gp_Pnt>> sections(static_cast<std::size_t>(section_count));
    std::size_t base = 0;
    for (int32_t s = 0; s < section_count; ++s) {
        const int32_t count = section_point_counts[s];
        if (count < 3 || count > occt_construct::kMaxPoints || !std::isfinite(offsets[s])) {
            return error.no_handle(-1, "Loft section needs at least 3 points and a finite offset");
        }
        const double* xy = xy_pairs + 2 * base;
        const int32_t vertices = occt_construct::open_count(xy, count);
        if (!is_valid_polygon_params(plane_codes[s], xy, vertices, 1.0)) {
            return error.no_handle(-1, "Loft section has an invalid plane, non-finite point or zero area");
        }
        auto& points = sections[static_cast<std::size_t>(s)];
        points.reserve(static_cast<std::size_t>(vertices));
        for (int32_t i = 0; i < vertices; ++i) {
            points.push_back(map_plane_point(plane_codes[s], xy[2 * static_cast<std::size_t>(i)],
                                             xy[2 * static_cast<std::size_t>(i) + 1], offsets[s]));
        }
        base += static_cast<std::size_t>(count);
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        BRepOffsetAPI_ThruSections loft(Standard_True, ruled != 0, 1.0e-6);
        for (const auto& points : sections) {
            const TopoDS_Wire wire = occt_construct::closed_wire(points);
            if (wire.IsNull()) return error.no_handle(-1, "Loft section has repeated consecutive points");
            loft.AddWire(wire);
        }
        loft.Build();
        if (!loft.IsDone()) return error.no_handle(-3, "OCCT loft builder did not complete");
        return error.handle_result(store_valid_solid(loft.Shape(), MeshOptions{}),
                                   "Loft did not produce a valid closed solid");
    } catch (...) {
        return error.handle_exception();
    }
}

} // extern "C"

#else

extern "C" {

uint64_t occt_bridge_make_cylinder_axis(double diameter, double height, const double origin[3],
                                        const double axis[3]) {
    const BridgeError error("make_cylinder_axis");
    const auto finite3 = [](const double* xyz) {
        return xyz && std::isfinite(xyz[0]) && std::isfinite(xyz[1]) && std::isfinite(xyz[2]);
    };
    if (!std::isfinite(diameter) || !std::isfinite(height) || diameter <= 0.0 || height <= 0.0 ||
        !finite3(origin) || !finite3(axis)) {
        return error.no_handle(-1, "Dimensions must be positive and all parameters finite");
    }
    const double length = std::hypot(axis[0], axis[1], axis[2]);
    if (!std::isfinite(length) || length <= 1.0e-12) {
        return error.no_handle(-1, "Cylinder axis must have a finite nonzero length");
    }
    const double r = 0.5 * diameter;
    constexpr double kPi = 3.14159265358979323846;
    OcctBridgeMetrics m{};
    m.watertight = 1;
    m.volume_mm3 = kPi * r * r * height;
    for (int i = 0; i < 3; ++i) {
        const double a = axis[i] / length;
        const double radial = r * std::sqrt(std::max(0.0, 1.0 - a * a));
        const double end = origin[i] + height * a;
        m.bbox.min[i] = std::min(origin[i], end) - radial;
        m.bbox.max[i] = std::max(origin[i], end) + radial;
    }
    m.face_count = 3;
    m.edge_count = 3;
    return store_mock_solid(m);
}

uint64_t occt_bridge_sweep_polygon(const double*, int32_t, const double*, int32_t) {
    const BridgeError error("sweep_polygon");
    return error.no_handle(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_loft_polygons(int32_t, const int32_t*, const int32_t*, const double*,
                                   const double*, int32_t) {
    const BridgeError error("loft_polygons");
    return error.no_handle(-4, "OCCT backend not enabled");
}

} // extern "C"

#endif
