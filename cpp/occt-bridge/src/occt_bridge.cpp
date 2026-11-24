#include "occt_bridge.h"

#if !defined(AI_NATIVE_CAD_HAS_OCCT) && !defined(AI_NATIVE_CAD_NO_OCCT)
#  if defined(__has_include)
#    if __has_include(<TopoDS_Shape.hxx>)
#      define AI_NATIVE_CAD_HAS_OCCT 1
#    endif
#  endif
#endif

#if defined(AI_NATIVE_CAD_HAS_OCCT)

#include "occt_hlr.h"
#include "occt_topology.h"

#include <BRepAdaptor_Curve.hxx>
#include <BRepAdaptor_Surface.hxx>
#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBndLib.hxx>
#include <BRep_Builder.hxx>
#include <BRep_Tool.hxx>
#include <BRepBuilderAPI_Copy.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_Sewing.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepCheck_Analyzer.hxx>
#include <BRepCheck_Shell.hxx>
#include <BRepExtrema_DistShapeShape.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepGProp.hxx>
#include <BRepGProp_Face.hxx>
#include <BRepLib.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepOffset_MakeOffset.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <BRepTools.hxx>
#include <Bnd_Box.hxx>
#include <GeomAbs_Shape.hxx>
#include <GProp_GProps.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <IGESCAFControl_Reader.hxx>
#include <IGESData_GlobalSection.hxx>
#include <IGESData_IGESModel.hxx>
#include <IGESData_IGESEntity.hxx>
#include <Interface_Static.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <NCollection_IndexedDataMap.hxx>
#include <NCollection_List.hxx>
#include <Poly_Triangulation.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <STEPCAFControl_Writer.hxx>
#include <STEPConstruct_ExternRefs.hxx>
#include <ShapeFix_Shape.hxx>
#include <ShapeUpgrade_UnifySameDomain.hxx>
#include <Standard_Failure.hxx>
#include <TCollection_ExtendedString.hxx>
#include <TCollection_HAsciiString.hxx>
#include <TColStd_SequenceOfAsciiString.hxx>
#include <TDataStd_Name.hxx>
#include <TDF_LabelSequence.hxx>
#include <TDocStd_Document.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopLoc_Location.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Iterator.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopTools_ShapeMapHasher.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Edge.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Shape.hxx>
#include <TopoDS_Solid.hxx>
#include <TopoDS_Wire.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <gp_Ax1.hxx>
#include <gp_Ax2.hxx>
#include <gp_Cone.hxx>
#include <gp_Cylinder.hxx>
#include <gp_Dir.hxx>
#include <gp_Pln.hxx>
#include <gp_Pnt.hxx>
#include <gp_Sphere.hxx>
#include <gp_Torus.hxx>
#include <gp_Trsf.hxx>
#include <gp_Vec.hxx>
#include <gp_XYZ.hxx>

#endif

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cmath>
#include <cstring>
#include <exception>
#include <filesystem>
#include <fstream>
#include <limits>
#include <memory>
#include <mutex>
#include <numbers>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

namespace {

std::mutex g_registry_mutex;
std::atomic<uint64_t> g_next_handle{1};
thread_local OcctBridgeLastError g_last_error{};

void copy_utf8(char* out, size_t capacity, std::string_view text) noexcept {
    if (!out || capacity == 0) {
        return;
    }
    size_t count = std::min(text.size(), capacity - 1);
    if (count < text.size()) {
        while (count > 0 && (static_cast<unsigned char>(text[count]) & 0xc0) == 0x80) {
            --count;
        }
    }
    if (count != 0) {
        std::memcpy(out, text.data(), count);
    }
    out[count] = '\0';
}

class BridgeError {
public:
    explicit BridgeError(const char* operation) noexcept : operation_(operation) {
        g_last_error = {};
    }

    int32_t fail(int32_t status, std::string_view message) const noexcept {
        g_last_error.status_code = status;
        copy_utf8(g_last_error.operation, sizeof g_last_error.operation, operation_);
        copy_utf8(g_last_error.message, sizeof g_last_error.message, message);
        return status;
    }

    uint64_t no_handle(int32_t status, std::string_view message) const noexcept {
        fail(status, message);
        return 0;
    }

    uint64_t handle_result(uint64_t handle, std::string_view message) const noexcept {
        return handle != 0 ? handle : no_handle(-3, message);
    }

    // Call only from a catch handler. Some legacy functions distinguish OCCT
    // exceptions from other exceptions in their return codes.
    int32_t exception(int32_t status, [[maybe_unused]] int32_t occt_status) const noexcept {
        try {
            throw;
#if defined(AI_NATIVE_CAD_HAS_OCCT)
        } catch (const Standard_Failure& failure) {
            const char* message = failure.GetMessageString();
            return fail(occt_status, message && *message ? message : "OCCT raised an exception");
#endif
        } catch (const std::exception& failure) {
            return fail(status, failure.what());
        } catch (...) {
            return fail(status, "Unknown C++ exception");
        }
    }

    int32_t exception(int32_t status) const noexcept { return exception(status, status); }

    uint64_t handle_exception() const noexcept {
        exception(-3);
        return 0;
    }

private:
    const char* operation_;
};

bool is_valid_polygon_params(
    int plane_code,
    const double* xy_pairs,
    int point_count,
    double height,
    double* out_signed_area = nullptr) {
    if (plane_code < 0 || plane_code > 2) {
        return false;
    }
    if (!xy_pairs || point_count < 3) {
        return false;
    }
    if (!std::isfinite(height) || std::abs(height) <= 1.0e-9) {
        return false;
    }
    double double_area = 0.0;
    for (size_t i = 0, count = static_cast<size_t>(point_count); i < count; ++i) {
        const double x0 = xy_pairs[2 * i];
        const double y0 = xy_pairs[2 * i + 1];
        const size_t next = (i + 1) % count;
        const double x1 = xy_pairs[2 * next];
        const double y1 = xy_pairs[2 * next + 1];
        if (!std::isfinite(x0) || !std::isfinite(y0)) {
            return false;
        }
        double_area += (x0 * y1 - x1 * y0);
    }
    if (!std::isfinite(double_area) || std::abs(double_area) <= 1.0e-9) {
        return false;
    }
    if (out_signed_area) {
        *out_signed_area = 0.5 * double_area;
    }
    return true;
}

#if defined(AI_NATIVE_CAD_HAS_OCCT)

using BRepOffsetAPI_MakeThickSolidByJoin = BRepOffsetAPI_MakeThickSolid;

constexpr double kDefaultLinearDeflection = 0.05;
constexpr double kDefaultAngularDeflection = 0.2;
constexpr double kDefaultSewingTolerance = 1.0e-3;

struct MeshOptions {
    double linear = kDefaultLinearDeflection;
    double angular = kDefaultAngularDeflection;
};

struct Component {
    OcctComponentDescriptor descriptor{};
    TopoDS_Shape local_shape;
    TopLoc_Location world;
};

struct ShapeRecord {
    TopoDS_Shape shape;
    MeshOptions mesh_defaults;
    std::vector<Component> components;
    int32_t imported_watertight;
};

struct MeshSnapshot {
    uint64_t handle = 0;
    MeshOptions options;
    OcctTessellationCounts counts{};
    std::vector<OcctFaceDescriptor> faces;
    std::vector<double> positions;
    std::vector<double> normals;
    std::vector<uint32_t> indices;
};

// OCCT algorithms and exchange parameters may mutate shared kernel state.
// Keep those operations separate from the registry lock and from buffer fills.
std::mutex g_occt_mutex;
std::unordered_map<uint64_t, std::shared_ptr<const ShapeRecord>> g_shapes;
// The C ABI has no snapshot token. Pin one query per caller thread so another
// thread can request a different deflection without invalidating its buffers.
thread_local MeshSnapshot g_mesh_query;

uint64_t store_shape(TopoDS_Shape shape, MeshOptions mesh_defaults = {},
                     std::vector<Component> components = {}, int32_t imported_watertight = -1) {
    if (shape.IsNull()) {
        return 0;
    }
    auto record = std::make_shared<const ShapeRecord>(
        ShapeRecord{std::move(shape), mesh_defaults, std::move(components), imported_watertight});
    const uint64_t handle = g_next_handle.fetch_add(1, std::memory_order_relaxed);
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_shapes.emplace(handle, std::move(record));
    return handle;
}

std::shared_ptr<const ShapeRecord> get_record(uint64_t handle) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    const auto it = g_shapes.find(handle);
    return it == g_shapes.end() ? nullptr : it->second;
}

bool get_shape(uint64_t handle, TopoDS_Shape& out) {
    const auto record = get_record(handle);
    if (!record) {
        return false;
    }
    out = record->shape;
    return true;
}

void free_shape_locked(uint64_t handle) {
    if (g_mesh_query.handle == handle) {
        g_mesh_query = {};
    }
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_shapes.erase(handle);
}

int32_t count_subshapes(const TopoDS_Shape& shape, TopAbs_ShapeEnum kind) {
    TopTools_IndexedMapOfShape subshapes;
    TopExp::MapShapes(shape, kind, subshapes);
    return subshapes.Extent();
}

gp_Pnt map_plane_point(int plane_code, double u, double v, double offset) {
    switch (plane_code) {
        case 1: // XZ plane
            return gp_Pnt(u, offset, v);
        case 2: // YZ plane
            return gp_Pnt(offset, u, v);
        case 0: // XY plane
        default:
            return gp_Pnt(u, v, offset);
    }
}

gp_Vec map_plane_extrude_vec(int plane_code, double height) {
    switch (plane_code) {
        case 1: // XZ plane -> extrude along Y
            return gp_Vec(0.0, height, 0.0);
        case 2: // YZ plane -> extrude along X
            return gp_Vec(height, 0.0, 0.0);
        case 0: // XY plane -> extrude along Z
        default:
            return gp_Vec(0.0, 0.0, height);
    }
}

void copy_xyz(const gp_XYZ& xyz, double* out) {
    out[0] = xyz.X();
    out[1] = xyz.Y();
    out[2] = xyz.Z();
}

std::string lower_ascii(std::string_view text) {
    std::string result(text);
    std::transform(result.begin(), result.end(), result.begin(), [](unsigned char c) {
        return static_cast<char>(std::tolower(c));
    });
    return result;
}

std::filesystem::path utf8_path(const char* path) {
    return std::filesystem::path(reinterpret_cast<const char8_t*>(path));
}

std::string_view normalize_length_unit(std::string_view name) {
    const std::string unit = lower_ascii(name);
    if (unit == "mm" || unit == "millimetre" || unit == "millimeter") return "mm";
    if (unit == "cm" || unit == "centimetre" || unit == "centimeter") return "cm";
    if (unit == "m" || unit == "metre" || unit == "meter") return "m";
    if (unit == "km" || unit == "kilometre" || unit == "kilometer") return "km";
    if (unit == "in" || unit == "inch" || unit == "inches") return "in";
    if (unit == "ft" || unit == "foot" || unit == "feet") return "ft";
    if (unit == "mi" || unit == "mile") return "mi";
    if (unit == "um" || unit == "micron" || unit == "micrometre" || unit == "micrometer") return "um";
    if (unit == "mil" || unit == "thou") return "mil";
    if (unit == "uin" || unit == "microinch") return "uin";
    return "unknown";
}

std::string_view step_source_unit(STEPControl_Reader& reader) {
    TColStd_SequenceOfAsciiString length, angle, solid_angle;
    reader.FileUnits(length, angle, solid_angle);
    if (length.IsEmpty()) {
        return "unknown";
    }
    const auto first = normalize_length_unit(length.First().ToCString());
    for (int i = 2; i <= length.Length(); ++i) {
        if (normalize_length_unit(length.Value(i).ToCString()) != first) {
            return "unknown"; // Multiple source length units cannot be represented by one field.
        }
    }
    return first;
}

std::string_view iges_source_unit(IGESCAFControl_Reader& reader) {
    const auto model = reader.IGESModel();
    if (model.IsNull()) return "unknown";
    const auto& global = model->GlobalSection();
    switch (global.UnitFlag()) {
        case 1: return "in";
        case 2: return "mm";
        case 3: {
            const auto name = global.UnitName();
            return name.IsNull() ? "unknown" : normalize_length_unit(name->ToCString());
        }
        case 4: return "ft";
        case 5: return "mi";
        case 6: return "m";
        case 7: return "km";
        case 8: return "mil";
        case 9: return "um";
        case 10: return "cm";
        case 11: return "uin";
        default: return "unknown";
    }
}

bool has_iges_external_references(const Handle(IGESData_IGESModel)& model) {
    if (!model.IsNull()) {
        for (int i = 1; i <= model->NbEntities(); ++i) {
            const auto entity = model->Entity(i);
            if (entity.IsNull()) continue;
            const int type = entity->TypeNumber();
            // Reject the reference entities, their file index, and the file property.
            if (type == 416 || ((type == 402 || type == 406) && entity->FormNumber() == 12)) return true;
        }
    }
    return false;
}

// Exchange translators write progress to OCCT's process-wide default messenger.
// Keep CLI stdout available for its own output.
// This guard is scoped under g_occt_mutex and restores the original printers.
class QuietExchangeTranslation {
public:
    QuietExchangeTranslation() { std::swap(printers_, Message::DefaultMessenger()->ChangePrinters()); }
    ~QuietExchangeTranslation() { std::swap(printers_, Message::DefaultMessenger()->ChangePrinters()); }
    QuietExchangeTranslation(const QuietExchangeTranslation&) = delete;
    QuietExchangeTranslation& operator=(const QuietExchangeTranslation&) = delete;

private:
    NCollection_Sequence<Handle(Message_Printer)> printers_;
};

struct SolidStatus {
    int32_t open_shell_count = 0;
    bool has_solid = false;
    bool all_closed_solids = true;

    void add_open_boundary() {
        if (open_shell_count == std::numeric_limits<int32_t>::max()) {
            throw std::overflow_error("Too many open shells or free faces");
        }
        ++open_shell_count;
    }
};

bool has_manifold_edges(const TopoDS_Shell& shell) {
    TopTools_IndexedMapOfShape edges;
    std::vector<int> uses;
    for (TopExp_Explorer exp(shell, TopAbs_EDGE); exp.More(); exp.Next()) {
        const auto edge = TopoDS::Edge(exp.Current());
        if ((edge.Orientation() != TopAbs_FORWARD && edge.Orientation() != TopAbs_REVERSED) ||
            BRep_Tool::Degenerated(edge)) {
            continue;
        }
        const auto index = static_cast<size_t>(edges.Add(edge)) - 1;
        if (index == uses.size()) uses.push_back(0);
        if (++uses[index] > 2) return false;
    }
    return std::all_of(uses.begin(), uses.end(), [](int count) { return count == 2; });
}

void inspect_topology(const TopoDS_Shape& shape, SolidStatus& status) {
    switch (shape.ShapeType()) {
        case TopAbs_COMPOUND:
        case TopAbs_COMPSOLID:
            for (TopoDS_Iterator it(shape); it.More(); it.Next()) {
                inspect_topology(it.Value(), status);
            }
            return;
        case TopAbs_SOLID: {
            status.has_solid = true;
            bool has_shell = false;
            for (TopExp_Explorer exp(shape, TopAbs_SHELL); exp.More(); exp.Next()) {
                has_shell = true;
                const auto shell = TopoDS::Shell(exp.Current());
                BRepCheck_Shell check(shell);
                if (check.Closed() != BRepCheck_NoError) {
                    status.add_open_boundary();
                    status.all_closed_solids = false;
                } else if (!has_manifold_edges(shell)) {
                    status.all_closed_solids = false;
                }
            }
            status.all_closed_solids &= has_shell;
            return;
        }
        case TopAbs_SHELL: {
            BRepCheck_Shell check(TopoDS::Shell(shape));
            if (check.Closed() != BRepCheck_NoError) {
                status.add_open_boundary();
            }
            break;
        }
        case TopAbs_FACE:
            status.add_open_boundary();
            break;
        default:
            break;
    }
    status.all_closed_solids = false;
}

