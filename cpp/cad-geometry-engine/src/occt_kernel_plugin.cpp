#include "occt_kernel_plugin.hpp"
#include "reference_analytic_kernel.hpp"
#include "worker_pool.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <sstream>
#include <string>
#include <thread>
#include <utility>

#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
#  include <BRepAlgoAPI_Common.hxx>
#  include <BRepAlgoAPI_Cut.hxx>
#  include <BRepAlgoAPI_Fuse.hxx>
#  include <BRepBndLib.hxx>
#  include <BRepGProp.hxx>
#  include <BRepMesh_IncrementalMesh.hxx>
#  include <BRepPrimAPI_MakeBox.hxx>
#  include <BRepPrimAPI_MakeCylinder.hxx>
#  include <BRepTools.hxx>
#  include <BRep_Builder.hxx>
#  include <BRep_Tool.hxx>
#  include <Bnd_Box.hxx>
#  include <GProp_GProps.hxx>
#  include <Interface_Static.hxx>
#  include <Poly_Triangulation.hxx>
#  include <STEPControl_Reader.hxx>
#  include <STEPControl_Writer.hxx>
#  include <Standard_Failure.hxx>
#  include <Standard_Version.hxx>
#  include <TopAbs_Orientation.hxx>
#  include <TopExp_Explorer.hxx>
#  include <TopLoc_Location.hxx>
#  include <TopoDS.hxx>
#  include <TopoDS_Face.hxx>
#  include <TopoDS_Shape.hxx>
#  include <gp_Ax2.hxx>
#  include <gp_Dir.hxx>
#  include <gp_Mat.hxx>
#  include <gp_Pnt.hxx>
#  include <gp_Trsf.hxx>
#endif

namespace cad_geomd {

namespace {

uint32_t resolve_worker_count(uint32_t requested) noexcept {
    if (requested != 0u) {
        return requested;
    }
    const unsigned int hw = std::thread::hardware_concurrency();
    return std::clamp(hw == 0u ? 4u : static_cast<uint32_t>(hw), 1u, 16u);
}

#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
bool is_finite_positive(double v) noexcept {
    return std::isfinite(v) && v > 0.0;
}

bool is_finite_vec3(const Vec3d& v) noexcept {
    return std::isfinite(v.x) && std::isfinite(v.y) && std::isfinite(v.z);
}
#else
Status occt_unavailable_status(std::string_view op_name) {
    return Status::Error(
        ErrorCode::KernelUnavailable,
        std::string(op_name) +
            ": OCCT kernel plugin is not compiled in this build; rebuild with -DCAD_GEOMD_WITH_OCCT=ON");
}
#endif

} // namespace

bool is_occt_plugin_compiled() noexcept {
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    return true;
#else
    return false;
#endif
}

std::string_view kernel_backend_kind_name(KernelBackendKind kind) noexcept {
    switch (kind) {
        case KernelBackendKind::Auto:
            return "auto";
        case KernelBackendKind::Analytic:
            return "analytic";
        case KernelBackendKind::Occt:
            return "occt";
    }
    return "unknown";
}

Status parse_kernel_backend_kind(std::string_view name, KernelBackendKind& out_kind) {
    std::string lower;
    lower.reserve(name.size());
    for (const char ch : name) {
        lower.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(ch))));
    }
    if (lower == "auto") {
        out_kind = KernelBackendKind::Auto;
        return Status::Ok();
    }
    if (lower == "analytic" || lower == "reference-analytic") {
        out_kind = KernelBackendKind::Analytic;
        return Status::Ok();
    }
    if (lower == "occt" || lower == "opencascade") {
        out_kind = KernelBackendKind::Occt;
        return Status::Ok();
    }
    return Status::Error(
        ErrorCode::InvalidArgument,
        "unknown kernel backend '" + std::string(name) + "' (expected 'auto', 'analytic', or 'occt')");
}

struct OcctKernelPlugin::Impl {
    uint32_t max_workers = 1;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    ShardedHandleTable<TopoDS_Shape, 64> shapes;
#endif
};

