#include "occt_bridge.h"

#if defined(AI_NATIVE_CAD_HAS_OCCT)

#include "occt_freeform.h"

#include <BRepAdaptor_Surface.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepOffsetAPI_DraftAngle.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRep_Tool.hxx>
#include <GeomAPI_PointsToBSplineSurface.hxx>
#include <Geom_BSplineSurface.hxx>
#include <NCollection_Array2.hxx>
#include <NCollection_IndexedDataMap.hxx>
#include <NCollection_IndexedMap.hxx>
#include <NCollection_List.hxx>
#include <Precision.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_ShapeMapHasher.hxx>
#include <TopoDS.hxx>
#include <gp_Dir.hxx>
#include <gp_Pln.hxx>
#include <gp_Pnt.hxx>
#include <gp_Vec.hxx>

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <limits>
#include <numbers>

namespace {
namespace occt_freeform {

constexpr std::size_t max_xyz_points =
    std::numeric_limits<std::ptrdiff_t>::max() / sizeof(double) / 3;

bool finite_xyz(const double* xyz) {
    return xyz && std::isfinite(xyz[0]) && std::isfinite(xyz[1]) && std::isfinite(xyz[2]);
}

gp_Pnt point_from_xyz(const double* xyz) {
    return gp_Pnt(xyz[0], xyz[1], xyz[2]);
}

TopoDS_Wire make_section_wire(const double* xyz, int32_t count) {
    if (!finite_xyz(xyz)) {
        return {};
    }
    const gp_Pnt first = point_from_xyz(xyz);
    gp_Pnt previous = first;
    BRepBuilderAPI_MakePolygon polygon;
    polygon.Add(first);
    for (int32_t i = 1; i < count; ++i) {
        const double* coordinates = xyz + 3 * static_cast<std::size_t>(i);
        if (!finite_xyz(coordinates)) {
            return {};
        }
        const gp_Pnt point = point_from_xyz(coordinates);
        if (point.IsEqual(first, Precision::Confusion())) {
            // Accept an explicitly repeated closing point after three vertices.
            if (i == count - 1 && i >= 3) {
                break;
            }
            return {};
        }
        if (point.IsEqual(previous, Precision::Confusion())) {
            return {};
        }
        polygon.Add(point);
        previous = point;
    }
    polygon.Close();
    return polygon.IsDone() ? polygon.Wire() : TopoDS_Wire{};
}

bool has_solid(const TopoDS_Shape& shape) {
    return TopExp_Explorer(shape, TopAbs_SOLID).More();
}

} // namespace occt_freeform
} // namespace