bool is_watertight(const TopoDS_Shape& shape, int32_t* open_shell_count = nullptr) {
    SolidStatus status;
    inspect_topology(shape, status);
    if (open_shell_count) {
        *open_shell_count = status.open_shell_count;
    }
    return status.has_solid && status.all_closed_solids && BRepCheck_Analyzer(shape).IsValid();
}

bool measure_brep_volume(const TopoDS_Shape& solid, double& volume) {
    GProp_GProps properties;
    BRepGProp::VolumeProperties(solid, properties, /*OnlyClosed=*/true,
                                /*SkipShared=*/false, /*UseTriangulation=*/false);
    volume = properties.Mass();
    return std::isfinite(volume) && volume >= 0.0;
}

bool measure_cut_volume(const TopoDS_Shape& base, const TopoDS_Shape& tool, double& volume) {
    TopTools_ListOfShape arguments, tools;
    arguments.Append(base);
    tools.Append(tool);
    BRepAlgoAPI_Cut cut;
    cut.SetArguments(arguments);
    cut.SetTools(tools);
    cut.SetNonDestructive(Standard_True);
    cut.Build();
    if (!cut.IsDone() || cut.HasErrors() || cut.Shape().IsNull()) return false;

    const auto& result = cut.Shape();
    volume = 0.0;
    // Equal/contained solids can give a valid empty compound, not a solid.
    if (result.ShapeType() == TopAbs_COMPOUND && !TopoDS_Iterator(result).More()) return true;
    if (!is_watertight(result)) return false;

    TopTools_IndexedMapOfShape solids;
    TopExp::MapShapes(result, TopAbs_SOLID, solids);
    for (int index = 1; index <= solids.Extent(); ++index) {
        double mass;
        if (!measure_brep_volume(solids(index), mass) || mass <= 0.0) return false;
        volume += mass;
    }
    return std::isfinite(volume);
}

MeshOptions mesh_options(const OcctImportOptions* opts, MeshOptions defaults = {}) {
    if (opts) {
        const auto option = [](double value, double fallback) {
            if (!std::isfinite(value) || value < 0.0) {
                throw std::invalid_argument("Invalid mesh deflection");
            }
            return value == 0.0 ? fallback : value;
        };
        defaults.linear = option(opts->linear_deflection_mm, defaults.linear);
        defaults.angular = option(opts->angular_deflection_rad, defaults.angular);
    }
    return defaults;
}

TopoDS_Shape combine_shapes(const std::vector<TopoDS_Shape>& shapes) {
    if (shapes.empty()) return {};
    if (shapes.size() == 1) return shapes.front();
    BRep_Builder builder;
    TopoDS_Compound compound;
    builder.MakeCompound(compound);
    for (const auto& shape : shapes) builder.Add(compound, shape);
    return compound;
}

TopoDS_Shape promote_closed_shells(const TopoDS_Shape& shape) {
    if (shape.ShapeType() == TopAbs_COMPOUND) {
        std::vector<TopoDS_Shape> children;
        for (TopoDS_Iterator it(shape); it.More(); it.Next()) {
            children.push_back(promote_closed_shells(it.Value()));
        }
        return combine_shapes(children);
    }
    if (shape.ShapeType() == TopAbs_SHELL) {
        const auto shell = TopoDS::Shell(shape);
        if (BRepCheck_Shell(shell).Closed() == BRepCheck_NoError && has_manifold_edges(shell)) {
            BRep_Builder builder;
            TopoDS_Solid solid;
            builder.MakeSolid(solid);
            builder.Add(solid, shell);
            if (BRepLib::OrientClosedSolid(solid) && BRepCheck_Analyzer(solid).IsValid()) return solid;
        }
    }
    return shape; // Preserve unresolved topology so E505 and preview still work.
}

void partition_surfaces(const TopoDS_Shape& shape, std::vector<TopoDS_Shape>& surfaces,
                        std::vector<TopoDS_Shape>& other) {
    if (shape.ShapeType() == TopAbs_COMPOUND) {
        for (TopoDS_Iterator it(shape); it.More(); it.Next()) {
            partition_surfaces(it.Value(), surfaces, other);
        }
    } else if (shape.ShapeType() == TopAbs_FACE || shape.ShapeType() == TopAbs_SHELL) {
        surfaces.push_back(shape);
    } else {
        other.push_back(shape);
    }
}

TopoDS_Shape prepare_imported_shape(TopoDS_Shape shape, const OcctImportOptions* opts) {
    if (!opts || shape.IsNull()) return shape;
    if (opts->heal_geometry) {
        const double tol = opts->sewing_tol_mm == 0.0 ? kDefaultSewingTolerance : opts->sewing_tol_mm;
        if (!std::isfinite(tol) || tol <= 0.0) {
            throw std::invalid_argument("Invalid sewing tolerance");
        }
        // ShapeFix may update geometry/tolerances in place. Work on a copy so
        // independently placed XCAF occurrences cannot affect each other.
        ShapeFix_Shape fixer(BRepBuilderAPI_Copy(shape).Shape());
        fixer.SetPrecision(tol);
        fixer.SetMaxTolerance(tol);
        fixer.Perform();
        shape = fixer.Shape();
        if (shape.IsNull()) return shape;

        // Sewing returns faces/shells; do not feed already solid boundaries to it.
        std::vector<TopoDS_Shape> surfaces, other;
        partition_surfaces(shape, surfaces, other);
        if (!surfaces.empty()) {
            BRepBuilderAPI_Sewing sewing(tol);
            sewing.SetLocalTolerancesMode(Standard_False);
            sewing.SetMaxTolerance(tol);
            for (const auto& surface : surfaces) sewing.Add(surface);
            sewing.Perform();
            const auto sewed = sewing.SewedShape();
            other.push_back(promote_closed_shells(sewed.IsNull() ? combine_shapes(surfaces) : sewed));
            shape = combine_shapes(other);
        }
    }
    if (opts->unify_same_domain && !shape.IsNull()) {
        ShapeUpgrade_UnifySameDomain unify(shape, Standard_True, Standard_True);
        unify.Build();
        shape = unify.Shape();
    }
    return shape;
}

std::string label_name(const TDF_Label& label) {
    Handle(TDataStd_Name) name;
    if (!label.FindAttribute(TDataStd_Name::GetID(), name)) {
        return {};
    }
    const TCollection_ExtendedString& value = name->Get();
    std::string utf8(static_cast<size_t>(value.LengthOfCString()) + 1, '\0');
    char* dest = utf8.data();
    utf8.resize(static_cast<size_t>(value.ToUTF8CString(dest)));
    return utf8;
}

void add_component(std::vector<Component>& out, TopoDS_Shape shape,
                   const TopLoc_Location& world, std::string_view name) {
    if (shape.IsNull()) {
        return;
    }
    if (out.size() >= static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
        throw std::overflow_error("Too many assembly components");
    }
    Component component;
    auto& descriptor = component.descriptor;
    const std::string id = "comp_" + std::to_string(out.size());
    copy_utf8(descriptor.component_id, sizeof descriptor.component_id, id);
    copy_utf8(descriptor.part_name, sizeof descriptor.part_name, name.empty() ? std::string_view(id) : name);
    const gp_Trsf& transform = world.Transformation();
    for (int column = 0; column < 4; ++column) {
        for (int row = 0; row < 3; ++row) {
            descriptor.transform_col_major[4 * column + row] = transform.Value(row + 1, column + 1);
        }
    }
    descriptor.transform_col_major[15] = 1.0;
    component.local_shape = std::move(shape);
    component.world = world;
    out.push_back(std::move(component));
}

void collect_components(const TDF_Label& label, const TopLoc_Location& parent,
                        const OcctImportOptions* opts, std::vector<Component>& out,
                        unsigned depth = 0, const std::string& instance_name = {}) {
    if (depth >= 256) {
        throw std::runtime_error("Assembly hierarchy is too deep or cyclic");
    }
    TDF_Label referred;
    if (XCAFDoc_ShapeTool::GetReferredShape(label, referred)) {
        collect_components(referred, parent * XCAFDoc_ShapeTool::GetLocation(label),
                           opts, out, depth + 1, label_name(label));
        return;
    }
    const TopoDS_Shape shape = XCAFDoc_ShapeTool::GetShape(label);
    if (shape.IsNull()) {
        return;
    }
    const TopLoc_Location world = parent * shape.Location();
    TDF_LabelSequence children;
    if (XCAFDoc_ShapeTool::GetComponents(label, children)) {
        for (int i = 1; i <= children.Length(); ++i) {
            collect_components(children.Value(i), world, opts, out, depth + 1);
        }
        return;
    }
    const std::string name = label_name(label);
    add_component(out, prepare_imported_shape(shape.Located(TopLoc_Location()), opts),
                  world, name.empty() ? instance_name : name);
}

TopoDS_Shape xde_root(const Handle(TDocStd_Document)& doc, const OcctImportOptions* opts,
                      std::vector<Component>& components) {
    const auto tool = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    TDF_LabelSequence roots;
    tool->GetFreeShapes(roots);
    // Surface-mode exchange files can expose every face as a free XCAF root.
    // Sew those roots together, but never sew across assembly occurrences.
    std::vector<int> loose_indices;
    std::vector<TopoDS_Shape> loose_roots;
    if (opts && opts->heal_geometry) {
        for (int i = 1; i <= roots.Length(); ++i) {
            const auto& label = roots.Value(i);
            if (XCAFDoc_ShapeTool::IsAssembly(label) || XCAFDoc_ShapeTool::IsReference(label)) continue;
            const auto shape = XCAFDoc_ShapeTool::GetShape(label);
            if (shape.IsNull()) continue;
            std::vector<TopoDS_Shape> surfaces, other;
            partition_surfaces(shape, surfaces, other);
            if (!surfaces.empty() && other.empty()) {
                loose_indices.push_back(i);
                loose_roots.push_back(shape);
            }
        }
    }
    for (int i = 1; i <= roots.Length(); ++i) {
        if (loose_indices.size() > 1 && std::binary_search(loose_indices.begin(), loose_indices.end(), i)) {
            if (i == loose_indices.front()) {
                add_component(components, prepare_imported_shape(combine_shapes(loose_roots), opts),
                              TopLoc_Location(), "");
            }
        } else {
            collect_components(roots.Value(i), TopLoc_Location(), opts, components);
        }
    }
    if (components.empty()) {
        return {};
    }
    TopoDS_Compound compound;
    if (!opts || (!opts->heal_geometry && !opts->unify_same_domain)) {
        if (roots.Length() == 1) {
            return XCAFDoc_ShapeTool::GetShape(roots.First());
        }
        BRep_Builder builder;
        builder.MakeCompound(compound);
        for (int i = 1; i <= roots.Length(); ++i) {
            const auto root_shape = XCAFDoc_ShapeTool::GetShape(roots.Value(i));
            if (!root_shape.IsNull()) builder.Add(compound, root_shape);
        }
        return compound;
    }
    // Reassemble processed local geometry using each occurrence's same pose.
    BRep_Builder builder;
    builder.MakeCompound(compound);
    for (const auto& component : components) {
        builder.Add(compound, component.local_shape.Moved(component.world, Standard_False));
    }
    return compound;
}

void describe_face(const TopoDS_Face& face, const BRepAdaptor_Surface& surface,
                   OcctFaceDescriptor& out) {
    GProp_GProps props;
    BRepGProp::SurfaceProperties(face, props);
    out.area_mm2 = props.Mass();
    copy_xyz(props.CentreOfMass().XYZ(), out.origin_or_center);
    const auto set_axis = [&](const gp_Ax1& axis) {
        copy_xyz(axis.Direction().XYZ(), out.normal_or_axis);
        copy_xyz(axis.Location().XYZ(), out.origin_or_center);
    };
    switch (surface.GetType()) {
        case GeomAbs_Plane: {
            out.surface_kind = 0;
            set_axis(surface.Plane().Axis());
            // gp_Pln can have an indirect parameterization.
            gp_Pnt p;
            gp_Vec du, dv;
            surface.D1(0.0, 0.0, p, du, dv);
            gp_Dir normal(du.Crossed(dv));
            if (face.Orientation() == TopAbs_REVERSED) normal.Reverse();
            copy_xyz(normal.XYZ(), out.normal_or_axis);
            break;
        }
        case GeomAbs_Cylinder: {
            out.surface_kind = 1;
            const auto cylinder = surface.Cylinder();
            set_axis(cylinder.Axis());
            out.radius_mm = cylinder.Radius();
            break;
        }
        case GeomAbs_Cone:
            out.surface_kind = 2;
            set_axis(surface.Cone().Axis());
            break;
        case GeomAbs_Sphere: {
            out.surface_kind = 3;
            const auto sphere = surface.Sphere();
            set_axis(sphere.Position().Axis());
            out.radius_mm = sphere.Radius();
            break;
        }
        case GeomAbs_Torus: {
            out.surface_kind = 4;
            const auto torus = surface.Torus();
            set_axis(torus.Axis());
            out.radius_mm = torus.MajorRadius();
            break;
        }
        case GeomAbs_BSplineSurface: out.surface_kind = 5; break;
        default: out.surface_kind = 6; break;
    }
}

