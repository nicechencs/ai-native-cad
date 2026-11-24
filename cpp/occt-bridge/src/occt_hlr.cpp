#include "occt_hlr.h"

#include <BRepAdaptor_Curve.hxx>
#include <BRepLib.hxx>
#include <GCPnts_QuasiUniformDeflection.hxx>
#include <HLRAlgo_Projector.hxx>
#include <HLRBRep_Algo.hxx>
#include <HLRBRep_HLRToShape.hxx>
#include <Precision.hxx>
#include <TopExp_Explorer.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Edge.hxx>
#include <gp_Ax2.hxx>

#include <cmath>
#include <limits>
#include <memory>
#include <stdexcept>
#include <vector>

namespace {
struct HlrStorage {
    std::vector<OcctHlrPoint> points;
    std::vector<OcctHlrPolyline> polylines;
};

bool unit_vector(const double* input, double (&unit)[3]) {
    if (!input) return false;
    const double length = std::hypot(input[0], input[1], input[2]);
    if (!std::isfinite(length) || length <= 0.0) return false;
    for (int i = 0; i < 3; ++i) unit[i] = input[i] / length;
    return true;
}

void append_edges(const TopoDS_Shape& edges, bool hidden, double deflection, HlrStorage& result) {
    if (edges.IsNull()) return;
    for (TopExp_Explorer ex(edges, TopAbs_EDGE); ex.More(); ex.Next()) {
        const TopoDS_Edge& edge = TopoDS::Edge(ex.Current());
        if (!BRepLib::BuildCurve3d(edge)) throw std::runtime_error("missing projected HLR curve");
        BRepAdaptor_Curve curve(edge);
        const size_t offset = result.points.size();
        const auto append = [&](const gp_Pnt& p) {
            if (!std::isfinite(p.X()) || !std::isfinite(p.Y())) {
                throw std::runtime_error("non-finite projected HLR point");
            }
            if (result.points.size() > offset) {
                const auto& prev = result.points.back();
                if (std::hypot(p.X() - prev.x, p.Y() - prev.y) <= Precision::Confusion()) return;
            }
            if (result.points.size() == std::numeric_limits<uint32_t>::max()) {
                throw std::length_error("too many projected HLR points");
            }
            result.points.push_back({p.X(), p.Y()});
        };
        if (curve.GetType() == GeomAbs_Line) {
            append(curve.Value(curve.FirstParameter()));
            append(curve.Value(curve.LastParameter()));
        } else {
            GCPnts_QuasiUniformDeflection samples(curve, deflection, GeomAbs_C0);
            if (!samples.IsDone()) throw std::runtime_error("could not sample projected HLR curve");
            const int count = samples.NbPoints();
            for (int i = 0; i < count; ++i) append(samples.Value(i + 1));
        }
        const auto count = result.points.size() - offset;
        if (count < 2) {
            result.points.resize(offset);
            continue;
        }
        result.polylines.push_back({static_cast<uint32_t>(offset), static_cast<uint32_t>(count), hidden});
    }
}
}

int32_t occt_hlr::compute(const TopoDS_Shape& shape, const double toward_eye[3], const double view_x[3],
                          double deflection_mm, OcctHlrResult& out) {
    double eye[3], x[3];
    if (!unit_vector(toward_eye, eye) || !unit_vector(view_x, x) ||
        !std::isfinite(deflection_mm) || deflection_mm < Precision::Confusion()) return -1;
    const gp_Dir z_dir(eye[0], eye[1], eye[2]);
    const gp_Dir x_dir(x[0], x[1], x[2]);
    if (std::abs(z_dir.Dot(x_dir)) > 1.0 - Precision::Angular()) return -1;

    Handle(HLRBRep_Algo) algo = new HLRBRep_Algo();
    algo->Projector(HLRAlgo_Projector(gp_Ax2(gp_Pnt(0.0, 0.0, 0.0), z_dir, x_dir)));
    // Run visibility on the B-Rep, never its stored tessellation. Curves are
    // sampled only after visibility has split them into visible/hidden intervals.
    algo->Add(shape, 0);
    algo->Update();
    algo->Hide();
    HLRBRep_HLRToShape lines(algo);
    auto storage = std::make_unique<HlrStorage>();
    // Sharp and silhouette lines only; exclude seams and surface isoparameters.
    append_edges(lines.VCompound(), false, deflection_mm, *storage);
    append_edges(lines.OutLineVCompound(), false, deflection_mm, *storage);
    append_edges(lines.HCompound(), true, deflection_mm, *storage);
    append_edges(lines.OutLineHCompound(), true, deflection_mm, *storage);
    out.points = storage->points.data();
    out.polylines = storage->polylines.data();
    out.point_count = static_cast<uint32_t>(storage->points.size());
    out.polyline_count = static_cast<uint32_t>(storage->polylines.size());
    out.storage = storage.release();
    return OCCT_OK;
}

void occt_hlr::release(OcctHlrResult& out) {
    delete static_cast<HlrStorage*>(out.storage);
    out = {};
}