extern "C" {

uint64_t occt_bridge_make_bspline_loft(
    int32_t section_count, const int32_t* section_point_counts,
    const double* xyz_points, int32_t make_solid, int32_t ruled) {
    const BridgeError error("make_bspline_loft");
    if (section_count < 2 || !section_point_counts || !xyz_points) {
        return error.no_handle(-1, "Invalid loft section count or point buffer");
    }
    return occt_freeform::run(error, [&]() -> uint64_t {
        BRepOffsetAPI_ThruSections loft(make_solid != 0, ruled != 0);
        std::size_t offset = 0;
        for (int32_t section = 0; section < section_count; ++section) {
            const int32_t count = section_point_counts[section];
            if (count < 3 || static_cast<std::size_t>(count) > occt_freeform::max_xyz_points - offset) {
                return error.no_handle(-1, "Invalid point count in loft section");
            }
            const auto wire = occt_freeform::make_section_wire(xyz_points + 3 * offset, count);
            if (wire.IsNull()) {
                return 0;
            }
            loft.AddWire(wire);
            offset += static_cast<std::size_t>(count);
        }
        loft.Build();
        return loft.IsDone() ? occt_freeform::store_result(loft.Shape(), make_solid != 0) : 0;
    });
}

uint64_t occt_bridge_draft_faces(
    uint64_t handle, double draft_angle_deg, const double pull_dir[3],
    const double neutral_origin[3], const double neutral_normal[3]) {
    const BridgeError error("draft_faces");
    if (!handle || !std::isfinite(draft_angle_deg) || draft_angle_deg == 0.0 ||
        std::abs(draft_angle_deg) >= 90.0 || !occt_freeform::finite_xyz(pull_dir) ||
        !occt_freeform::finite_xyz(neutral_origin) || !occt_freeform::finite_xyz(neutral_normal)) {
        return error.no_handle(-1, "Invalid shape handle, draft angle, direction, or neutral plane");
    }
    return occt_freeform::run(error, [&]() -> uint64_t {
        TopoDS_Shape shape;
        if (!get_shape(handle, shape)) {
            return error.no_handle(-1, "Invalid shape handle");
        }
        const gp_Dir pull(pull_dir[0], pull_dir[1], pull_dir[2]);
        const gp_Dir normal(neutral_normal[0], neutral_normal[1], neutral_normal[2]);
        const gp_Pln neutral(occt_freeform::point_from_xyz(neutral_origin), normal);
        const double angle = draft_angle_deg * (std::numbers::pi / 180.0);
        BRepOffsetAPI_DraftAngle draft(shape);
        NCollection_IndexedMap<TopoDS_Shape, TopTools_ShapeMapHasher> faces;
        TopExp::MapShapes(shape, TopAbs_FACE, faces);
        for (int i = 1; i <= faces.Extent(); ++i) {
            const auto& face = TopoDS::Face(faces(i));
            const BRepAdaptor_Surface surface(face);
            if (surface.GetType() != GeomAbs_Plane ||
                !surface.Plane().Axis().Direction().IsNormal(pull, Precision::Angular())) {
                continue;
            }
            const auto& modified = draft.ModifiedFaces();
            if (std::any_of(modified.begin(), modified.end(), [&](const TopoDS_Shape& item) {
                    return item.IsSame(face);
                })) {
                continue;
            }
            draft.Add(face, pull, angle, neutral);
            if (!draft.AddDone()) {
                return 0;
            }
        }
        if (draft.ModifiedFaces().IsEmpty()) {
            return 0;
        }
        draft.Build();
        return draft.IsDone() ? occt_freeform::store_result(draft.Shape(), occt_freeform::has_solid(shape)) : 0;
    });
}

uint64_t occt_bridge_variable_fillet(uint64_t handle, double r_start_mm, double r_end_mm) {
    const BridgeError error("variable_fillet");
    if (!handle || !std::isfinite(r_start_mm) || !std::isfinite(r_end_mm) ||
        r_start_mm <= 0.0 || r_end_mm <= 0.0) {
        return error.no_handle(-1, "Invalid shape handle or variable fillet radii");
    }
    return occt_freeform::run(error, [&]() -> uint64_t {
        TopoDS_Shape shape;
        if (!get_shape(handle, shape)) {
            return error.no_handle(-1, "Invalid shape handle");
        }
        BRepFilletAPI_MakeFillet fillet(shape);
        NCollection_IndexedDataMap<TopoDS_Shape, NCollection_List<TopoDS_Shape>, TopTools_ShapeMapHasher> edge_faces;
        TopExp::MapShapesAndUniqueAncestors(shape, TopAbs_EDGE, TopAbs_FACE, edge_faces);
        for (int i = 1; i <= edge_faces.Extent(); ++i) {
            const auto& edge = TopoDS::Edge(edge_faces.FindKey(i));
            const auto& faces = edge_faces.FindFromIndex(i);
            if (faces.Extent() != 2 || BRep_Tool::Degenerated(edge) || fillet.Contour(edge) != 0) {
                continue;
            }
            const auto& first = TopoDS::Face(faces.First());
            const auto& second = TopoDS::Face(faces.Last());
            if (BRep_Tool::Continuity(edge, first, second) >= GeomAbs_G1) {
                continue;
            }

            // OCCT's two-radius overload applies a linear law along the contour.
            fillet.Add(r_start_mm, r_end_mm, edge);
            const int contour = fillet.Contour(edge);
            if (contour != 0 && fillet.Closed(contour) && r_start_mm != r_end_mm) {
                // A closed contour has a single shared start/end vertex.
                fillet.Remove(edge);
            }
        }
        if (fillet.NbContours() == 0) {
            return error.no_handle(-3, "Shape has no edges for variable fillet");
        }
        fillet.Build();
        if (!fillet.IsDone()) return error.no_handle(-3, fillet_failure(fillet));
        return occt_freeform::store_result(fillet.Shape(), occt_freeform::has_solid(shape));
    });
}

uint64_t occt_bridge_quad_grid_to_bspline_surface(
    int32_t nu, int32_t nv, const double* xyz_grid, double thickness_mm) {
    const BridgeError error("quad_grid_to_bspline_surface");
    if (nu < 2 || nv < 2 || !xyz_grid || !std::isfinite(thickness_mm) || thickness_mm < 0.0 ||
        nu > std::numeric_limits<int>::max() / nv ||
        static_cast<std::size_t>(nu) > occt_freeform::max_xyz_points / static_cast<std::size_t>(nv)) {
        return error.no_handle(-1, "Invalid grid dimensions, point buffer, or thickness");
    }
    return occt_freeform::run(error, [&]() -> uint64_t {
        // GeomAPI_PointsToBSplineSurface expects one-based point indices.
        NCollection_Array2<gp_Pnt> points(1, nu, 1, nv);
        const double* xyz = xyz_grid;
        for (int32_t u = 0; u < nu; ++u) {
            for (int32_t v = 0; v < nv; ++v, xyz += 3) {
                if (!occt_freeform::finite_xyz(xyz)) {
                    return error.no_handle(-1, "Grid contains nonfinite coordinates");
                }
                points.SetValue(u + 1, v + 1, occt_freeform::point_from_xyz(xyz));
            }
        }
        GeomAPI_PointsToBSplineSurface fit;
        fit.Interpolate(points);
        if (!fit.IsDone()) {
            return 0;
        }
        const auto& surface = fit.Surface();
        BRepBuilderAPI_MakeFace face(surface, Precision::Confusion());
        if (!face.IsDone()) {
            return 0;
        }
        if (thickness_mm == 0.0) {
            return occt_freeform::store_result(face.Face(), false);
        }

        double u_min, u_max, v_min, v_max;
        surface->Bounds(u_min, u_max, v_min, v_max);
        gp_Pnt center;
        gp_Vec du, dv;
        surface->D1(0.5 * (u_min + u_max), 0.5 * (v_min + v_max), center, du, dv);
        const gp_Dir normal(du.Crossed(dv));
        BRepPrimAPI_MakePrism prism(face.Face(), gp_Vec(normal) * thickness_mm, true);
        return prism.IsDone() ? occt_freeform::store_result(prism.Shape(), true) : 0;
    });
}

} // extern "C"

#else

extern "C" {

uint64_t occt_bridge_make_bspline_loft(int32_t, const int32_t*, const double*, int32_t, int32_t) {
    const BridgeError error("make_bspline_loft");
    return error.no_handle(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_draft_faces(uint64_t, double, const double*, const double*, const double*) {
    const BridgeError error("draft_faces");
    return error.no_handle(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_variable_fillet(uint64_t, double, double) {
    const BridgeError error("variable_fillet");
    return error.no_handle(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_quad_grid_to_bspline_surface(int32_t, int32_t, const double*, double) {
    const BridgeError error("quad_grid_to_bspline_surface");
    return error.no_handle(-4, "OCCT backend not enabled");
}

} // extern "C"

#endif