void append_triangulation(const TopoDS_Face& face, const BRepAdaptor_Surface& surface,
                          OcctFaceDescriptor& descriptor, MeshSnapshot& out) {
    TopLoc_Location location;
    const auto tri = BRep_Tool::Triangulation(face, location);
    if (tri.IsNull()) {
        return;
    }
    const uint64_t vertices = static_cast<uint64_t>(tri->NbNodes());
    const uint64_t indices = 3ULL * static_cast<uint64_t>(tri->NbTriangles());
    if (vertices + out.counts.total_vertices > std::numeric_limits<uint32_t>::max() ||
        vertices + out.counts.total_vertices > out.positions.max_size() / 3 ||
        indices + out.counts.total_indices > std::numeric_limits<uint32_t>::max()) {
        throw std::overflow_error("Tessellation exceeds buffer index range");
    }
    const auto& transform = location.Transformation();
    const size_t position_offset = out.positions.size();
    out.positions.resize(position_offset + 3 * static_cast<size_t>(vertices));
    out.normals.resize(out.positions.size());
    for (uint32_t i = 0; i < vertices; ++i) {
        const gp_Pnt p = tri->Node(static_cast<int>(i + 1)).Transformed(transform);
        copy_xyz(p.XYZ(), out.positions.data() + position_offset + 3 * static_cast<size_t>(i));
    }

    std::vector<gp_Vec> fallback_normals(static_cast<size_t>(vertices));
    const bool reversed = face.Orientation() == TopAbs_REVERSED;
    const auto point = [&](uint32_t i) {
        const double* xyz = out.positions.data() + position_offset + 3 * static_cast<size_t>(i);
        return gp_Pnt(xyz[0], xyz[1], xyz[2]);
    };
    for (uint64_t i = 0; i < indices / 3; ++i) {
        int a, b, c;
        tri->Triangle(static_cast<int>(i + 1)).Get(a, b, c);
        if (a <= 0 || b <= 0 || c <= 0 || a > tri->NbNodes() || b > tri->NbNodes() || c > tri->NbNodes()) {
            throw std::runtime_error("Invalid tessellation index");
        }
        if (reversed) std::swap(b, c);
        const auto ia = static_cast<uint32_t>(a - 1);
        const auto ib = static_cast<uint32_t>(b - 1);
        const auto ic = static_cast<uint32_t>(c - 1);
        out.indices.insert(out.indices.end(), {ia, ib, ic});
        const gp_Pnt origin = point(ia);
        const gp_Vec normal = gp_Vec(origin, point(ib)).Crossed(gp_Vec(origin, point(ic)));
        fallback_normals[ia] += normal;
        fallback_normals[ib] += normal;
        fallback_normals[ic] += normal;
    }
    // Prefer exact support normals, using the oriented triangle fan at singularities.
    for (uint32_t i = 0; i < vertices; ++i) {
        gp_Vec normal;
        if (descriptor.surface_kind == 0) {
            normal = gp_Vec(descriptor.normal_or_axis[0], descriptor.normal_or_axis[1], descriptor.normal_or_axis[2]);
        } else if (tri->HasUVNodes()) {
            try {
                const auto uv = tri->UVNode(static_cast<int>(i + 1));
                gp_Pnt p;
                gp_Vec du, dv;
                surface.D1(uv.X(), uv.Y(), p, du, dv);
                normal = du.Crossed(dv);
                if (reversed) normal.Reverse();
            } catch (const Standard_Failure&) {
                // Some surfaces cannot give derivatives at a UV node (e.g. a knot).
            }
        }
        double magnitude = normal.Magnitude();
        if (!std::isfinite(magnitude) || magnitude <= 1.0e-15) {
            normal = fallback_normals[i];
            magnitude = normal.Magnitude();
        }
        if (std::isfinite(magnitude) && magnitude > 0.0) {
            copy_xyz(normal.XYZ() / magnitude,
                     out.normals.data() + position_offset + 3 * static_cast<size_t>(i));
        }
    }
    descriptor.tri_vertex_count = static_cast<uint32_t>(vertices);
    descriptor.tri_index_count = static_cast<uint32_t>(indices);
    out.counts.total_vertices += descriptor.tri_vertex_count;
    out.counts.total_indices += descriptor.tri_index_count;
}

MeshSnapshot make_mesh(uint64_t handle, const TopoDS_Shape& shape, MeshOptions opts) {
    MeshSnapshot out;
    out.handle = handle;
    out.options = opts;
    if (!TopExp_Explorer(shape, TopAbs_FACE).More()) {
        return out;
    }
    // Copy topology, not its triangulation: meshing writes to faces and edges.
    BRepBuilderAPI_Copy copy(shape, Standard_True, Standard_False);
    const TopoDS_Shape& scratch = copy.Shape();
    // Copy may keep existing tessellation when a face has no support surface.
    BRepTools::Clean(scratch);
    BRepMesh_IncrementalMesh mesh(scratch, opts.linear, Standard_False, opts.angular, Standard_False);
    if (!mesh.IsDone()) {
        throw std::runtime_error("OCCT meshing failed");
    }
    TopTools_IndexedMapOfShape faces;
    TopExp::MapShapes(scratch, TopAbs_FACE, faces);
    out.faces.reserve(faces.Extent());
    for (int i = 0; i < faces.Extent(); ++i) {
        const TopoDS_Face face = TopoDS::Face(faces(i + 1));
        TopLoc_Location support_location;
        if (BRep_Tool::Surface(face, support_location).IsNull()) {
            throw std::runtime_error("B-Rep face has no geometric support surface");
        }
        const BRepAdaptor_Surface surface(face);
        OcctFaceDescriptor descriptor{};
        descriptor.face_index = out.counts.face_count++;
        descriptor.tri_vertex_offset = out.counts.total_vertices;
        descriptor.tri_index_offset = out.counts.total_indices;
        describe_face(face, surface, descriptor);
        append_triangulation(face, surface, descriptor, out);
        out.faces.push_back(descriptor);
    }
    return out;
}

// The first direct-edit slice accepts a periodic wall with two complete circular
// rims, each an inner wire of a distinct perpendicular exterior planar face.
constexpr double kHoleLengthTol = 1.0e-6;
constexpr double kHoleAngleTol = 1.0e-7;

struct ThroughHole {
    gp_Ax2 axis;
    double height = 0.0;
    double radius = 0.0;
    int caps[2]{};
    TopoDS_Edge rims[2];
};

TopoDS_Shape single_solid(TopoDS_Shape shape) {
    while (!shape.IsNull() && shape.ShapeType() == TopAbs_COMPOUND) {
        TopoDS_Iterator it(shape);
        if (!it.More()) return {};
        const auto child = it.Value();
        it.Next();
        if (it.More()) return {};
        shape = child;
    }
    return !shape.IsNull() && shape.ShapeType() == TopAbs_SOLID ? shape : TopoDS_Shape{};
}

bool read_through_hole(const TopTools_IndexedMapOfShape& faces, int wall_index, ThroughHole& hole) {
    const auto wall = TopoDS::Face(faces(wall_index));
    const BRepAdaptor_Surface surface(wall);
    if (surface.GetType() != GeomAbs_Cylinder ||
        std::abs(surface.LastUParameter() - surface.FirstUParameter() - 2.0 * std::numbers::pi) > kHoleAngleTol ||
        (wall.Orientation() != TopAbs_FORWARD && wall.Orientation() != TopAbs_REVERSED)) return false;
    const auto cylinder = surface.Cylinder();
    const auto axis = cylinder.Axis();
    const auto dir = axis.Direction();
    gp_Pnt mid;
    gp_Vec du, dv;
    surface.D1((surface.FirstUParameter() + surface.LastUParameter()) / 2.0,
               (surface.FirstVParameter() + surface.LastVParameter()) / 2.0, mid, du, dv);
    const auto radial = gp_Vec(axis.Location(), mid);
    const double radial_dot = du.Crossed(dv).Dot(radial);
    if (wall.Orientation() == TopAbs_REVERSED ? radial_dot <= 0.0 : radial_dot >= 0.0) return false;

    TopTools_IndexedMapOfShape wall_edges;
    TopExp::MapShapes(wall, TopAbs_EDGE, wall_edges);
    if (wall_edges.Extent() != 3) return false;
    double axial[2]{};
    int rim_count = 0, seam_count = 0;
    for (int i = 1; i <= wall_edges.Extent(); ++i) {
        const auto edge = TopoDS::Edge(wall_edges(i));
        const BRepAdaptor_Curve curve(edge);
        if (BRep_Tool::IsClosed(edge, wall)) {
            if (curve.GetType() != GeomAbs_Line ||
                !curve.Line().Direction().IsParallel(dir, kHoleAngleTol)) return false;
            ++seam_count;
            continue;
        }
        if (rim_count == 2 || curve.GetType() != GeomAbs_Circle || !BRep_Tool::IsClosed(edge) ||
            std::abs(curve.LastParameter() - curve.FirstParameter() - 2.0 * std::numbers::pi) > kHoleAngleTol) return false;
        const auto ring = curve.Circle();
        const auto shift = gp_Vec(axis.Location(), ring.Location());
        const double t = shift.Dot(gp_Vec(dir));
        if (shift.Crossed(gp_Vec(dir)).Magnitude() > kHoleLengthTol ||
            std::abs(ring.Radius() - cylinder.Radius()) > kHoleLengthTol ||
            !ring.Axis().Direction().IsParallel(dir, kHoleAngleTol)) return false;
        int cap_index = 0;
        for (int f = 1; f <= faces.Extent(); ++f) {
            if (f == wall_index) continue;
            const auto cap = TopoDS::Face(faces(f));
            TopTools_IndexedMapOfShape cap_edges;
            TopExp::MapShapes(cap, TopAbs_EDGE, cap_edges);
            if (!cap_edges.Contains(edge)) continue;
            if (cap_index != 0) return false;
            const BRepAdaptor_Surface support(cap);
            if (support.GetType() != GeomAbs_Plane) return false;
            auto normal = support.Plane().Axis().Direction();
            if (cap.Orientation() == TopAbs_REVERSED) normal.Reverse();
            const double along = normal.Dot(dir);
            if (!normal.IsParallel(dir, kHoleAngleTol)) return false;
            const auto outer = BRepTools::OuterWire(cap);
            bool inner_circle = false;
            for (TopoDS_Iterator it(cap); it.More(); it.Next()) {
                const auto& wire = it.Value();
                if (wire.ShapeType() != TopAbs_WIRE || wire.IsSame(outer)) continue;
                TopoDS_Iterator edges(wire);
                if (edges.More() && edges.Value().IsSame(edge)) {
                    edges.Next();
                    if (!edges.More()) inner_circle = true;
                }
            }
            if (!inner_circle) return false;
            // Keep the opening side for the low/high orientation check below.
            cap_index = along > 0.0 ? f : -f;
        }
        if (cap_index == 0) return false;
        axial[rim_count] = t;
        hole.rims[rim_count] = edge;
        hole.caps[rim_count++] = cap_index;
    }
    if (rim_count != 2 || seam_count != 1) return false;
    if (axial[0] > axial[1]) {
        std::swap(axial[0], axial[1]);
        std::swap(hole.caps[0], hole.caps[1]);
        std::swap(hole.rims[0], hole.rims[1]);
    }
    if (hole.caps[0] >= 0 || hole.caps[1] <= 0 || -hole.caps[0] == hole.caps[1]) return false;
    hole.caps[0] = -hole.caps[0];
    hole.axis = gp_Ax2(axis.Location().Translated(gp_Vec(dir) * axial[0]), dir,
                      cylinder.Position().XDirection());
    hole.height = axial[1] - axial[0];
    hole.radius = cylinder.Radius();
    return hole.height > kHoleLengthTol && hole.radius > kHoleLengthTol;
}

void run_hole_boolean(BRepAlgoAPI_BooleanOperation& op, const TopoDS_Shape& base,
                      const TopoDS_Shape& tool) {
    TopTools_ListOfShape arguments, tools;
    arguments.Append(base);
    tools.Append(tool);
    op.SetArguments(arguments);
    op.SetTools(tools);
    op.SetNonDestructive(Standard_True);
    op.Build();
}

bool finite_xyz(const double* xyz) {
    return xyz && std::all_of(xyz, xyz + 3, [](double value) { return std::isfinite(value); });
}

bool matches_through_hole(const ThroughHole& actual, const ThroughHole& expected,
                          double radius, const int (&caps)[2]) {
    const auto end = expected.axis.Location().Translated(gp_Vec(expected.axis.Direction()) * expected.height);
    const bool same_direction = actual.axis.Direction().Dot(expected.axis.Direction()) > 0.0;
    return std::abs(actual.radius - radius) <= kHoleLengthTol &&
           std::abs(actual.height - expected.height) <= kHoleLengthTol &&
           actual.axis.Direction().IsParallel(expected.axis.Direction(), kHoleAngleTol) &&
           actual.caps[0] == caps[same_direction ? 0 : 1] &&
           actual.caps[1] == caps[same_direction ? 1 : 0] &&
           actual.axis.Location().Distance(same_direction ? expected.axis.Location() : end) <= kHoleLengthTol;
}

// Map result faces back to the operand. Only the two opening faces may be
// replaced by history; all other surviving faces must retain their identity.
int map_hole_faces(const TopTools_IndexedMapOfShape& original,
                   const TopTools_IndexedMapOfShape& updated, int removed_wall,
                   const int (&caps)[2], BRepAlgoAPI_BooleanOperation* history,
                   std::vector<int32_t>& sources, int (&updated_caps)[2]) {
    for (int i = 1; i <= original.Extent(); ++i) {
        if (i == removed_wall) continue;
        int j = updated.FindIndex(original(i));
        const bool cap = i == caps[0] || i == caps[1];
        if (history && cap && j == 0) {
            const auto& modified = history->Modified(original(i));
            if (modified.Extent() == 1) j = updated.FindIndex(modified.First());
        }
        if (j == 0 || sources[j - 1] != -1 || (!cap && !updated(j).IsEqual(original(i)))) return -1;
        sources[j - 1] = i - 1;
        if (cap) updated_caps[i == caps[0] ? 0 : 1] = j;
    }
    const auto unmapped = std::find(sources.begin(), sources.end(), -1);
    if (unmapped == sources.end()) return 0;
    if (std::find(unmapped + 1, sources.end(), -1) != sources.end()) return -1;
    return static_cast<int>(unmapped - sources.begin()) + 1;
}

bool cap_without_rim_matches(const TopoDS_Shape& open, const TopoDS_Edge& rim,
                             const TopoDS_Shape& filled) {
    TopTools_IndexedMapOfShape open_edges, filled_edges;
    TopExp::MapShapes(open, TopAbs_EDGE, open_edges);
    TopExp::MapShapes(filled, TopAbs_EDGE, filled_edges);
    if (!open_edges.Contains(rim) || filled_edges.Contains(rim) ||
        open_edges.Extent() != filled_edges.Extent() + 1) return false;
    for (int i = 1; i <= filled_edges.Extent(); ++i) {
        if (!open_edges.Contains(filled_edges(i))) return false;
    }
    return true;
}

bool hole_volume_delta_matches(double larger, double smaller, double expected) {
    return std::isfinite(expected) &&
           std::abs(larger - smaller - expected) <= std::max(1.0e-6, 1.0e-7 * std::abs(expected));
}

bool cutter_clears_other_boundaries(const TopoDS_Shape& cutter, const TopTools_IndexedMapOfShape& faces,
                                    const int (&caps)[2]) {
    BRep_Builder builder;
    TopoDS_Compound boundaries;
    builder.MakeCompound(boundaries);
    for (int i = 1; i <= faces.Extent(); ++i) {
        if (i == caps[0] || i == caps[1]) {
            for (TopoDS_Iterator it(faces(i)); it.More(); it.Next()) builder.Add(boundaries, it.Value());
        } else {
            builder.Add(boundaries, faces(i));
        }
    }
    BRepExtrema_DistShapeShape distance(cutter, boundaries);
    return distance.IsDone() && !distance.InnerSolution() && distance.Value() > kHoleLengthTol;
}

int32_t boolean_failure(const BridgeError& error, const BRepAlgoAPI_BooleanOperation& boolean,
                        const char* fallback) {
    if (boolean.HasErrors()) {
        std::ostringstream report;
        boolean.DumpErrors(report);
        const std::string text = std::move(report).str();
        if (!text.empty()) return error.fail(-3, text);
    }
    return error.fail(-3, fallback);
}

