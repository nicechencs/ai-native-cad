#include "occt_topology.h"

#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRep_Tool.hxx>
#include <Geom_Curve.hxx>
#include <Geom_Surface.hxx>
#include <TopExp.hxx>
#include <TopLoc_Location.hxx>
#include <TopTools_IndexedDataMapOfShapeListOfShape.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Face.hxx>

#include <algorithm>
#include <limits>
#include <memory>
#include <stdexcept>
#include <vector>

namespace occt_topology {
namespace {
struct Storage {
    std::vector<int32_t> surface_kinds;
    std::vector<OcctTopologyEdge> edges;
    std::vector<OcctTopologyUse> uses;
};

int32_t surface_kind(const TopoDS_Face& face) {
    TopLoc_Location location;
    if (BRep_Tool::Surface(face, location).IsNull()) return 6;
    switch (BRepAdaptor_Surface(face).GetType()) {
        case GeomAbs_Plane: return 0;
        case GeomAbs_Cylinder: return 1;
        case GeomAbs_Cone: return 2;
        case GeomAbs_Sphere: return 3;
        case GeomAbs_Torus: return 4;
        case GeomAbs_BSplineSurface: return 5;
        default: return 6;
    }
}

int32_t curve_kind(const TopoDS_Edge& edge) {
    TopLoc_Location location;
    double first, last;
    // Degenerate or pcurve-only edges may have no standalone 3D curve.
    if (BRep_Tool::Curve(edge, location, first, last).IsNull()) return 4;
    switch (BRepAdaptor_Curve(edge).GetType()) {
        case GeomAbs_Line: return 0;
        case GeomAbs_Circle: return 1;
        case GeomAbs_Ellipse: return 2;
        case GeomAbs_BSplineCurve: return 3;
        default: return 4;
    }
}
}

void extract(const TopoDS_Shape& shape, OcctTopologyResult& out) {
    auto data = std::make_unique<Storage>();
    TopTools_IndexedMapOfShape faces, edges;
    // The mesh path maps faces in the same order on its topology-preserving copy.
    TopExp::MapShapes(shape, TopAbs_FACE, faces);
    TopExp::MapShapes(shape, TopAbs_EDGE, edges);
    TopTools_IndexedDataMapOfShapeListOfShape ancestors;
    // Deduplicate differently oriented uses of the same face, e.g. at seams.
    TopExp::MapShapesAndUniqueAncestors(shape, TopAbs_EDGE, TopAbs_FACE, ancestors);
    data->surface_kinds.reserve(faces.Extent());
    data->edges.reserve(edges.Extent());
    for (int i = 1; i <= faces.Extent(); ++i) {
        data->surface_kinds.push_back(surface_kind(TopoDS::Face(faces(i))));
    }
    for (int i = 1; i <= edges.Extent(); ++i) {
        const auto edge = TopoDS::Edge(edges(i));
        const auto offset = static_cast<uint32_t>(data->uses.size());
        const int ancestor_index = ancestors.FindIndex(edge);
        if (ancestor_index != 0) {
            for (const auto& ancestor : ancestors.FindFromIndex(ancestor_index)) {
                const int face_index = faces.FindIndex(ancestor);
                if (face_index == 0) throw std::logic_error("Unknown incident face");
                if (data->uses.size() == std::numeric_limits<uint32_t>::max()) {
                    throw std::overflow_error("Too many edge-face incidences");
                }
                data->uses.push_back({static_cast<uint32_t>(face_index - 1),
                    BRep_Tool::IsClosed(edge, TopoDS::Face(ancestor)) ? 1 : 0});
            }
            std::sort(data->uses.begin() + offset, data->uses.end(),
                [](const auto& a, const auto& b) { return a.face_index < b.face_index; });
        }
        const auto count = static_cast<uint32_t>(data->uses.size()) - offset;
        data->edges.push_back({curve_kind(edge), BRep_Tool::Degenerated(edge) ? 1 : 0, offset, count});
    }
    out = {data->surface_kinds.data(), data->edges.data(), data->uses.data(),
        static_cast<uint32_t>(data->surface_kinds.size()), static_cast<uint32_t>(data->edges.size()),
        static_cast<uint32_t>(data->uses.size()), data.get()};
    data.release();
}

void release(OcctTopologyResult& result) {
    delete static_cast<Storage*>(result.storage);
    result = {};
}
}
