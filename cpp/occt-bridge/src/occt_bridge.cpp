#include "occt_bridge.h"

#include <BRepAlgoAPI_Cut.hxx>
#include <BRepBndLib.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepGProp.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <Bnd_Box.hxx>
#include <GProp_GProps.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <STEPCAFControl_Writer.hxx>
#include <Standard_Failure.hxx>
#include <TDocStd_Document.hxx>
#include <TopExp_Explorer.hxx>
#include <TopoDS_Shape.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <gp_Ax2.hxx>
#include <gp_Dir.hxx>
#include <gp_Pnt.hxx>

#include <atomic>
#include <memory>
#include <mutex>
#include <unordered_map>

namespace {

std::mutex g_registry_mutex;
std::atomic<uint64_t> g_next_handle{1};
std::unordered_map<uint64_t, TopoDS_Shape> g_shapes;

uint64_t store_shape(TopoDS_Shape shape) {
    if (shape.IsNull()) {
        return 0;
    }
    const uint64_t handle = g_next_handle.fetch_add(1, std::memory_order_relaxed);
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_shapes.emplace(handle, std::move(shape));
    return handle;
}

bool get_shape(uint64_t handle, TopoDS_Shape& out) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    const auto it = g_shapes.find(handle);
    if (it == g_shapes.end()) {
        return false;
    }
    out = it->second;
    return true;
}

int32_t count_subshapes(const TopoDS_Shape& shape, TopAbs_ShapeEnum kind) {
    int32_t count = 0;
    for (TopExp_Explorer exp(shape, kind); exp.More(); exp.Next()) {
        ++count;
    }
    return count;
}

} // namespace

extern "C" {

const char* occt_bridge_version() {
    return "occt-bridge/0.1.0 (OCCT dynamic LGPL-2.1 boundary)";
}

uint64_t occt_bridge_make_box(
    double dx,
    double dy,
    double dz,
    double ox,
    double oy,
    double oz) {
    try {
        gp_Pnt origin(ox, oy, oz);
        BRepPrimAPI_MakeBox builder(origin, dx, dy, dz);
        builder.Build();
        if (!builder.IsDone()) {
            return 0;
        }
        return store_shape(builder.Shape());
    } catch (const Standard_Failure&) {
        return 0;
    } catch (...) {
        return 0;
    }
}

uint64_t occt_bridge_make_cylinder(
    double diameter,
    double height,
    double ox,
    double oy,
    double oz) {
    try {
        gp_Ax2 axis(gp_Pnt(ox, oy, oz), gp_Dir(0.0, 0.0, 1.0));
        BRepPrimAPI_MakeCylinder builder(axis, diameter * 0.5, height);
        builder.Build();
        if (!builder.IsDone()) {
            return 0;
        }
        return store_shape(builder.Shape());
    } catch (const Standard_Failure&) {
        return 0;
    } catch (...) {
        return 0;
    }
}

uint64_t occt_bridge_boolean_cut(uint64_t base_handle, uint64_t tool_handle) {
    try {
        TopoDS_Shape base, tool;
        if (!get_shape(base_handle, base) || !get_shape(tool_handle, tool)) {
            return 0;
        }
        BRepAlgoAPI_Cut cut(base, tool);
        cut.Build();
        if (!cut.IsDone()) {
            return 0;
        }
        return store_shape(cut.Shape());
    } catch (const Standard_Failure&) {
        return 0;
    } catch (...) {
        return 0;
    }
}

int32_t occt_bridge_check(uint64_t handle, OcctBridgeMetrics* out_metrics) {
    if (!out_metrics) {
        return -1;
    }
    try {
        TopoDS_Shape shape;
        if (!get_shape(handle, shape)) {
            return -2;
        }

        BRepCheck_Analyzer analyzer(shape, Standard_True);
        out_metrics->watertight = analyzer.IsValid() ? 1 : 0;

        GProp_GProps vol_props;
        BRepGProp::VolumeProperties(shape, vol_props);
        out_metrics->volume_mm3 = vol_props.Mass();

        Bnd_Box box;
        BRepBndLib::AddOptimal(shape, box);
        if (!box.IsVoid()) {
            double xmin, ymin, zmin, xmax, ymax, zmax;
            box.Get(xmin, ymin, zmin, xmax, ymax, zmax);
            out_metrics->bbox.min[0] = xmin;
            out_metrics->bbox.min[1] = ymin;
            out_metrics->bbox.min[2] = zmin;
            out_metrics->bbox.max[0] = xmax;
            out_metrics->bbox.max[1] = ymax;
            out_metrics->bbox.max[2] = zmax;
        }

        BRepMesh_IncrementalMesh mesher(shape, 0.05, Standard_False, 0.25, Standard_True);
        (void)mesher;

        out_metrics->face_count = count_subshapes(shape, TopAbs_FACE);
        out_metrics->edge_count = count_subshapes(shape, TopAbs_EDGE);
        return 0;
    } catch (const Standard_Failure&) {
        return -3;
    } catch (...) {
        return -4;
    }
}

int32_t occt_bridge_export_step(uint64_t handle, const char* path_utf8) {
    if (!path_utf8) {
        return -1;
    }
    try {
        TopoDS_Shape shape;
        if (!get_shape(handle, shape)) {
            return -2;
        }

        Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
        Handle(XCAFDoc_ShapeTool) shape_tool = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        shape_tool->AddShape(shape);

        STEPCAFControl_Writer writer;
        writer.SetNameMode(Standard_True);
        if (!writer.Transfer(doc, STEPControl_AsIs)) {
            return -3;
        }
        const IFSelect_ReturnStatus status = writer.Write(path_utf8);
        return (status == IFSelect_RetDone) ? 0 : -4;
    } catch (const Standard_Failure&) {
        return -5;
    } catch (...) {
        return -6;
    }
}

void occt_bridge_free(uint64_t handle) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_shapes.erase(handle);
}

} // extern "C"