int32_t relocate_through_hole(const BridgeError& error, uint64_t handle, int32_t face_index, const double* offset,
                              int32_t capacity, int32_t* out_sources, uint64_t* out_handle) {
    if (face_index < 0 || !out_sources || !out_handle) return error.fail(-1, "Invalid face index or missing output buffer");
    const bool move = offset != nullptr;
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-1, "Invalid shape handle");
        const auto base = single_solid(record->shape);
        if (base.IsNull() || !is_watertight(base)) return error.fail(-2, "Expected a single watertight solid");
        TopTools_IndexedMapOfShape original;
        TopExp::MapShapes(base, TopAbs_FACE, original);
        const int count = original.Extent();
        if (face_index >= count || capacity != count - (move ? 0 : 1)) return error.fail(-1, "Invalid face index or capacity");
        const int wall = face_index + 1;
        ThroughHole hole;
        if (!read_through_hole(original, wall, hole)) return error.fail(-2, "Face is not the wall of a supported through hole");

        ThroughHole target = hole;
        if (move) {
            gp_Vec shift(offset[0], offset[1], offset[2]);
            const gp_Vec direction(hole.axis.Direction());
            const double axial = shift.Dot(direction);
            const double length = std::hypot(offset[0], offset[1], offset[2]);
            if (!std::isfinite(length) || !std::isfinite(axial)) return error.fail(-3, "Hole offset is not finite");
            if (std::abs(axial) > std::min(kHoleLengthTol, kHoleAngleTol * length)) return error.fail(-5, "Hole offset must be perpendicular to its axis");
            if (length == 0.0) {
                const uint64_t result = store_shape(base, record->mesh_defaults);
                if (result == 0) return error.fail(-3, "Could not store hole result");
                for (int i = 0; i < count; ++i) out_sources[i] = i;
                *out_handle = result;
                return OCCT_OK;
            }
            if (length <= kHoleLengthTol) return error.fail(-3, "Hole offset is below geometric tolerance");
            // Discard only axial floating-point noise so the openings stay coplanar.
            target.axis.Translate(shift - direction * axial);
        }

        // Fill the exact space bounded by the wall and its two rims. Merge
        // the planar disks into their opening faces, then check the untouched perimeter.
        const auto old_cylinder = BRepPrimAPI_MakeCylinder(hole.axis, hole.radius, hole.height).Shape();
        BRepAlgoAPI_Fuse fuse;
        run_hole_boolean(fuse, base, old_cylinder);
        if (!fuse.IsDone() || fuse.HasErrors()) {
            return boolean_failure(error, fuse, "OCCT could not fill the through hole");
        }
        fuse.SimplifyResult(false, true);
        if (fuse.HasErrors()) {
            return boolean_failure(error, fuse, "OCCT could not simplify the filled solid");
        }
        const auto filled = single_solid(fuse.Shape());
        if (filled.IsNull() || !is_watertight(filled)) return error.fail(-3, "Filled hole did not produce a watertight solid");
        double base_volume, filled_volume;
        const double hole_volume = std::numbers::pi * hole.radius * hole.radius * hole.height;
        if (!measure_brep_volume(base, base_volume) || !measure_brep_volume(filled, filled_volume) ||
            !hole_volume_delta_matches(filled_volume, base_volume, hole_volume)) return error.fail(-3, "Filled hole changed the volume unexpectedly");

        TopTools_IndexedMapOfShape filled_faces;
        TopExp::MapShapes(filled, TopAbs_FACE, filled_faces);
        if (filled_faces.Extent() != count - 1) return error.fail(-3, "Filled hole changed the face count unexpectedly");
        std::vector<int32_t> sources(count - 1, -1);
        int filled_caps[2]{};
        if (map_hole_faces(original, filled_faces, wall, hole.caps, &fuse, sources, filled_caps) != 0) return error.fail(-3, "Could not map the remaining faces");
        for (int i = 0; i < 2; ++i) {
            if (!cap_without_rim_matches(original(hole.caps[i]), hole.rims[i], filled_faces(filled_caps[i]))) return error.fail(-3, "Filled hole changed a cap boundary");
        }

        TopoDS_Shape result = filled;
        if (move) {
            const auto cutter = BRepPrimAPI_MakeCylinder(target.axis, target.radius, target.height).Shape();
            if (!cutter_clears_other_boundaries(cutter, filled_faces, filled_caps)) return error.fail(-3, "New hole intersects another face boundary");
            BRepAlgoAPI_Cut cut;
            run_hole_boolean(cut, filled, cutter);
            if (!cut.IsDone() || cut.HasErrors()) {
                return boolean_failure(error, cut, "OCCT could not cut the moved hole");
            }
            result = single_solid(cut.Shape());
            double moved_volume;
            if (result.IsNull() || !is_watertight(result) || !measure_brep_volume(result, moved_volume) ||
                !hole_volume_delta_matches(filled_volume, moved_volume, hole_volume) ||
                !hole_volume_delta_matches(moved_volume, base_volume, 0.0)) return error.fail(-3, "Moved hole did not preserve the expected solid volume");
            TopTools_IndexedMapOfShape moved_faces;
            TopExp::MapShapes(result, TopAbs_FACE, moved_faces);
            if (moved_faces.Extent() != count) return error.fail(-3, "Moved hole changed the face count");
            std::vector<int32_t> moved_sources(count, -1);
            int moved_caps[2]{};
            const int moved_wall = map_hole_faces(filled_faces, moved_faces, 0, filled_caps, &cut,
                                                  moved_sources, moved_caps);
            ThroughHole verified;
            if (moved_wall <= 0 || !read_through_hole(moved_faces, moved_wall, verified) ||
                !matches_through_hole(verified, target, target.radius, moved_caps)) return error.fail(-3, "Moved hole failed geometric verification");
            for (int i = 0; i < 2; ++i) {
                const int side = verified.caps[i] == moved_caps[0] ? 0 : 1;
                if (!cap_without_rim_matches(moved_faces(verified.caps[i]), verified.rims[i],
                                             filled_faces(filled_caps[side]))) return error.fail(-3, "Moved hole changed a cap boundary");
            }
            for (auto& source : moved_sources) source = source < 0 ? face_index : sources[source];
            sources = std::move(moved_sources);
        }
        const uint64_t result_handle = store_shape(result, record->mesh_defaults);
        if (result_handle == 0) return error.fail(-3, "Could not store hole result");
        std::copy(sources.begin(), sources.end(), out_sources);
        *out_handle = result_handle;
        return OCCT_OK;
    } catch (...) {
        return error.exception(-3);
    }
}

const char* fillet_failure(const BRepFilletAPI_MakeFillet& fillet) {
    if (fillet.NbFaultyContours() > 0) {
        switch (fillet.StripeStatus(fillet.FaultyContour(1))) {
            case ChFiDS_Error: return "OCCT fillet contour failed (ChFiDS_Error)";
            case ChFiDS_WalkingFailure: return "OCCT fillet could not follow the contour (ChFiDS_WalkingFailure)";
            case ChFiDS_StartsolFailure: return "OCCT fillet could not find a starting solution (ChFiDS_StartsolFailure)";
            case ChFiDS_TwistedSurface: return "OCCT fillet encountered a twisted surface (ChFiDS_TwistedSurface)";
            default: break;
        }
    }
    return "OCCT fillet builder did not complete";
}

const char* shell_failure(BRepOffset_Error status) {
    switch (status) {
        case BRepOffset_BadNormalsOnGeometry: return "OCCT shell encountered invalid surface normals";
        case BRepOffset_C0Geometry: return "OCCT shell encountered C0 geometry";
        case BRepOffset_NullOffset: return "OCCT shell rejected the offset distance";
        case BRepOffset_NotConnectedShell: return "OCCT shell encountered disconnected faces";
        case BRepOffset_CannotTrimEdges: return "OCCT shell could not trim edges";
        case BRepOffset_CannotFuseVertices: return "OCCT shell could not fuse vertices";
        case BRepOffset_CannotExtentEdge: return "OCCT shell could not extend an edge";
        case BRepOffset_UserBreak: return "OCCT shell operation was interrupted";
        default: return "OCCT shell builder did not complete";
    }
}

const char* step_write_failure(IFSelect_ReturnStatus status) {
    switch (status) {
        case IFSelect_RetVoid: return "OCCT STEP writer produced no output (IFSelect_RetVoid)";
        case IFSelect_RetError: return "OCCT STEP writer rejected the output (IFSelect_RetError)";
        case IFSelect_RetFail: return "OCCT STEP writer could not write the file (IFSelect_RetFail)";
        case IFSelect_RetStop: return "OCCT STEP write was interrupted (IFSelect_RetStop)";
        default: return "OCCT STEP writer did not complete";
    }
}

template <typename Operation>
uint64_t make_boolean(const BridgeError& error, uint64_t base_handle, uint64_t tool_handle) {
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto base = get_record(base_handle);
        const auto tool = get_record(tool_handle);
        if (!base || !tool) return error.no_handle(-1, "Invalid base or tool shape handle");
        TopTools_ListOfShape arguments, tools;
        arguments.Append(base->shape);
        tools.Append(tool->shape);
        Operation boolean;
        boolean.SetArguments(arguments);
        boolean.SetTools(tools);
        boolean.SetNonDestructive(Standard_True);
        boolean.Build();
        if (boolean.IsDone()) {
            const auto& shape = boolean.Shape();
            if (!shape.IsNull()) return store_shape(shape, base->mesh_defaults);
        }
        boolean_failure(error, boolean, "OCCT Boolean builder produced no shape");
        return 0;
    } catch (...) {
        return error.handle_exception();
    }
}

TopoDS_Face make_polygon_face(int plane_code, const double* xy_pairs, int point_count, double offset) {
    BRepBuilderAPI_MakePolygon poly;
    for (size_t i = 0; i < static_cast<size_t>(point_count); ++i) {
        poly.Add(map_plane_point(plane_code, xy_pairs[2 * i], xy_pairs[2 * i + 1], offset));
    }
    poly.Close();
    if (!poly.IsDone()) {
        return {};
    }
    BRepBuilderAPI_MakeFace face(poly.Wire(), Standard_True);
    return face.IsDone() ? face.Face() : TopoDS_Face();
}

// Boolean results usually wrap one solid in a compound. Dress-up and offset
// algorithms are applied to that solid; the edge order is unchanged.
TopoDS_Shape single_solid_or_shape(const TopoDS_Shape& shape) {
    if (shape.ShapeType() != TopAbs_SOLID && count_subshapes(shape, TopAbs_SOLID) == 1) {
        TopExp_Explorer exp(shape, TopAbs_SOLID);
        return exp.Current();
    }
    return shape;
}

// Only a validated closed solid is a dress-up result. Never return the input.
uint64_t store_valid_solid(const TopoDS_Shape& shape, MeshOptions mesh_defaults) {
    if (shape.IsNull() || !is_watertight(shape)) return 0;
    GProp_GProps props;
    BRepGProp::VolumeProperties(shape, props, Standard_True);
    if (!std::isfinite(props.Mass()) || props.Mass() <= 0.0) return 0;
    return store_shape(shape, mesh_defaults);
}

double solid_volume(const TopoDS_Shape& shape) {
    GProp_GProps props;
    BRepGProp::VolumeProperties(shape, props, Standard_True);
    return props.Mass();
}

// OCCT can report IsDone() yet hand back the unmodified operand (e.g. an
// impossible offset). A dress-up always adds faces; a shell always removes
// material. Anything else is a failure, not a result.
bool changed_by_dressup(const TopoDS_Shape& input, const TopoDS_Shape& result) {
    return !result.IsNull() && !result.IsSame(input) &&
           count_subshapes(result, TopAbs_FACE) > count_subshapes(input, TopAbs_FACE);
}

bool hollowed_by_shell(const TopoDS_Shape& input, const TopoDS_Shape& result) {
    if (result.IsNull() || result.IsSame(input)) return false;
    const double before = solid_volume(input);
    const double after = solid_volume(result);
    return std::isfinite(before) && std::isfinite(after) && after > 0.0 &&
           after < before * (1.0 - 1.0e-9);
}

using EdgeFaceMap =
    NCollection_IndexedDataMap<TopoDS_Shape, NCollection_List<TopoDS_Shape>, TopTools_ShapeMapHasher>;

// A feature edge joins exactly two faces with a tangent discontinuity. Seams,
// degenerate edges and splits between tangent faces cannot be dressed up.
bool is_sharp_edge(const TopoDS_Edge& edge, const NCollection_List<TopoDS_Shape>& faces) {
    if (faces.Extent() != 2 || BRep_Tool::Degenerated(edge)) return false;
    return BRep_Tool::Continuity(edge, TopoDS::Face(faces.First()), TopoDS::Face(faces.Last())) <
           GeomAbs_G1;
}

// Merge coplanar/coaxial faces split by Booleans so "all edges" means real
// feature edges. Keep the original if unification does not give a valid solid.
TopoDS_Shape unify_for_dressup(const TopoDS_Shape& shape) {
    try {
        ShapeUpgrade_UnifySameDomain unify(shape, Standard_True, Standard_True, Standard_False);
        unify.Build();
        const TopoDS_Shape result = single_solid_or_shape(unify.Shape());
        if (!result.IsNull() && is_watertight(result)) return result;
    } catch (const Standard_Failure&) {
    }
    return shape;
}

// edge_indices == nullptr selects every sharp edge of the unified solid.
// Otherwise every index is a zero-based TopExp edge-map index of the stored
// shape (as in occt_bridge_extract_topology) and must name a distinct sharp edge.
template <typename Maker>
uint64_t dress_up_edges(uint64_t handle, double value, const int32_t* edge_indices,
                        int32_t edge_count, const char* op_name) {
    const BridgeError error(op_name);
    const bool all_edges = edge_indices == nullptr;
    if (handle == 0 || !std::isfinite(value) || value <= 0.0 || (!all_edges && edge_count <= 0)) {
        return error.no_handle(-1, "Invalid shape handle, dress-up size, or edge list");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record || !is_watertight(record->shape)) {
            return error.no_handle(-2, "Dress-up requires a valid watertight solid");
        }
        const TopoDS_Shape base = single_solid_or_shape(record->shape);
        const TopoDS_Shape shape = all_edges ? unify_for_dressup(base) : base;

        EdgeFaceMap edge_faces;
        TopExp::MapShapesAndUniqueAncestors(shape, TopAbs_EDGE, TopAbs_FACE, edge_faces);
        Maker maker(shape);
        int32_t added = 0;
        if (all_edges) {
            for (int i = 1; i <= edge_faces.Extent(); ++i) {
                const TopoDS_Edge& edge = TopoDS::Edge(edge_faces.FindKey(i));
                if (!is_sharp_edge(edge, edge_faces.FindFromIndex(i)) || maker.Contour(edge) != 0) {
                    continue;
                }
                maker.Add(value, edge);
                ++added;
            }
        } else {
            TopTools_IndexedMapOfShape edges;
            TopExp::MapShapes(shape, TopAbs_EDGE, edges);
            std::vector<bool> seen(static_cast<size_t>(edges.Extent()), false);
            for (int32_t k = 0; k < edge_count; ++k) {
                const int32_t index = edge_indices[k];
                if (index < 0 || index >= edges.Extent() || seen[static_cast<size_t>(index)]) {
                    return error.no_handle(-1, "Edge index out of range or duplicated");
                }
                seen[static_cast<size_t>(index)] = true;
                const TopoDS_Edge& edge = TopoDS::Edge(edges(index + 1));
                const int ancestors = edge_faces.FindIndex(edge);
                if (ancestors == 0 || !is_sharp_edge(edge, edge_faces.FindFromIndex(ancestors))) {
                    return error.no_handle(-1, "Selected edge is not a sharp feature edge");
                }
                if (maker.Contour(edge) == 0) maker.Add(value, edge);
                ++added;
            }
        }
        if (added == 0) return error.no_handle(-3, "No eligible sharp edges found for dress-up");
        maker.Build();
        if (!maker.IsDone()) {
            if constexpr (std::is_same_v<Maker, BRepFilletAPI_MakeFillet>) {
                return error.no_handle(-3, fillet_failure(maker));
            } else {
                return error.no_handle(-3, "OCCT chamfer builder did not complete");
            }
        }
        if (!changed_by_dressup(shape, maker.Shape())) {
            return error.no_handle(-3, "OCCT dress-up builder did not produce modified geometry");
        }
        return error.handle_result(
            store_valid_solid(maker.Shape(), record->mesh_defaults),
            "OCCT dress-up result is not a valid closed solid");
    } catch (...) {
        return error.handle_exception();
    }
}

#else