OcctKernelPlugin::OcctKernelPlugin(uint32_t max_workers)
    : impl_(std::make_unique<Impl>()) {
    impl_->max_workers = resolve_worker_count(max_workers);
}

OcctKernelPlugin::~OcctKernelPlugin() = default;
OcctKernelPlugin::OcctKernelPlugin(OcctKernelPlugin&&) noexcept = default;
OcctKernelPlugin& OcctKernelPlugin::operator=(OcctKernelPlugin&&) noexcept = default;

KernelCapabilities OcctKernelPlugin::capabilities() const {
    KernelCapabilities caps{};
    caps.abi_major = CAD_GEOMD_ABI_MAJOR;
    caps.abi_minor = CAD_GEOMD_ABI_MINOR;
    caps.max_worker_threads = impl_->max_workers;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    caps.kernel_name = "occt";
    caps.kernel_version = OCC_VERSION_COMPLETE;
    caps.authoritative_brep = true;
    caps.supports_parallel_tessellation = true;
    caps.supports_step_ap242 = true;
    caps.supports_exact_booleans = true;
    caps.supports_two_stage_assembly = true;
#else
    caps.kernel_name = "occt-unavailable";
    caps.kernel_version = "0.0.0-stub";
    caps.authoritative_brep = false;
    caps.supports_parallel_tessellation = false;
    caps.supports_step_ap242 = false;
    caps.supports_exact_booleans = false;
    caps.supports_two_stage_assembly = false;
#endif
    return caps;
}

Status OcctKernelPlugin::make_box(
    double dx_mm,
    double dy_mm,
    double dz_mm,
    const Vec3d& origin_mm,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    if (!is_finite_positive(dx_mm) || !is_finite_positive(dy_mm) || !is_finite_positive(dz_mm) ||
        !is_finite_vec3(origin_mm)) {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "make_box requires finite positive dimensions (dx, dy, dz) and finite origin");
    }
    try {
        const gp_Pnt corner(origin_mm.x, origin_mm.y, origin_mm.z);
        BRepPrimAPI_MakeBox maker(corner, dx_mm, dy_mm, dz_mm);
        maker.Build();
        if (!maker.IsDone()) {
            return Status::Error(ErrorCode::GeometryFailure, "OCCT BRepPrimAPI_MakeBox failed");
        }
        out_handle = impl_->shapes.insert(maker.Shape());
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT make_box exception: ") + (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)dx_mm;
    (void)dy_mm;
    (void)dz_mm;
    (void)origin_mm;
    return occt_unavailable_status("make_box");
#endif
}

Status OcctKernelPlugin::make_cylinder(
    double diameter_mm,
    double height_mm,
    const Vec3d& origin_mm,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    if (!is_finite_positive(diameter_mm) || !is_finite_positive(height_mm) ||
        !is_finite_vec3(origin_mm)) {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "make_cylinder requires finite positive diameter/height and finite origin");
    }
    try {
        const gp_Ax2 axis(gp_Pnt(origin_mm.x, origin_mm.y, origin_mm.z), gp_Dir(0.0, 0.0, 1.0));
        BRepPrimAPI_MakeCylinder maker(axis, 0.5 * diameter_mm, height_mm);
        maker.Build();
        if (!maker.IsDone()) {
            return Status::Error(ErrorCode::GeometryFailure, "OCCT BRepPrimAPI_MakeCylinder failed");
        }
        out_handle = impl_->shapes.insert(maker.Shape());
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT make_cylinder exception: ") + (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)diameter_mm;
    (void)height_mm;
    (void)origin_mm;
    return occt_unavailable_status("make_cylinder");
#endif
}