void report_no_occt(char* err_buf, int32_t err_buf_len) {
    if (err_buf && err_buf_len > 0) {
        const char message[] = "OCCT support is unavailable";
        const auto length = std::min(sizeof message - 1, static_cast<size_t>(err_buf_len - 1));
        std::memcpy(err_buf, message, length);
        err_buf[length] = '\0';
    }
}

struct MockSolidRecord {
    OcctBridgeMetrics metrics;
};

std::unordered_map<uint64_t, MockSolidRecord> g_registry;

uint64_t store_mock_solid(const OcctBridgeMetrics& metrics) {
    if (metrics.volume_mm3 <= 1.0e-9 || metrics.face_count <= 0 || metrics.edge_count <= 0) {
        return 0;
    }
    const uint64_t handle = g_next_handle.fetch_add(1, std::memory_order_relaxed);
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_registry.emplace(handle, MockSolidRecord{metrics});
    return handle;
}

bool get_mock_solid(uint64_t handle, MockSolidRecord& out) {
    if (handle == 0) {
        return false;
    }
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    const auto it = g_registry.find(handle);
    if (it == g_registry.end()) {
        return false;
    }
    out = it->second;
    return true;
}

double min_bbox_extent(const OcctBridgeBBox& bbox) {
    const double dx = bbox.max[0] - bbox.min[0];
    const double dy = bbox.max[1] - bbox.min[1];
    const double dz = bbox.max[2] - bbox.min[2];
    return std::min({dx, dy, dz});
}

double intersection_volume_bbox(const OcctBridgeBBox& a, const OcctBridgeBBox& b) {
    const double ix = std::max(0.0, std::min(a.max[0], b.max[0]) - std::max(a.min[0], b.min[0]));
    const double iy = std::max(0.0, std::min(a.max[1], b.max[1]) - std::max(a.min[1], b.min[1]));
    const double iz = std::max(0.0, std::min(a.max[2], b.max[2]) - std::max(a.min[2], b.min[2]));
    return ix * iy * iz;
}

#endif

} // namespace

extern "C" {

int32_t occt_bridge_take_last_error(OcctBridgeLastError* out) {
    if (!out) return 0;
    *out = g_last_error;
    const bool had_error = g_last_error.status_code != OCCT_OK;
    g_last_error = {};
    return had_error ? 1 : 0;
}

void occt_bridge_clear_last_error(void) {
    g_last_error = {};
}

const char* occt_bridge_version() {
    return "occt-bridge/0.1.0 (OCCT dynamic LGPL-2.1 boundary)";
}

uint32_t aicad_kernel_abi_version(void) {
    return AICAD_KERNEL_ABI_VERSION;
}

#if defined(AI_NATIVE_CAD_HAS_OCCT)

uint64_t occt_bridge_make_box(
    double dx,
    double dy,
    double dz,
    double ox,
    double oy,
    double oz) {
    const BridgeError error("make_box");
    if (!std::isfinite(dx) || !std::isfinite(dy) || !std::isfinite(dz) ||
        !std::isfinite(ox) || !std::isfinite(oy) || !std::isfinite(oz) ||
        dx <= 0.0 || dy <= 0.0 || dz <= 0.0) {
        return error.no_handle(-1, "Dimensions must be positive and all parameters finite");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        gp_Pnt origin(ox, oy, oz);
        BRepPrimAPI_MakeBox builder(origin, dx, dy, dz);
        builder.Build();
        if (!builder.IsDone()) {
            return error.no_handle(-3, "OCCT primitive builder did not complete");
        }
        return error.handle_result(store_shape(builder.Shape()), "OCCT primitive builder produced no shape");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_make_cylinder(
    double diameter,
    double height,
    double ox,
    double oy,
    double oz) {
    const BridgeError error("make_cylinder");
    if (!std::isfinite(diameter) || !std::isfinite(height) ||
        !std::isfinite(ox) || !std::isfinite(oy) || !std::isfinite(oz) ||
        diameter <= 0.0 || height <= 0.0) {
        return error.no_handle(-1, "Dimensions must be positive and all parameters finite");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        gp_Ax2 axis(gp_Pnt(ox, oy, oz), gp_Dir(0.0, 0.0, 1.0));
        BRepPrimAPI_MakeCylinder builder(axis, diameter * 0.5, height);
        builder.Build();
        if (!builder.IsDone()) {
            return error.no_handle(-3, "OCCT primitive builder did not complete");
        }
        return error.handle_result(store_shape(builder.Shape()), "OCCT primitive builder produced no shape");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_boolean_cut(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_cut");
    return make_boolean<BRepAlgoAPI_Cut>(error, base_handle, tool_handle);
}

uint64_t occt_bridge_boolean_union(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_union");
    return make_boolean<BRepAlgoAPI_Fuse>(error, base_handle, tool_handle);
}

uint64_t occt_bridge_boolean_intersect(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_intersect");
    return make_boolean<BRepAlgoAPI_Common>(error, base_handle, tool_handle);
}

int32_t occt_bridge_read_through_holes(uint64_t handle, int32_t count,
                                       const int32_t* face_indices, double* out_radii) {
    const BridgeError error("read_through_holes");
    if (count < 1 || !face_indices || !out_radii) return error.fail(-1, "Invalid face count or missing indices or radii buffer");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-1, "Invalid shape handle");
        const auto base = single_solid(record->shape);
        if (base.IsNull() || !is_watertight(base)) return error.fail(-2, "Expected a single watertight solid");
        TopTools_IndexedMapOfShape faces;
        TopExp::MapShapes(base, TopAbs_FACE, faces);
        for (int32_t i = 0; i < count; ++i) {
            const int32_t index = face_indices[i];
            ThroughHole hole;
            if (index < 0 || index >= faces.Extent()) return error.fail(-1, "Face index is out of range");
            out_radii[i] = read_through_hole(faces, index + 1, hole) ? hole.radius : 0.0;
        }
        return 0;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_offset_through_hole(uint64_t handle, int32_t face_index, double delta_mm,
                                        int32_t capacity, int32_t* out_source_indices,
                                        uint64_t* out_handle) {
    const BridgeError error("offset_through_hole");
    if (face_index < 0 || !std::isfinite(delta_mm) || !out_source_indices || !out_handle) return error.fail(-1, "Invalid face index, hole offset, or output buffer");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-1, "Invalid shape handle");
        const auto base = single_solid(record->shape);
        if (base.IsNull() || !is_watertight(base)) return error.fail(-2, "Expected a single watertight solid");
        TopTools_IndexedMapOfShape original;
        TopExp::MapShapes(base, TopAbs_FACE, original);
        const int count = original.Extent();
        if (face_index >= count || capacity != count) return error.fail(-1, "Invalid face index or output capacity");
        const int wall = face_index + 1;
        ThroughHole hole;
        if (!read_through_hole(original, wall, hole)) return error.fail(-2, "Face is not the wall of a supported through hole");
        const double radius = hole.radius + delta_mm;
        if (!std::isfinite(radius) || radius <= kHoleLengthTol ||
            (delta_mm != 0.0 && std::abs(delta_mm) <= kHoleLengthTol)) return error.fail(-3, "New radius or offset lies outside the supported range");

        // A larger cylinder removes the annulus; a smaller hole is formed by
        // fusing the exact annulus between the two radii. Neither tool extends
        // beyond the hole's two planar openings.
        TopoDS_Shape result = base;
        BRepAlgoAPI_Cut cut, ring;
        BRepAlgoAPI_Fuse fuse;
        BRepAlgoAPI_BooleanOperation* history = nullptr;
        if (delta_mm != 0.0) {
            const auto cylinder = BRepPrimAPI_MakeCylinder(hole.axis, radius, hole.height).Shape();
            if (delta_mm > 0.0) {
                run_hole_boolean(cut, base, cylinder);
                history = &cut;
            } else {
                const auto old_cylinder = BRepPrimAPI_MakeCylinder(hole.axis, hole.radius, hole.height).Shape();
                run_hole_boolean(ring, old_cylinder, cylinder);
                if (!ring.IsDone() || ring.HasErrors()) {
                    return boolean_failure(error, ring, "OCCT could not construct the offset ring");
                }
                run_hole_boolean(fuse, base, ring.Shape());
                if (!fuse.IsDone() || fuse.HasErrors()) {
                    return boolean_failure(error, fuse, "OCCT could not fuse the offset ring");
                }
                // The annulus has coplanar ends that must merge with the caps.
                fuse.SimplifyResult(false, true);
                history = &fuse;
            }
            if (!history->IsDone() || history->HasErrors()) {
                return boolean_failure(error, *history, "OCCT could not offset the through hole");
            }
            result = single_solid(history->Shape());
            if (result.IsNull() || !is_watertight(result)) return error.fail(-3, "Offset did not produce a single watertight solid");
            GProp_GProps before, after;
            BRepGProp::VolumeProperties(base, before);
            BRepGProp::VolumeProperties(result, after);
            const double expected = std::numbers::pi * delta_mm * (radius + hole.radius) * hole.height;
            const double removed = before.Mass() - after.Mass();
            if (!std::isfinite(expected) || !std::isfinite(removed) ||
                std::abs(removed - expected) > std::max(1.0e-6, 1.0e-7 * std::abs(expected))) return error.fail(-3, "Hole offset did not produce the expected volume change");
        }

        TopTools_IndexedMapOfShape updated;
        TopExp::MapShapes(result, TopAbs_FACE, updated);
        if (updated.Extent() != count) return error.fail(-3, "Hole offset changed the face count");
        std::vector<int32_t> sources(count, -1);
        int updated_caps[2]{};
        for (int i = 1; i <= count; ++i) {
            if (i == wall) continue;
            int j = updated.FindIndex(original(i));
            const bool cap = i == hole.caps[0] || i == hole.caps[1];
            if (history && cap && j == 0) {
                const auto& modified = history->Modified(original(i));
                if (modified.Extent() == 1) j = updated.FindIndex(modified.First());
            }
            // Outside the two cap faces, every original non-wall face must
            // still be present unchanged. An extra face cannot go unnoticed.
            if (j == 0 || sources[j - 1] != -1) return error.fail(-3, "Hole offset changed an unrelated face");
            sources[j - 1] = i - 1;
            if (cap) updated_caps[i == hole.caps[0] ? 0 : 1] = j;
        }
        const auto new_wall = std::find(sources.begin(), sources.end(), -1);
        if (new_wall == sources.end()) return error.fail(-3, "Hole offset did not produce a new wall face");
        ThroughHole verified;
        if (!read_through_hole(updated, static_cast<int>(new_wall - sources.begin()) + 1, verified) ||
            std::abs(verified.radius - radius) > kHoleLengthTol ||
            std::abs(verified.height - hole.height) > kHoleLengthTol ||
            !verified.axis.Direction().IsParallel(hole.axis.Direction(), kHoleAngleTol) ||
            (verified.caps[0] != updated_caps[0] && verified.caps[0] != updated_caps[1]) ||
            (verified.caps[1] != updated_caps[0] && verified.caps[1] != updated_caps[1]) ||
            std::min(verified.axis.Location().Distance(hole.axis.Location()),
                     verified.axis.Location().Distance(hole.axis.Location().Translated(
                         gp_Vec(hole.axis.Direction()) * hole.height))) > kHoleLengthTol) return error.fail(-3, "Offset hole failed geometric verification");
        *new_wall = face_index;
        const uint64_t new_handle = store_shape(result, record->mesh_defaults);
        if (new_handle == 0) return error.fail(-3, "Could not store the offset result");
        std::copy(sources.begin(), sources.end(), out_source_indices);
        *out_handle = new_handle;
        return OCCT_OK;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_move_through_hole(uint64_t handle, int32_t face_index, const double* offset_mm,
                                      int32_t capacity, int32_t* out_source_indices,
                                      uint64_t* out_handle) {
    const BridgeError error("move_through_hole");
    if (!finite_xyz(offset_mm)) return error.fail(-1, "Hole offset must contain three finite values");
    return relocate_through_hole(error, handle, face_index, offset_mm, capacity, out_source_indices, out_handle);
}

int32_t occt_bridge_delete_through_hole(uint64_t handle, int32_t face_index, int32_t capacity,
                                        int32_t* out_source_indices, uint64_t* out_handle) {
    const BridgeError error("delete_through_hole");
    return relocate_through_hole(error, handle, face_index, nullptr, capacity, out_source_indices, out_handle);
}

int32_t occt_bridge_has_exact_interference() { return 1; }

int32_t occt_bridge_check_interference(uint64_t a_handle, uint64_t b_handle,
                                      double min_clearance_mm, double* out_measurement) {
    const BridgeError error("check_interference");
    if (!out_measurement || !std::isfinite(min_clearance_mm) || min_clearance_mm < 0.0) return error.fail(-1, "Invalid minimum clearance or missing measurement output");
    constexpr double volume_tolerance_mm3 = 1e-4;
    constexpr double distance_tolerance_mm = 1e-4;
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        TopoDS_Shape a, b;
        if (!get_shape(a_handle, a) || !get_shape(b_handle, b)) return error.fail(-1, "Invalid shape handle");
        if (!is_watertight(a) || !is_watertight(b)) return error.fail(-2, "Expected two watertight shapes");

        TopTools_ListOfShape arguments, tools;
        arguments.Append(a);
        tools.Append(b);
        BRepAlgoAPI_Common common;
        common.SetArguments(arguments);
        common.SetTools(tools);
        common.SetNonDestructive(Standard_True);
        common.Build();
        if (!common.IsDone() || common.HasErrors() || common.Shape().IsNull()) {
            return boolean_failure(error, common, "OCCT could not compute the intersection for interference");
        }

        // Only common solids contribute volume, never shared contact faces or edges.
        double volume_mm3 = 0.0;
        TopTools_IndexedMapOfShape solids;
        TopExp::MapShapes(common.Shape(), TopAbs_SOLID, solids);
        for (int index = 1; index <= solids.Extent(); ++index) {
            const auto& solid = solids(index);
            if (!is_watertight(solid)) return error.fail(-3, "Intersection solid is not watertight");
            double volume;
            if (!measure_brep_volume(solid, volume)) return error.fail(-3, "Could not measure intersection volume");
            volume_mm3 += volume;
        }
        if (!std::isfinite(volume_mm3)) return error.fail(-3, "Intersection volume is not finite");
        if (volume_mm3 > volume_tolerance_mm3) {
            *out_measurement = volume_mm3;
            return 1;
        }
        const double distance_limit = min_clearance_mm - distance_tolerance_mm;
        if (distance_limit <= 0.0) return 0; // No nonnegative distance can violate it.

        // A subthreshold common volume passes the overlap gate but still has zero clearance.
        double distance_mm = 0.0;
        if (volume_mm3 == 0.0) {
            BRepExtrema_DistShapeShape distance;
            distance.LoadS1(a);
            distance.LoadS2(b);
            if (!distance.Perform() || !distance.IsDone() || distance.NbSolution() == 0) return error.fail(-3, "OCCT could not compute clearance distance");
            distance_mm = distance.InnerSolution() ? 0.0 : distance.Value();
            if (!std::isfinite(distance_mm) || distance_mm < 0.0) return error.fail(-3, "Clearance distance is invalid");
        }
        if (distance_mm < distance_limit) {
            *out_measurement = distance_mm;
            return 2;
        }
        return 0;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_measure_symmetric_difference(uint64_t candidate_handle,
                                                  uint64_t reference_handle,
                                                  OcctBridgeSymmetricDifference* out) {
    const BridgeError error("measure_symmetric_difference");
    if (!out) return error.fail(-1, "Missing symmetric difference output");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        TopoDS_Shape candidate, reference;
        if (!get_shape(candidate_handle, candidate) || !get_shape(reference_handle, reference)) return error.fail(-1, "Invalid shape handle");
        if (!is_watertight(candidate) || !is_watertight(reference)) return error.fail(-2, "Expected two watertight shapes");

        TopTools_IndexedMapOfShape candidate_solids, reference_solids;
        TopExp::MapShapes(candidate, TopAbs_SOLID, candidate_solids);
        TopExp::MapShapes(reference, TopAbs_SOLID, reference_solids);
        if (candidate_solids.Extent() != 1 || reference_solids.Extent() != 1) return error.fail(-5, "Expected one solid in each shape");
        const auto& a = candidate_solids(1);
        const auto& b = reference_solids(1);
        OcctBridgeSymmetricDifference result{};
        double candidate_volume;
        if (!measure_brep_volume(a, candidate_volume) || candidate_volume <= 0.0 ||
            !measure_brep_volume(b, result.reference_volume_mm3) || result.reference_volume_mm3 <= 0.0) return error.fail(-2, "Expected finite positive input volumes");

        if (!a.IsEqual(b) &&
            (!measure_cut_volume(a, b, result.candidate_only_mm3) ||
             !measure_cut_volume(b, a, result.reference_only_mm3))) return error.fail(-3, "OCCT could not measure the two Boolean differences");
        *out = result;
        return 0;
    } catch (...) {
        return error.exception(-3);
    }
}

uint64_t occt_bridge_fillet_all(uint64_t handle, double radius) {
    return dress_up_edges<BRepFilletAPI_MakeFillet>(handle, radius, nullptr, 0, "fillet_all");
}

uint64_t occt_bridge_chamfer_all(uint64_t handle, double distance) {
    return dress_up_edges<BRepFilletAPI_MakeChamfer>(handle, distance, nullptr, 0, "chamfer_all");
}

uint64_t occt_bridge_fillet_edges(uint64_t handle, double radius, const int32_t* edge_indices,
                                  int32_t edge_count) {
    if (!edge_indices) {
        const BridgeError error("fillet_edges");
        return error.no_handle(-1, "Missing edge index array");
    }
    return dress_up_edges<BRepFilletAPI_MakeFillet>(
        handle, radius, edge_indices, edge_count, "fillet_edges");
}

uint64_t occt_bridge_chamfer_edges(uint64_t handle, double distance, const int32_t* edge_indices,
                                   int32_t edge_count) {
    if (!edge_indices) {
        const BridgeError error("chamfer_edges");
        return error.no_handle(-1, "Missing edge index array");
    }
    return dress_up_edges<BRepFilletAPI_MakeChamfer>(
        handle, distance, edge_indices, edge_count, "chamfer_edges");
}

uint64_t occt_bridge_shell(
    uint64_t handle,
    double thickness,
    double open_nx,
    double open_ny,
    double open_nz) {
    const BridgeError error("shell");
    if (handle == 0 || !std::isfinite(thickness) || thickness <= 0.0 ||
        !std::isfinite(open_nx) || !std::isfinite(open_ny) || !std::isfinite(open_nz)) {
        return error.no_handle(-1, "Invalid shape handle or shell thickness");
    }
    const double norm_len = std::sqrt(open_nx * open_nx + open_ny * open_ny + open_nz * open_nz);
    if (!std::isfinite(norm_len) || norm_len <= 1.0e-12) {
        return error.no_handle(-1, "Shell opening direction must have a finite nonzero length");
    }
    try {
        const gp_Vec target_dir(open_nx / norm_len, open_ny / norm_len, open_nz / norm_len);
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record || !is_watertight(record->shape)) {
            return error.no_handle(-2, "Shell requires a valid watertight solid");
        }
        const TopoDS_Shape shape = unify_for_dressup(single_solid_or_shape(record->shape));

        TopoDS_Face best_face;
        double best_dot = -2.0;
        for (TopExp_Explorer exp(shape, TopAbs_FACE); exp.More(); exp.Next()) {
            const TopoDS_Face face = TopoDS::Face(exp.Current());
            BRepGProp_Face prop_face(face);
            double umin = 0.0, umax = 0.0, vmin = 0.0, vmax = 0.0;
            prop_face.Bounds(umin, umax, vmin, vmax);
            gp_Pnt mid_pt;
            gp_Vec normal_vec;
            prop_face.Normal(0.5 * (umin + umax), 0.5 * (vmin + vmax), mid_pt, normal_vec);
            if (normal_vec.Magnitude() > 1.0e-12) {
                normal_vec.Normalize();
                const double dot = normal_vec.Dot(target_dir);
                if (dot > best_dot) {
                    best_dot = dot;
                    best_face = face;
                }
            }
        }

        // Never open an arbitrary face: the opening must face the requested direction.
        if (best_face.IsNull() || best_dot < 0.9) {
            return error.no_handle(-3, "Shape has no face aligned with the shell opening normal");
        }

        TopTools_ListOfShape closing_faces;
        closing_faces.Append(best_face);

        BRepOffsetAPI_MakeThickSolidByJoin thick_maker;
        thick_maker.MakeThickSolidByJoin(shape, closing_faces, -thickness, 1.0e-3);
        if (!thick_maker.IsDone() || !hollowed_by_shell(shape, thick_maker.Shape())) {
            return error.no_handle(-3, shell_failure(thick_maker.MakeOffset().Error()));
        }
        return error.handle_result(
            store_valid_solid(thick_maker.Shape(), record->mesh_defaults),
            "OCCT shell result is not a valid closed solid");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_extrude_polygon(
    int plane_code,
    const double* xy_pairs,
    int point_count,
    double height,
    int symmetric) {
    const BridgeError error("extrude_polygon");
    if (!is_valid_polygon_params(plane_code, xy_pairs, point_count, height)) {
        return error.no_handle(-1, "Invalid extrusion plane, polygon, or height");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const double z0 = (symmetric != 0) ? (-0.5 * height) : 0.0;
        const auto face = make_polygon_face(plane_code, xy_pairs, point_count, z0);
        if (face.IsNull()) return error.no_handle(-3, "OCCT could not construct the extrusion profile");

        const gp_Vec extrude_vec = map_plane_extrude_vec(plane_code, height);
        BRepPrimAPI_MakePrism prism(face, extrude_vec);
        prism.Build();
        if (!prism.IsDone()) {
            return error.no_handle(-3, "OCCT prism builder did not complete");
        }
        return error.handle_result(store_shape(prism.Shape()), "OCCT prism builder produced no shape");
    } catch (...) {
        return error.handle_exception();
    }
}

int32_t occt_bridge_check(uint64_t handle, OcctBridgeMetrics* out_metrics) {
    const BridgeError error("check");
    if (!out_metrics) {
        return error.fail(-1, "Missing metrics output");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-2, "Invalid shape handle");
        const auto& shape = record->shape;

        *out_metrics = {};
        // Import already validated this exact stored root. New shapes, including
        // placed components and Boolean results, still run the regular check.
        out_metrics->watertight = record->imported_watertight >= 0
            ? record->imported_watertight : (is_watertight(shape) ? 1 : 0);

        GProp_GProps vol_props;
        // Adaptive integration to a relative error of 1e-9, so reported volumes
        // do not depend on parametrization details such as seam placement.
        if (out_metrics->watertight) {
            BRepGProp::VolumeProperties(shape, vol_props, 1.0e-9, Standard_True);
        }
        out_metrics->volume_mm3 = vol_props.Mass();
        if (out_metrics->watertight &&
            (!std::isfinite(out_metrics->volume_mm3) || out_metrics->volume_mm3 <= 0.0)) {
            out_metrics->watertight = 0;
            out_metrics->volume_mm3 = 0.0;
        }

        Bnd_Box box;
        BRepBndLib::AddOptimal(shape, box, Standard_False, Standard_False);
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

        out_metrics->face_count = count_subshapes(shape, TopAbs_FACE);
        out_metrics->edge_count = count_subshapes(shape, TopAbs_EDGE);
        return 0;
    } catch (...) {
        return error.exception(-4, -3);
    }
}

static int32_t export_step_impl(const BridgeError& error, uint64_t handle,
                                const char* path_utf8, const char* name_utf8);

int32_t occt_bridge_export_step(uint64_t handle, const char* path_utf8) {
    const BridgeError error("export_step");
    return export_step_impl(error, handle, path_utf8, nullptr);
}

int32_t occt_bridge_volume_centroid(uint64_t handle, double out_mm[3]) {
    const BridgeError error("volume_centroid");
    if (!out_mm) return error.fail(-1, "Missing centroid output");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        TopoDS_Shape shape;
        if (!get_shape(handle, shape) || !is_watertight(shape)) return error.fail(-2, "Invalid handle or shape is not watertight");
        GProp_GProps props;
        BRepGProp::VolumeProperties(shape, props, Standard_True);
        if (!std::isfinite(props.Mass()) || props.Mass() <= 0.0) return error.fail(-2, "Shape has no finite positive volume");
        const gp_Pnt center = props.CentreOfMass();
        if (!std::isfinite(center.X()) || !std::isfinite(center.Y()) || !std::isfinite(center.Z())) {
            return error.fail(-2, "Centroid coordinates are not finite");
        }
        out_mm[0] = center.X();
        out_mm[1] = center.Y();
        out_mm[2] = center.Z();
        return 0;
    } catch (...) {
        return error.exception(-4, -3);
    }
}

int32_t occt_bridge_export_step_named(uint64_t handle, const char* path_utf8, const char* name_utf8) {
    const BridgeError error("export_step_named");
    return export_step_impl(error, handle, path_utf8, name_utf8);
}

static int32_t export_step_impl(const BridgeError& error, uint64_t handle,
                                const char* path_utf8, const char* name_utf8) {
    if (!path_utf8) {
        return error.fail(-1, "Missing STEP output path");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        TopoDS_Shape shape;
        if (!get_shape(handle, shape)) {
            return error.fail(-2, "Invalid shape handle");
        }

        Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
        Handle(XCAFDoc_ShapeTool) shape_tool = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        const bool name_product = name_utf8 && *name_utf8;
        // Boolean operators may wrap a single solid in a compound. Keep its
        // product name on the solid instead of naming a synthetic assembly.
        const bool as_assembly = !name_product || count_subshapes(shape, TopAbs_SOLID) != 1;
        const TDF_Label label = shape_tool->AddShape(shape, as_assembly);
        if (name_product) {
            TDataStd_Name::Set(label, TCollection_ExtendedString(name_utf8, Standard_True));
        }

        const QuietExchangeTranslation quiet;
        STEPCAFControl_Writer writer;
        writer.SetNameMode(Standard_True);
        // OCCT 7.8 uses this process-wide STEP writer parameter. Hold the
        // bridge lock through translation and restore it even on failure.
        struct RestoreSchema {
            std::string previous = Interface_Static::CVal("write.step.schema");
            ~RestoreSchema() { Interface_Static::SetCVal("write.step.schema", previous.c_str()); }
        } restore_schema;
        if (!Interface_Static::SetCVal("write.step.schema", "AP242DIS")) {
            return error.fail(-3, "OCCT rejected the AP242DIS STEP schema");
        }
        if (!writer.Transfer(doc, STEPControl_AsIs)) {
            return error.fail(-3, "OCCT could not transfer the shape for STEP export");
        }
        const IFSelect_ReturnStatus status = writer.Write(path_utf8);
        return status == IFSelect_RetDone ? OCCT_OK : error.fail(-4, step_write_failure(status));
    } catch (...) {
        return error.exception(-6, -5);
    }
}

int32_t occt_bridge_export_step_assembly(int32_t count, const uint64_t* handles,
                                         const char* const* names_utf8,
                                         const char* assembly_name_utf8, const char* path_utf8) {
    const BridgeError error("export_step_assembly");
    if (count < 1 || !handles || !names_utf8 || !path_utf8 || !assembly_name_utf8 ||
        !*assembly_name_utf8) {
        return error.fail(-1, "Assembly export needs components, names, an assembly name and a path");
    }
    for (int32_t i = 0; i < count; ++i) {
        if (!names_utf8[i] || !*names_utf8[i]) {
            return error.fail(-1, "Assembly component names must be non-empty");
        }
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        std::vector<TopoDS_Shape> parts;
        parts.reserve(static_cast<size_t>(count));
        for (int32_t i = 0; i < count; ++i) {
            TopoDS_Shape shape;
            if (!get_shape(handles[i], shape) || shape.IsNull()) {
                return error.fail(-2, "Invalid assembly component handle");
            }
            parts.push_back(single_solid_or_shape(shape));
        }

        Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
        Handle(XCAFDoc_ShapeTool) shape_tool = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        const TDF_Label root = shape_tool->NewShape();
        TDataStd_Name::Set(root, TCollection_ExtendedString(assembly_name_utf8, Standard_True));
        for (int32_t i = 0; i < count; ++i) {
            const TopoDS_Shape& shape = parts[static_cast<size_t>(i)];
            const TCollection_ExtendedString name(names_utf8[i], Standard_True);
            // The part keeps the exact geometry; its placement becomes the
            // component location, so instances stay explicit in the STEP file.
            const TDF_Label part = shape_tool->AddShape(shape.Located(TopLoc_Location()), Standard_False);
            const TDF_Label component = shape_tool->AddComponent(root, part, shape.Location());
            if (part.IsNull() || component.IsNull()) {
                return error.fail(-3, "OCCT could not add an assembly component");
            }
            TDataStd_Name::Set(part, name);
            TDataStd_Name::Set(component, name);
        }
        shape_tool->UpdateAssemblies();

        const QuietExchangeTranslation quiet;
        STEPCAFControl_Writer writer;
        writer.SetNameMode(Standard_True);
        struct RestoreSchema {
            std::string previous = Interface_Static::CVal("write.step.schema");
            ~RestoreSchema() { Interface_Static::SetCVal("write.step.schema", previous.c_str()); }
        } restore_schema;
        if (!Interface_Static::SetCVal("write.step.schema", "AP242DIS")) {
            return error.fail(-3, "OCCT rejected the AP242DIS STEP schema");
        }
        if (!writer.Transfer(doc, STEPControl_AsIs)) {
            return error.fail(-3, "OCCT could not transfer the assembly for STEP export");
        }
        const IFSelect_ReturnStatus status = writer.Write(path_utf8);
        return status == IFSelect_RetDone ? OCCT_OK : error.fail(-4, step_write_failure(status));
    } catch (...) {
        return error.exception(-3);
    }
}

uint64_t occt_bridge_import_file(const char* path_utf8, int32_t format, const OcctImportOptions* opts,
                                 OcctImportMetadata* out_meta, char* err_buf, int32_t err_buf_len) {
    const BridgeError error("import_file");
    const auto report = [&](int32_t status, std::string_view message) {
        if (err_buf_len > 0) copy_utf8(err_buf, static_cast<size_t>(err_buf_len), message);
        return error.no_handle(status, message);
    };
    if (err_buf_len > 0) copy_utf8(err_buf, static_cast<size_t>(err_buf_len), "");
    if (out_meta) *out_meta = {};
    try {
        if (!path_utf8 || !*path_utf8) {
            return report(-1, "Missing model file path");
        }
        const auto path = utf8_path(path_utf8);
        if (format == 0) {
            const auto ext = lower_ascii(path.extension().string());
            if (ext == ".step" || ext == ".stp") format = 1;
            else if (ext == ".iges" || ext == ".igs") format = 2;
            else if (ext == ".brep") format = 3;
        }
        if (format < 1 || format > 3) {
            return report(-1, "Unsupported model format; expected STEP, IGES or BREP");
        }
        const auto deflection = mesh_options(opts);
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        TopoDS_Shape shape;
        std::vector<Component> components;
        std::string_view source_unit = "unknown";
        if (format == 3) {
            // Native BREP has no unit metadata. Interpret coordinates as mm, but report the source as unknown.
            std::ifstream stream(path, std::ios::binary);
            if (!stream) {
                return report(-3, "Could not open BREP file");
            }
            BRep_Builder builder;
            BRepTools::Read(shape, stream, builder);
            shape = prepare_imported_shape(shape, opts);
            if (!shape.IsNull()) {
                add_component(components, shape.Located(TopLoc_Location()), shape.Location(), "");
            }
        } else {
            Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
            if (format == 1) {
                STEPCAFControl_Reader reader;
                reader.SetNameMode(Standard_True);
                std::ifstream stream(path, std::ios::binary);
                if (!stream || reader.ReadStream(path_utf8, stream) != IFSelect_RetDone) {
                    return report(-3, "Could not read STEP file");
                }
                auto& step = reader.ChangeReader();
                STEPConstruct_ExternRefs external(step.WS());
                external.LoadExternRefs();
                if (external.NbExternRefs() != 0) {
                    return report(-3, "External STEP references are not supported");
                }
                source_unit = step_source_unit(step);
                Interface_Static::SetCVal("xstep.cascade.unit", "MM");
                step.SetSystemLengthUnit(1.0);
                XCAFDoc_DocumentTool::SetLengthUnit(doc, 0.001);
                if (!reader.Transfer(doc)) {
                    return report(-3, "Could not transfer STEP geometry");
                }
            } else {
                const QuietExchangeTranslation quiet;
                IGESCAFControl_Reader reader;
                reader.SetNameMode(Standard_True);
                if (reader.ReadFile(path_utf8) != IFSelect_RetDone) {
                    return report(-3, "Could not read IGES file");
                }
                if (has_iges_external_references(reader.IGESModel())) {
                    return report(-3, "External IGES references are not supported");
                }
                source_unit = iges_source_unit(reader);
                Interface_Static::SetCVal("xstep.cascade.unit", "MM");
                XCAFDoc_DocumentTool::SetLengthUnit(doc, 0.001);
                if (!reader.Transfer(doc)) {
                    return report(-3, "Could not transfer IGES geometry");
                }
            }
            shape = xde_root(doc, opts, components);
        }
        if (shape.IsNull()) {
            return report(-3, "Model has no transferable geometry");
        }
        OcctImportMetadata metadata{};
        copy_utf8(metadata.unit_detected, sizeof metadata.unit_detected, source_unit);
        metadata.is_watertight_solid = is_watertight(shape, &metadata.open_shell_count) ? 1 : 0;
        metadata.component_count = static_cast<int32_t>(components.size());
        const auto handle = store_shape(std::move(shape), deflection, std::move(components),
                                        metadata.is_watertight_solid);
        if (out_meta) *out_meta = metadata;
        return handle != 0 ? handle : report(-3, "OCCT could not store the imported shape");
    } catch (const Standard_Failure& failure) {
        const char* message = failure.GetMessageString();
        return report(-3, message && *message ? message : "OCCT model import failed");
    } catch (const std::exception& failure) {
        return report(-3, failure.what());
    } catch (...) {
        return report(-3, "Model import failed");
    }
}

int32_t occt_bridge_get_components(uint64_t root_handle, OcctComponentDescriptor* out_items, int32_t max_items) {
    const BridgeError error("get_components");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto root = get_record(root_handle);
        if (!root) return error.fail(-2, "Invalid root shape handle");
        const auto count = static_cast<int32_t>(root->components.size());
        if (!out_items || max_items == 0) return count;
        if (max_items < count) return error.fail(OCCT_ERR_BUFFER_TOO_SMALL, "Component output capacity is too small");
        std::vector<OcctComponentDescriptor> items;
        items.reserve(root->components.size());
        try {
            for (const auto& component : root->components) {
                auto item = component.descriptor;
                item.shape_handle = store_shape(component.local_shape, root->mesh_defaults);
                items.push_back(item);
            }
        } catch (...) {
            for (const auto& item : items) free_shape_locked(item.shape_handle);
            throw;
        }
        if (!items.empty()) std::copy(items.begin(), items.end(), out_items);
        return count;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_get_component_prototypes(uint64_t root_handle, int32_t* out_indices,
                                              int32_t capacity) {
    const BridgeError error("get_component_prototypes");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto root = get_record(root_handle);
        if (!root) return error.fail(-2, "Invalid root shape handle");
        const auto count = static_cast<int32_t>(root->components.size());
        if (!out_indices || capacity < count) return error.fail(OCCT_ERR_BUFFER_TOO_SMALL, "Component output capacity is too small");
        // OCCT's indexed map compares TShape and location, but not orientation.
        TopTools_IndexedMapOfShape shapes[4];
        std::vector<int32_t> first[4];
        for (int32_t i = 0; i < count; ++i) {
            const auto& local = root->components[i].local_shape;
            const auto kind = static_cast<size_t>(local.Orientation());
            const auto key = static_cast<size_t>(shapes[kind].Add(local)) - 1;
            if (key == first[kind].size()) first[kind].push_back(i);
            out_indices[i] = first[kind][key];
        }
        return count;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_mesh_and_query_counts(uint64_t handle, const OcctImportOptions* opts,
                                          OcctTessellationCounts* out_counts) {
    const BridgeError error("mesh_and_query_counts");
    if (!out_counts) return error.fail(-1, "Missing tessellation counts output");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-2, "Invalid shape handle");
        const auto requested = mesh_options(opts, record->mesh_defaults);
        if (g_mesh_query.handle != handle || g_mesh_query.options.linear != requested.linear ||
            g_mesh_query.options.angular != requested.angular) {
            g_mesh_query = make_mesh(handle, record->shape, requested);
        }
        *out_counts = g_mesh_query.counts;
        return OCCT_OK;
    } catch (...) {
        return error.exception(-3);
    }
}

int32_t occt_bridge_extract_tessellation(uint64_t handle, int32_t max_faces, uint32_t max_vertices,
                                         uint32_t max_indices, OcctFaceDescriptor* out_faces,
                                         double* out_positions_mm, double* out_normals, uint32_t* out_indices) {
    const BridgeError error("extract_tessellation");
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record || g_mesh_query.handle != handle) return error.fail(-2, "Invalid shape handle or missing mesh query on this thread");
        const auto& mesh = g_mesh_query;
        const auto& counts = mesh.counts;
        if (max_faces < counts.face_count || max_vertices < counts.total_vertices ||
            max_indices < counts.total_indices) {
            return error.fail(OCCT_ERR_BUFFER_TOO_SMALL, "Tessellation output capacity is too small");
        }
        if ((counts.face_count && !out_faces) ||
            (counts.total_vertices && (!out_positions_mm || !out_normals)) ||
            (counts.total_indices && !out_indices)) {
            return error.fail(-1, "Missing tessellation output buffer");
        }
        if (counts.face_count) std::copy(mesh.faces.begin(), mesh.faces.end(), out_faces);
        if (counts.total_vertices) {
            std::copy(mesh.positions.begin(), mesh.positions.end(), out_positions_mm);
            std::copy(mesh.normals.begin(), mesh.normals.end(), out_normals);
        }
        if (counts.total_indices) std::copy(mesh.indices.begin(), mesh.indices.end(), out_indices);
        return OCCT_OK;
    } catch (...) {
        return error.exception(-3);
    }
}

uint64_t occt_bridge_transform(uint64_t handle, const double mat_col_major[16]) {
    const BridgeError error("transform");
    if (!mat_col_major || !std::all_of(mat_col_major, mat_col_major + 16, [](double x) { return std::isfinite(x); })) {
        return error.no_handle(-1, "Transform matrix must contain 16 finite values");
    }
    const auto* m = mat_col_major;
    constexpr double tolerance = 1.0e-8;
    if (std::abs(m[3]) > tolerance || std::abs(m[7]) > tolerance ||
        std::abs(m[11]) > tolerance || std::abs(m[15] - 1.0) > tolerance) {
        return error.no_handle(-1, "Transform matrix must be affine");
    }
    // Only rigid transforms (including reflections); never silently orthogonalize a scale/shear.
    for (int a = 0; a < 3; ++a) {
        for (int b = a; b < 3; ++b) {
            const double dot = m[4 * a] * m[4 * b] + m[4 * a + 1] * m[4 * b + 1] +
                               m[4 * a + 2] * m[4 * b + 2];
            if (!std::isfinite(dot) || std::abs(dot - (a == b ? 1.0 : 0.0)) > tolerance) return error.no_handle(-1, "Transform matrix must be rigid");
        }
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.no_handle(-1, "Invalid shape handle");
        gp_Trsf transform;
        transform.SetValues(m[0], m[4], m[8], m[12], m[1], m[5], m[9], m[13], m[2], m[6], m[10], m[14]);
        BRepBuilderAPI_Transform builder(record->shape, transform, Standard_True);
        if (!builder.IsDone()) return error.no_handle(-3, "OCCT transform builder did not complete");
        return error.handle_result(store_shape(builder.Shape(), record->mesh_defaults), "OCCT transform produced no shape");
    } catch (...) {
        return error.handle_exception();
    }
}

uint64_t occt_bridge_revolve_polygon(int plane_code, const double* xy_pairs, int point_count,
                                     const double axis_origin[3], const double axis_dir[3], double angle_deg) {
    const BridgeError error("revolve_polygon");
    if (!is_valid_polygon_params(plane_code, xy_pairs, point_count, angle_deg) ||
        std::abs(angle_deg) > 360.0 || !finite_xyz(axis_origin) || !finite_xyz(axis_dir)) {
        return error.no_handle(-1, "Invalid revolution plane, polygon, angle, or axis");
    }
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto face = make_polygon_face(plane_code, xy_pairs, point_count, 0.0);
        if (face.IsNull()) return error.no_handle(-3, "OCCT could not construct the revolution profile");
        const double sign = angle_deg < 0.0 ? -1.0 : 1.0;
        const double magnitude = std::hypot(axis_dir[0], axis_dir[1], axis_dir[2]);
        if (!std::isfinite(magnitude) || magnitude == 0.0) return error.no_handle(-1, "Revolution axis direction must have a finite nonzero length");
        const gp_Ax1 axis(gp_Pnt(axis_origin[0], axis_origin[1], axis_origin[2]),
                          gp_Dir(sign * (axis_dir[0] / magnitude), sign * (axis_dir[1] / magnitude),
                                 sign * (axis_dir[2] / magnitude)));
        BRepPrimAPI_MakeRevol builder(face, axis, std::abs(angle_deg) * std::numbers::pi / 180.0, Standard_True);
        if (!builder.IsDone()) return error.no_handle(-3, "OCCT revolution builder did not complete");
        if (!is_watertight(builder.Shape())) return error.no_handle(-3, "Revolution did not produce a watertight shape");
        return error.handle_result(store_shape(builder.Shape()), "OCCT revolution produced no shape");
    } catch (...) {
        return error.handle_exception();
    }
}

void occt_bridge_free(uint64_t handle) {
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        free_shape_locked(handle);
    } catch (...) {
        // No C++ exception may cross the C boundary.
    }
}