Status OcctKernelPlugin::boolean_op(
    ShapeHandle base_handle,
    ShapeHandle tool_handle,
    BooleanOpKind op,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    TopoDS_Shape base_shape;
    if (!impl_->shapes.find_copy(base_handle, base_shape)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown base shape handle");
    }
    TopoDS_Shape tool_shape;
    if (!impl_->shapes.find_copy(tool_handle, tool_shape)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown tool shape handle");
    }
    try {
        TopoDS_Shape result;
        if (op == BooleanOpKind::Union) {
            BRepAlgoAPI_Fuse algo(base_shape, tool_shape);
            algo.Build();
            if (!algo.IsDone()) {
                return Status::Error(ErrorCode::GeometryFailure, "OCCT boolean union failed");
            }
            result = algo.Shape();
        } else if (op == BooleanOpKind::Cut) {
            BRepAlgoAPI_Cut algo(base_shape, tool_shape);
            algo.Build();
            if (!algo.IsDone()) {
                return Status::Error(ErrorCode::GeometryFailure, "OCCT boolean cut failed");
            }
            result = algo.Shape();
        } else if (op == BooleanOpKind::Intersect) {
            BRepAlgoAPI_Common algo(base_shape, tool_shape);
            algo.Build();
            if (!algo.IsDone()) {
                return Status::Error(ErrorCode::GeometryFailure, "OCCT boolean intersect failed");
            }
            result = algo.Shape();
        } else {
            return Status::Error(ErrorCode::InvalidArgument, "unsupported boolean operation kind");
        }
        if (result.IsNull()) {
            return Status::Error(ErrorCode::GeometryFailure, "OCCT boolean produced null shape");
        }
        out_handle = impl_->shapes.insert(std::move(result));
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT boolean_op exception: ") + (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)base_handle;
    (void)tool_handle;
    (void)op;
    return occt_unavailable_status("boolean_op");
#endif
}

Status OcctKernelPlugin::release_shape(ShapeHandle handle) {
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    if (!impl_->shapes.erase(handle)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    return Status::Ok();
#else
    (void)handle;
    return occt_unavailable_status("release_shape");
#endif
}

Status OcctKernelPlugin::compute_mass_properties(
    ShapeHandle handle,
    MassProperties& out_props) const {
    out_props = MassProperties{};
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    TopoDS_Shape shape;
    if (!impl_->shapes.find_copy(handle, shape)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    try {
        GProp_GProps vol_props;
        BRepGProp::VolumeProperties(shape, vol_props);
        GProp_GProps surf_props;
        BRepGProp::SurfaceProperties(shape, surf_props);

        out_props.volume_mm3 = vol_props.Mass();
        out_props.surface_area_mm2 = surf_props.Mass();
        const gp_Pnt cog = vol_props.CentreOfMass();
        out_props.centroid_mm = {cog.X(), cog.Y(), cog.Z()};

        const gp_Mat inertia = vol_props.MatrixOfInertia();
        out_props.inertia_tensor_unit_density = {
            inertia.Value(1, 1),
            inertia.Value(2, 2),
            inertia.Value(3, 3),
            inertia.Value(1, 2),
            inertia.Value(1, 3),
            inertia.Value(2, 3),
        };

        Bnd_Box box;
        BRepBndLib::Add(shape, box);
        if (!box.IsVoid()) {
            double xmin = 0.0, ymin = 0.0, zmin = 0.0, xmax = 0.0, ymax = 0.0, zmax = 0.0;
            box.Get(xmin, ymin, zmin, xmax, ymax, zmax);
            out_props.bbox.min_mm = {xmin, ymin, zmin};
            out_props.bbox.max_mm = {xmax, ymax, zmax};
        }

        uint32_t faces = 0;
        for (TopExp_Explorer exp(shape, TopAbs_FACE); exp.More(); exp.Next()) {
            ++faces;
        }
        uint32_t edges = 0;
        for (TopExp_Explorer exp(shape, TopAbs_EDGE); exp.More(); exp.Next()) {
            ++edges;
        }
        out_props.face_count = faces;
        out_props.edge_count = edges;
        out_props.watertight = out_props.volume_mm3 > 0.0;
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT compute_mass_properties exception: ") +
                (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)handle;
    return occt_unavailable_status("compute_mass_properties");
#endif
}

Status OcctKernelPlugin::tessellate(
    ShapeHandle handle,
    const TessellationParams& params,
    TriangleMesh& out_mesh) const {
    out_mesh = TriangleMesh{};
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    TopoDS_Shape shape;
    if (!impl_->shapes.find_copy(handle, shape)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    try {
        const double lin_def = params.linear_deflection_mm > 0.0 ? params.linear_deflection_mm : 0.05;
        const double ang_def = params.angular_deflection_rad > 0.0 ? params.angular_deflection_rad : 0.2;
        BRepMesh_IncrementalMesh mesher(shape, lin_def, Standard_False, ang_def, Standard_True);
        mesher.Perform();

        out_mesh.source_handle = handle;
        uint32_t face_count = 0;
        for (TopExp_Explorer exp(shape, TopAbs_FACE); exp.More(); exp.Next()) {
            ++face_count;
            const TopoDS_Face face = TopoDS::Face(exp.Current());
            TopLoc_Location loc;
            const Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(face, loc);
            if (tri.IsNull()) {
                continue;
            }
            const gp_Trsf trsf = loc.Transformation();
            const uint32_t base_vertex = out_mesh.vertex_count();
            const int nb_nodes = tri->NbNodes();
            for (int i = 1; i <= nb_nodes; ++i) {
                gp_Pnt p = tri->Node(i).Transformed(trsf);
                out_mesh.positions_mm.push_back(p.X());
                out_mesh.positions_mm.push_back(p.Y());
                out_mesh.positions_mm.push_back(p.Z());
                out_mesh.normals.push_back(0.0);
                out_mesh.normals.push_back(0.0);
                out_mesh.normals.push_back(1.0);
            }
            const bool reversed = (face.Orientation() == TopAbs_REVERSED);
            const int nb_tris = tri->NbTriangles();
            for (int i = 1; i <= nb_tris; ++i) {
                int n1 = 0, n2 = 0, n3 = 0;
                tri->Triangle(i).Get(n1, n2, n3);
                const uint32_t i0 = base_vertex + static_cast<uint32_t>(n1 - 1);
                const uint32_t i1 = base_vertex + static_cast<uint32_t>(n2 - 1);
                const uint32_t i2 = base_vertex + static_cast<uint32_t>(n3 - 1);
                if (reversed) {
                    out_mesh.indices.push_back(i0);
                    out_mesh.indices.push_back(i2);
                    out_mesh.indices.push_back(i1);
                } else {
                    out_mesh.indices.push_back(i0);
                    out_mesh.indices.push_back(i1);
                    out_mesh.indices.push_back(i2);
                }
            }
        }
        out_mesh.face_count = face_count;
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT tessellate exception: ") + (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)handle;
    (void)params;
    return occt_unavailable_status("tessellate");
#endif
}

Status OcctKernelPlugin::tessellate_parallel(
    std::span<const ShapeHandle> handles,
    const TessellationParams& params,
    WorkerPool& pool,
    std::vector<TriangleMesh>& out_meshes) const {
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    return pool.tessellate_batch(*this, handles, params, out_meshes, nullptr);
#else
    (void)handles;
    (void)params;
    (void)pool;
    out_meshes.clear();
    return occt_unavailable_status("tessellate_parallel");
#endif
}

Status OcctKernelPlugin::export_step_ap242(
    ShapeHandle handle,
    std::string_view path_utf8,
    std::string_view product_name_utf8) const {
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    TopoDS_Shape shape;
    if (!impl_->shapes.find_copy(handle, shape)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    if (path_utf8.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "export path must not be empty");
    }
    try {
        Interface_Static::SetCVal("write.step.schema", "AP242DIS");
        if (!product_name_utf8.empty()) {
            const std::string prod(product_name_utf8);
            Interface_Static::SetCVal("write.step.product.name", prod.c_str());
        }
        STEPControl_Writer writer;
        if (writer.Transfer(shape, STEPControl_AsIs) != IFSelect_RetDone) {
            return Status::Error(ErrorCode::GeometryFailure, "OCCT STEP transfer failed");
        }
        const std::string path(path_utf8);
        if (writer.Write(path.c_str()) != IFSelect_RetDone) {
            return Status::Error(ErrorCode::IoError, "OCCT STEP write failed");
        }
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT export_step_ap242 exception: ") +
                (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)handle;
    (void)path_utf8;
    (void)product_name_utf8;
    return occt_unavailable_status("export_step_ap242");
#endif
}

Status OcctKernelPlugin::import_step_ap242(
    std::string_view path_utf8,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    if (path_utf8.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "import path must not be empty");
    }
    try {
        STEPControl_Reader reader;
        const std::string path(path_utf8);
        if (reader.ReadFile(path.c_str()) != IFSelect_RetDone) {
            return Status::Error(ErrorCode::IoError, "OCCT STEP read failed");
        }
        reader.TransferRoots();
        TopoDS_Shape shape = reader.OneShape();
        if (shape.IsNull()) {
            return Status::Error(ErrorCode::GeometryFailure, "OCCT STEP import produced null shape");
        }
        out_handle = impl_->shapes.insert(std::move(shape));
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT import_step_ap242 exception: ") +
                (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)path_utf8;
    return occt_unavailable_status("import_step_ap242");
#endif
}

bool OcctKernelPlugin::serialize_native_brep(
    ShapeHandle handle,
    std::vector<uint8_t>& out_bytes) const {
    out_bytes.clear();
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    TopoDS_Shape shape;
    if (!impl_->shapes.find_copy(handle, shape)) {
        return false;
    }
    try {
        std::ostringstream oss(std::ios::binary);
        BRepTools::Write(shape, oss);
        const std::string text = oss.str();
        out_bytes.assign(text.begin(), text.end());
        return !out_bytes.empty();
    } catch (const Standard_Failure&) {
        return false;
    }
#else
    (void)handle;
    return false;
#endif
}

Status OcctKernelPlugin::deserialize_native_brep(
    std::span<const uint8_t> bytes,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    if (bytes.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "empty native brep buffer");
    }
    try {
        const std::string text(reinterpret_cast<const char*>(bytes.data()), bytes.size());
        std::istringstream iss(text, std::ios::binary);
        TopoDS_Shape shape;
        BRep_Builder builder;
        BRepTools::Read(shape, iss, builder);
        if (shape.IsNull()) {
            return Status::Error(ErrorCode::InvalidArgument, "failed to deserialize OCCT native brep");
        }
        out_handle = impl_->shapes.insert(std::move(shape));
        return Status::Ok();
    } catch (const Standard_Failure& e) {
        return Status::Error(
            ErrorCode::GeometryFailure,
            std::string("OCCT deserialize_native_brep exception: ") +
                (e.GetMessageString() ? e.GetMessageString() : "unknown"));
    }
#else
    (void)bytes;
    return occt_unavailable_status("deserialize_native_brep");
#endif
}

std::size_t OcctKernelPlugin::live_shape_count() const {
#if defined(CAD_GEOMD_HAS_OCCT) && CAD_GEOMD_HAS_OCCT
    return impl_->shapes.size();
#else
    return 0u;
#endif
}

std::unique_ptr<IBRepKernel> create_kernel_backend(
    KernelBackendKind kind,
    uint32_t max_workers) {
    switch (kind) {
        case KernelBackendKind::Analytic:
            return std::make_unique<ReferenceAnalyticKernel>(max_workers);
        case KernelBackendKind::Occt:
            return std::make_unique<OcctKernelPlugin>(max_workers);
        case KernelBackendKind::Auto:
            if (is_occt_plugin_compiled()) {
                return std::make_unique<OcctKernelPlugin>(max_workers);
            }
            return std::make_unique<ReferenceAnalyticKernel>(max_workers);
    }
    return std::make_unique<ReferenceAnalyticKernel>(max_workers);
}

} // namespace cad_geomd