#else

uint64_t occt_bridge_make_box(
    double dx,
    double dy,
    double dz,
    double ox,
    double oy,
    double oz) {
    const BridgeError error("make_box");
    if (!std::isfinite(dx) || !std::isfinite(dy) || !std::isfinite(dz) ||
        !std::isfinite(ox) || !std::isfinite(oy) || !std::isfinite(oz) ||
        dx <= 0.0 || dy <= 0.0 || dz <= 0.0) {
        return error.no_handle(-1, "Dimensions must be positive and all parameters finite");
    }
    OcctBridgeMetrics m{};
    m.watertight = 1;
    m.volume_mm3 = dx * dy * dz;
    m.bbox.min[0] = ox;
    m.bbox.min[1] = oy;
    m.bbox.min[2] = oz;
    m.bbox.max[0] = ox + dx;
    m.bbox.max[1] = oy + dy;
    m.bbox.max[2] = oz + dz;
    m.face_count = 6;
    m.edge_count = 12;
    return store_mock_solid(m);
}

uint64_t occt_bridge_make_cylinder(
    double diameter,
    double height,
    double ox,
    double oy,
    double oz) {
    const BridgeError error("make_cylinder");
    if (!std::isfinite(diameter) || !std::isfinite(height) ||
        !std::isfinite(ox) || !std::isfinite(oy) || !std::isfinite(oz) ||
        diameter <= 0.0 || height <= 0.0) {
        return error.no_handle(-1, "Dimensions must be positive and all parameters finite");
    }
    const double r = 0.5 * diameter;
    constexpr double kPi = 3.14159265358979323846;
    OcctBridgeMetrics m{};
    m.watertight = 1;
    m.volume_mm3 = kPi * r * r * height;
    m.bbox.min[0] = ox - r;
    m.bbox.min[1] = oy - r;
    m.bbox.min[2] = oz;
    m.bbox.max[0] = ox + r;
    m.bbox.max[1] = oy + r;
    m.bbox.max[2] = oz + height;
    m.face_count = 3;
    m.edge_count = 3;
    return store_mock_solid(m);
}

uint64_t occt_bridge_boolean_cut(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_cut");
    MockSolidRecord base{}, tool{};
    if (!get_mock_solid(base_handle, base) || !get_mock_solid(tool_handle, tool)) {
        return error.no_handle(-1, "Invalid base or tool shape handle");
    }
    const double overlap_vol = std::min(
        tool.metrics.volume_mm3,
        intersection_volume_bbox(base.metrics.bbox, tool.metrics.bbox));
    const double rem_vol = std::max(1.0e-3, base.metrics.volume_mm3 - overlap_vol);
    OcctBridgeMetrics m = base.metrics;
    m.watertight = (base.metrics.watertight != 0 && tool.metrics.watertight != 0) ? 1 : 0;
    m.volume_mm3 = rem_vol;
    m.face_count = base.metrics.face_count + std::max(1, tool.metrics.face_count - 2);
    m.edge_count = base.metrics.edge_count + tool.metrics.edge_count;
    return store_mock_solid(m);
}

uint64_t occt_bridge_boolean_union(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_union");
    MockSolidRecord base{}, tool{};
    if (!get_mock_solid(base_handle, base) || !get_mock_solid(tool_handle, tool)) {
        return error.no_handle(-1, "Invalid base or tool shape handle");
    }
    const double overlap_vol = std::min(
        {base.metrics.volume_mm3,
         tool.metrics.volume_mm3,
         intersection_volume_bbox(base.metrics.bbox, tool.metrics.bbox)});
    OcctBridgeMetrics m{};
    m.watertight = (base.metrics.watertight != 0 && tool.metrics.watertight != 0) ? 1 : 0;
    m.volume_mm3 = std::max(
        base.metrics.volume_mm3,
        base.metrics.volume_mm3 + tool.metrics.volume_mm3 - 0.5 * overlap_vol);
    for (int i = 0; i < 3; ++i) {
        m.bbox.min[i] = std::min(base.metrics.bbox.min[i], tool.metrics.bbox.min[i]);
        m.bbox.max[i] = std::max(base.metrics.bbox.max[i], tool.metrics.bbox.max[i]);
    }
    m.face_count = base.metrics.face_count + tool.metrics.face_count;
    m.edge_count = base.metrics.edge_count + tool.metrics.edge_count;
    return store_mock_solid(m);
}

uint64_t occt_bridge_boolean_intersect(uint64_t base_handle, uint64_t tool_handle) {
    const BridgeError error("boolean_intersect");
    MockSolidRecord base{}, tool{};
    if (!get_mock_solid(base_handle, base) || !get_mock_solid(tool_handle, tool)) {
        return error.no_handle(-1, "Invalid base or tool shape handle");
    }
    OcctBridgeMetrics m{};
    for (int i = 0; i < 3; ++i) {
        m.bbox.min[i] = std::max(base.metrics.bbox.min[i], tool.metrics.bbox.min[i]);
        m.bbox.max[i] = std::min(base.metrics.bbox.max[i], tool.metrics.bbox.max[i]);
        if (m.bbox.min[i] >= m.bbox.max[i]) {
            return error.no_handle(-3, "Shapes do not overlap");
        }
    }
    const double bbox_vol = intersection_volume_bbox(base.metrics.bbox, tool.metrics.bbox);
    m.watertight = (base.metrics.watertight != 0 && tool.metrics.watertight != 0) ? 1 : 0;
    m.volume_mm3 = std::min({base.metrics.volume_mm3, tool.metrics.volume_mm3, bbox_vol});
    m.face_count = std::max(6, std::min(base.metrics.face_count, tool.metrics.face_count));
    m.edge_count = std::max(12, std::min(base.metrics.edge_count, tool.metrics.edge_count));
    return store_mock_solid(m);
}

int32_t occt_bridge_read_through_holes(uint64_t, int32_t, const int32_t*, double*) {
    const BridgeError error("read_through_holes");
    return error.fail(-4, "OCCT backend not enabled");
}

int32_t occt_bridge_offset_through_hole(uint64_t, int32_t, double, int32_t, int32_t*, uint64_t*) {
    const BridgeError error("offset_through_hole");
    return error.fail(-4, "OCCT backend not enabled");
}

int32_t occt_bridge_move_through_hole(uint64_t, int32_t, const double*, int32_t, int32_t*, uint64_t*) {
    const BridgeError error("move_through_hole");
    return error.fail(-4, "OCCT backend not enabled");
}

int32_t occt_bridge_delete_through_hole(uint64_t, int32_t, int32_t, int32_t*, uint64_t*) {
    const BridgeError error("delete_through_hole");
    return error.fail(-4, "OCCT backend not enabled");
}

int32_t occt_bridge_has_exact_interference() { return 0; }

int32_t occt_bridge_check_interference(uint64_t, uint64_t, double min_clearance_mm,
                                      double* out_measurement) {
    const BridgeError error("check_interference");
    if (!out_measurement || !std::isfinite(min_clearance_mm) || min_clearance_mm < 0.0) return error.fail(-1, "Invalid minimum clearance or missing measurement output");
    return error.fail(-4, "OCCT backend not enabled"); // Never substitute the mock bounding-box Boolean for exact geometry.
}

int32_t occt_bridge_measure_symmetric_difference(uint64_t, uint64_t,
                                                  OcctBridgeSymmetricDifference* out) {
    const BridgeError error("measure_symmetric_difference");
    if (!out) return error.fail(-1, "Missing symmetric difference output");
    return error.fail(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_fillet_all(uint64_t handle, double radius) {
    const BridgeError error("fillet_all");
    if (handle == 0 || !std::isfinite(radius) || radius <= 0.0) {
        return error.no_handle(-1, "Invalid shape handle or fillet radius");
    }
    MockSolidRecord rec{};
    if (!get_mock_solid(handle, rec)) {
        return error.no_handle(-1, "Invalid shape handle");
    }
    const double min_ext = min_bbox_extent(rec.metrics.bbox);
    if (radius >= 0.5 * min_ext) {
        return error.no_handle(-3, "Fillet radius is too large for the shape");
    }
    OcctBridgeMetrics m = rec.metrics;
    const double removed = std::min(
        0.25 * rec.metrics.volume_mm3,
        0.2146 * radius * radius * min_ext * static_cast<double>(rec.metrics.edge_count));
    m.volume_mm3 = std::max(1.0e-3, rec.metrics.volume_mm3 - removed);
    m.face_count = rec.metrics.face_count + rec.metrics.edge_count;
    m.edge_count = rec.metrics.edge_count * 2;
    return store_mock_solid(m);
}

uint64_t occt_bridge_chamfer_all(uint64_t handle, double distance) {
    const BridgeError error("chamfer_all");
    if (handle == 0 || !std::isfinite(distance) || distance <= 0.0) {
        return error.no_handle(-1, "Invalid shape handle or chamfer distance");
    }
    MockSolidRecord rec{};
    if (!get_mock_solid(handle, rec)) {
        return error.no_handle(-1, "Invalid shape handle");
    }
    const double min_ext = min_bbox_extent(rec.metrics.bbox);
    if (distance >= 0.5 * min_ext) {
        return error.no_handle(-3, "Chamfer distance is too large for the shape");
    }
    OcctBridgeMetrics m = rec.metrics;
    const double removed = std::min(
        0.25 * rec.metrics.volume_mm3,
        0.5 * distance * distance * min_ext * static_cast<double>(rec.metrics.edge_count));
    m.volume_mm3 = std::max(1.0e-3, rec.metrics.volume_mm3 - removed);
    m.face_count = rec.metrics.face_count + rec.metrics.edge_count;
    m.edge_count = rec.metrics.edge_count * 2;
    return store_mock_solid(m);
}

// Edge-indexed dress-ups need real B-Rep topology; the mock has none.
uint64_t occt_bridge_fillet_edges(uint64_t, double, const int32_t*, int32_t) {
    const BridgeError error("fillet_edges");
    return error.no_handle(-4, "Edge-indexed fillet requires the native OCCT backend");
}

uint64_t occt_bridge_chamfer_edges(uint64_t, double, const int32_t*, int32_t) {
    const BridgeError error("chamfer_edges");
    return error.no_handle(-4, "Edge-indexed chamfer requires the native OCCT backend");
}

uint64_t occt_bridge_shell(
    uint64_t handle,
    double thickness,
    double open_nx,
    double open_ny,
    double open_nz) {
    const BridgeError error("shell");
    if (handle == 0 || !std::isfinite(thickness) || thickness <= 0.0 ||
        !std::isfinite(open_nx) || !std::isfinite(open_ny) || !std::isfinite(open_nz)) {
        return error.no_handle(-1, "Invalid shape handle or shell thickness");
    }
    const double norm_sq = open_nx * open_nx + open_ny * open_ny + open_nz * open_nz;
    if (!std::isfinite(norm_sq) || norm_sq <= 1.0e-12) {
        return error.no_handle(-1, "Shell opening direction must have a finite nonzero length");
    }
    MockSolidRecord rec{};
    if (!get_mock_solid(handle, rec)) {
        return error.no_handle(-1, "Invalid shape handle");
    }
    const double min_ext = min_bbox_extent(rec.metrics.bbox);
    if (thickness >= 0.5 * min_ext) {
        return error.no_handle(-3, "Shell thickness is too large for the shape");
    }
    const double dx = rec.metrics.bbox.max[0] - rec.metrics.bbox.min[0];
    const double dy = rec.metrics.bbox.max[1] - rec.metrics.bbox.min[1];
    const double dz = rec.metrics.bbox.max[2] - rec.metrics.bbox.min[2];
    const double inner_dx = std::max(0.0, dx - 2.0 * thickness);
    const double inner_dy = std::max(0.0, dy - 2.0 * thickness);
    const double inner_dz = std::max(0.0, dz - thickness);
    const double cavity_vol = std::min(
        0.9 * rec.metrics.volume_mm3,
        inner_dx * inner_dy * inner_dz);

    OcctBridgeMetrics m = rec.metrics;
    m.volume_mm3 = std::max(1.0e-3, rec.metrics.volume_mm3 - cavity_vol);
    m.face_count = rec.metrics.face_count * 2 - 1;
    m.edge_count = rec.metrics.edge_count * 2;
    return store_mock_solid(m);
}

uint64_t occt_bridge_extrude_polygon(
    int plane_code,
    const double* xy_pairs,
    int point_count,
    double height,
    int symmetric) {
    const BridgeError error("extrude_polygon");
    double signed_area = 0.0;
    if (!is_valid_polygon_params(plane_code, xy_pairs, point_count, height, &signed_area)) {
        return error.no_handle(-1, "Invalid extrusion plane, polygon, or height");
    }
    double u_min = xy_pairs[0];
    double u_max = xy_pairs[0];
    double v_min = xy_pairs[1];
    double v_max = xy_pairs[1];
    for (int i = 1; i < point_count; ++i) {
        const double u = xy_pairs[2 * i];
        const double v = xy_pairs[2 * i + 1];
        u_min = std::min(u_min, u);
        u_max = std::max(u_max, u);
        v_min = std::min(v_min, v);
        v_max = std::max(v_max, v);
    }

    const double w_start = (symmetric != 0) ? (-0.5 * height) : 0.0;
    const double w_end = (symmetric != 0) ? (0.5 * height) : height;
    const double w_min = std::min(w_start, w_end);
    const double w_max = std::max(w_start, w_end);

    OcctBridgeMetrics m{};
    m.watertight = 1;
    m.volume_mm3 = std::abs(signed_area * height);
    if (plane_code == 1) { // XZ
        m.bbox.min[0] = u_min;
        m.bbox.max[0] = u_max;
        m.bbox.min[1] = w_min;
        m.bbox.max[1] = w_max;
        m.bbox.min[2] = v_min;
        m.bbox.max[2] = v_max;
    } else if (plane_code == 2) { // YZ
        m.bbox.min[0] = w_min;
        m.bbox.max[0] = w_max;
        m.bbox.min[1] = u_min;
        m.bbox.max[1] = u_max;
        m.bbox.min[2] = v_min;
        m.bbox.max[2] = v_max;
    } else { // XY
        m.bbox.min[0] = u_min;
        m.bbox.max[0] = u_max;
        m.bbox.min[1] = v_min;
        m.bbox.max[1] = v_max;
        m.bbox.min[2] = w_min;
        m.bbox.max[2] = w_max;
    }
    m.face_count = point_count + 2;
    m.edge_count = point_count * 3;
    return store_mock_solid(m);
}

int32_t occt_bridge_check(uint64_t handle, OcctBridgeMetrics* out_metrics) {
    const BridgeError error("check");
    if (!out_metrics) {
        return error.fail(-1, "Missing metrics output");
    }
    MockSolidRecord rec{};
    if (!get_mock_solid(handle, rec)) {
        return error.fail(-2, "Invalid shape handle");
    }
    *out_metrics = rec.metrics;
    return 0;
}

static int32_t export_step_impl(const BridgeError& error, uint64_t handle,
                                const char* path_utf8, const char* name_utf8);

int32_t occt_bridge_export_step(uint64_t handle, const char* path_utf8) {
    const BridgeError error("export_step");
    return export_step_impl(error, handle, path_utf8, nullptr);
}

int32_t occt_bridge_volume_centroid(uint64_t, double out_mm[3]) {
    const BridgeError error("volume_centroid");
    if (!out_mm) return error.fail(-1, "Missing centroid output");
    return error.fail(-2, "OCCT backend not enabled");
}

int32_t occt_bridge_export_step_named(uint64_t handle, const char* path_utf8, const char* name_utf8) {
    const BridgeError error("export_step_named");
    return export_step_impl(error, handle, path_utf8, name_utf8);
}

static int32_t export_step_impl(const BridgeError& error, uint64_t handle,
                                const char* path_utf8, const char*) {
    if (!path_utf8) {
        return error.fail(-1, "Missing STEP output path");
    }
    MockSolidRecord rec{};
    if (!get_mock_solid(handle, rec)) {
        return error.fail(-2, "Invalid shape handle");
    }
    std::ofstream out(path_utf8, std::ios::out | std::ios::trunc);
    if (!out.is_open()) {
        return error.fail(-4, "Could not open STEP output file");
    }
    out << "ISO-10303-21;\n"
        << "HEADER;\n"
        << "FILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF'));\n"
        << "ENDSEC;\n"
        << "DATA;\n"
        << "ENDSEC;\n"
        << "END-ISO-10303-21;\n";
    return out.good() ? OCCT_OK : error.fail(-4, "Could not write STEP output file");
}

int32_t occt_bridge_export_step_assembly(int32_t, const uint64_t*, const char* const*, const char*,
                                         const char*) {
    const BridgeError error("export_step_assembly");
    return error.fail(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_import_file(const char*, int32_t, const OcctImportOptions*,
                                 OcctImportMetadata* out_meta, char* err_buf, int32_t err_buf_len) {
    const BridgeError error("import_file");
    if (out_meta) *out_meta = {};
    report_no_occt(err_buf, err_buf_len);
    return error.no_handle(-4, "OCCT support is unavailable");
}

int32_t occt_bridge_get_components(uint64_t, OcctComponentDescriptor*, int32_t) {
    const BridgeError error("get_components");
    return error.fail(-2, "OCCT backend not enabled");
}
int32_t occt_bridge_get_component_prototypes(uint64_t, int32_t*, int32_t) {
    const BridgeError error("get_component_prototypes");
    return error.fail(-2, "OCCT backend not enabled");
}

int32_t occt_bridge_mesh_and_query_counts(uint64_t, const OcctImportOptions*, OcctTessellationCounts*) {
    const BridgeError error("mesh_and_query_counts");
    return error.fail(-2, "OCCT backend not enabled");
}

int32_t occt_bridge_extract_tessellation(uint64_t, int32_t, uint32_t, uint32_t, OcctFaceDescriptor*,
                                         double*, double*, uint32_t*) {
    const BridgeError error("extract_tessellation");
    return error.fail(-2, "OCCT backend not enabled");
}

uint64_t occt_bridge_transform(uint64_t, const double[16]) {
    const BridgeError error("transform");
    return error.no_handle(-4, "OCCT backend not enabled");
}

uint64_t occt_bridge_revolve_polygon(int, const double*, int, const double[3], const double[3], double) {
    const BridgeError error("revolve_polygon");
    return error.no_handle(-4, "OCCT backend not enabled");
}

void occt_bridge_free(uint64_t handle) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_registry.erase(handle);
}

#endif

int32_t occt_bridge_extract_topology(uint64_t handle, OcctTopologyResult* out) {
    const BridgeError error("extract_topology");
    if (!out) return error.fail(-1, "Missing topology output");
    *out = {};
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-1, "Invalid shape handle");
        occt_topology::extract(record->shape, *out);
        return OCCT_OK;
    } catch (...) {
        return error.exception(-3);
    }
#else
    return error.fail(-4, "OCCT backend not enabled");
#endif
}

void occt_bridge_free_topology(OcctTopologyResult* result) {
    if (!result) return;
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    occt_topology::release(*result);
#else
    *result = {};
#endif
}

int32_t occt_bridge_compute_hlr(uint64_t handle, const double toward_eye[3], const double view_x[3],
                                double deflection_mm, OcctHlrResult* out) {
    const BridgeError error("compute_hlr");
    if (!out) return error.fail(-1, "Missing HLR output");
    *out = {};
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    try {
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const auto record = get_record(handle);
        if (!record) return error.fail(-1, "Invalid shape handle");
        const auto status = occt_hlr::compute(record->shape, toward_eye, view_x, deflection_mm, *out);
        return status == OCCT_OK ? status : error.fail(status, "Invalid HLR view vectors or deflection");
    } catch (...) {
        return error.exception(-3);
    }
#else
    return error.fail(-4, "OCCT backend not enabled");
#endif
}

void occt_bridge_free_hlr(OcctHlrResult* result) {
    if (!result) return;
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    occt_hlr::release(*result);
#else
    *result = {};
#endif
}

} // extern "C"
