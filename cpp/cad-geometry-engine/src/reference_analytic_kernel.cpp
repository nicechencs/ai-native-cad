#include "reference_analytic_kernel.hpp"
#include "worker_pool.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <limits>
#include <thread>

namespace cad_geomd {

namespace {

constexpr double kPi = 3.14159265358979323846;
constexpr uint32_t kNativeAnalyticMagic = 0x424C4E41u; // "ANLB"
constexpr uint32_t kNativeAnalyticVersion = 1u;

bool is_finite_positive(double v) noexcept {
    return std::isfinite(v) && v > 0.0;
}

bool is_finite_vec3(const Vec3d& v) noexcept {
    return std::isfinite(v.x) && std::isfinite(v.y) && std::isfinite(v.z);
}

void push_quad(
    TriangleMesh& mesh,
    const Vec3d& p0,
    const Vec3d& p1,
    const Vec3d& p2,
    const Vec3d& p3,
    const Vec3d& normal) {
    const uint32_t base = mesh.vertex_count();
    const Vec3d pts[4] = {p0, p1, p2, p3};
    for (const auto& p : pts) {
        mesh.positions_mm.push_back(p.x);
        mesh.positions_mm.push_back(p.y);
        mesh.positions_mm.push_back(p.z);
        mesh.normals.push_back(normal.x);
        mesh.normals.push_back(normal.y);
        mesh.normals.push_back(normal.z);
    }
    mesh.indices.push_back(base + 0u);
    mesh.indices.push_back(base + 1u);
    mesh.indices.push_back(base + 2u);
    mesh.indices.push_back(base + 0u);
    mesh.indices.push_back(base + 2u);
    mesh.indices.push_back(base + 3u);
}

uint32_t resolve_cylinder_segments(const TessellationParams& params) noexcept {
    uint32_t segs = params.cylinder_segments;
    if (segs == 0u) {
        if (params.lod_level == 0u) {
            segs = 48u;
        } else if (params.lod_level == 2u) {
            segs = 12u;
        } else {
            segs = 24u;
        }
    }
    return std::clamp(segs, 8u, 256u);
}

template <typename T>
void append_pod(std::vector<uint8_t>& dst, const T& value) {
    const auto* raw = reinterpret_cast<const uint8_t*>(&value);
    dst.insert(dst.end(), raw, raw + sizeof(T));
}

template <typename T>
bool read_pod(std::span<const uint8_t> src, std::size_t& offset, T& out) {
    if (offset + sizeof(T) > src.size()) {
        return false;
    }
    std::memcpy(&out, src.data() + offset, sizeof(T));
    offset += sizeof(T);
    return true;
}

} // namespace

ReferenceAnalyticKernel::ReferenceAnalyticKernel(uint32_t max_workers) {
    if (max_workers == 0u) {
        const unsigned int hw = std::thread::hardware_concurrency();
        max_workers_ = std::clamp(hw == 0u ? 4u : static_cast<uint32_t>(hw), 1u, 16u);
    } else {
        max_workers_ = max_workers;
    }
}

KernelCapabilities ReferenceAnalyticKernel::capabilities() const {
    KernelCapabilities caps{};
    caps.kernel_name = "reference-analytic";
    caps.kernel_version = "1.0.0";
    caps.abi_major = CAD_GEOMD_ABI_MAJOR;
    caps.abi_minor = CAD_GEOMD_ABI_MINOR;
    caps.authoritative_brep = false;
    caps.supports_parallel_tessellation = true;
    caps.supports_step_ap242 = false;
    caps.supports_exact_booleans = false;
    caps.supports_two_stage_assembly = true;
    caps.max_worker_threads = max_workers_;
    return caps;
}

Status ReferenceAnalyticKernel::make_box(
    double dx_mm,
    double dy_mm,
    double dz_mm,
    const Vec3d& origin_mm,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
    if (!is_finite_positive(dx_mm) || !is_finite_positive(dy_mm) || !is_finite_positive(dz_mm) ||
        !is_finite_vec3(origin_mm)) {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "make_box requires finite positive dimensions (dx, dy, dz) and finite origin");
    }

    AnalyticShapeRecord rec{};
    rec.kind = PrimitiveKind::Box;
    rec.origin_mm = origin_mm;
    rec.p0 = dx_mm;
    rec.p1 = dy_mm;
    rec.p2 = dz_mm;
    out_handle = shapes_.insert(rec);
    return Status::Ok();
}

Status ReferenceAnalyticKernel::make_cylinder(
    double diameter_mm,
    double height_mm,
    const Vec3d& origin_mm,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
    if (!is_finite_positive(diameter_mm) || !is_finite_positive(height_mm) ||
        !is_finite_vec3(origin_mm)) {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "make_cylinder requires finite positive diameter/height and finite origin");
    }

    AnalyticShapeRecord rec{};
    rec.kind = PrimitiveKind::Cylinder;
    rec.origin_mm = origin_mm;
    rec.p0 = diameter_mm;
    rec.p1 = height_mm;
    rec.p2 = 0.0;
    out_handle = shapes_.insert(rec);
    return Status::Ok();
}

Status ReferenceAnalyticKernel::boolean_op(
    ShapeHandle base_handle,
    ShapeHandle tool_handle,
    BooleanOpKind op,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
    MassProperties base_props{};
    MassProperties tool_props{};
    const Status s1 = compute_mass_properties(base_handle, base_props);
    if (!s1.ok()) {
        return s1;
    }
    const Status s2 = compute_mass_properties(tool_handle, tool_props);
    if (!s2.ok()) {
        return s2;
    }

    BoundingBox3d result_bbox{};
    if (op == BooleanOpKind::Union) {
        result_bbox.min_mm.x = std::min(base_props.bbox.min_mm.x, tool_props.bbox.min_mm.x);
        result_bbox.min_mm.y = std::min(base_props.bbox.min_mm.y, tool_props.bbox.min_mm.y);
        result_bbox.min_mm.z = std::min(base_props.bbox.min_mm.z, tool_props.bbox.min_mm.z);
        result_bbox.max_mm.x = std::max(base_props.bbox.max_mm.x, tool_props.bbox.max_mm.x);
        result_bbox.max_mm.y = std::max(base_props.bbox.max_mm.y, tool_props.bbox.max_mm.y);
        result_bbox.max_mm.z = std::max(base_props.bbox.max_mm.z, tool_props.bbox.max_mm.z);
    } else if (op == BooleanOpKind::Intersect) {
        result_bbox.min_mm.x = std::max(base_props.bbox.min_mm.x, tool_props.bbox.min_mm.x);
        result_bbox.min_mm.y = std::max(base_props.bbox.min_mm.y, tool_props.bbox.min_mm.y);
        result_bbox.min_mm.z = std::max(base_props.bbox.min_mm.z, tool_props.bbox.min_mm.z);
        result_bbox.max_mm.x = std::min(base_props.bbox.max_mm.x, tool_props.bbox.max_mm.x);
        result_bbox.max_mm.y = std::min(base_props.bbox.max_mm.y, tool_props.bbox.max_mm.y);
        result_bbox.max_mm.z = std::min(base_props.bbox.max_mm.z, tool_props.bbox.max_mm.z);
        if (result_bbox.max_mm.x <= result_bbox.min_mm.x ||
            result_bbox.max_mm.y <= result_bbox.min_mm.y ||
            result_bbox.max_mm.z <= result_bbox.min_mm.z) {
            return Status::Error(
                ErrorCode::GeometryFailure,
                "boolean intersection produced empty volume");
        }
    } else {
        // Cut: retain base envelope for non-authoritative analytic reference.
        result_bbox = base_props.bbox;
    }

    return make_box(
        result_bbox.max_mm.x - result_bbox.min_mm.x,
        result_bbox.max_mm.y - result_bbox.min_mm.y,
        result_bbox.max_mm.z - result_bbox.min_mm.z,
        result_bbox.min_mm,
        out_handle);
}

Status ReferenceAnalyticKernel::release_shape(ShapeHandle handle) {
    if (!shapes_.erase(handle)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    return Status::Ok();
}

Status ReferenceAnalyticKernel::compute_mass_properties(
    ShapeHandle handle,
    MassProperties& out_props) const {
    AnalyticShapeRecord rec{};
    if (!shapes_.find_copy(handle, rec)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }

    out_props = MassProperties{};
    out_props.watertight = true;

    if (rec.kind == PrimitiveKind::Box) {
        const double dx = rec.p0;
        const double dy = rec.p1;
        const double dz = rec.p2;
        const double vol = dx * dy * dz;
        out_props.volume_mm3 = vol;
        out_props.surface_area_mm2 = 2.0 * (dx * dy + dy * dz + dz * dx);
        out_props.centroid_mm = {
            rec.origin_mm.x + 0.5 * dx,
            rec.origin_mm.y + 0.5 * dy,
            rec.origin_mm.z + 0.5 * dz,
        };
        out_props.inertia_tensor_unit_density = {
            vol * (dy * dy + dz * dz) / 12.0,
            vol * (dx * dx + dz * dz) / 12.0,
            vol * (dx * dx + dy * dy) / 12.0,
            0.0,
            0.0,
            0.0,
        };
        out_props.bbox.min_mm = rec.origin_mm;
        out_props.bbox.max_mm = {
            rec.origin_mm.x + dx,
            rec.origin_mm.y + dy,
            rec.origin_mm.z + dz,
        };
        out_props.face_count = 6u;
        out_props.edge_count = 12u;
        return Status::Ok();
    }

    const double r = 0.5 * rec.p0;
    const double h = rec.p1;
    const double vol = kPi * r * r * h;
    out_props.volume_mm3 = vol;
    out_props.surface_area_mm2 = 2.0 * kPi * r * (r + h);
    out_props.centroid_mm = {
        rec.origin_mm.x,
        rec.origin_mm.y,
        rec.origin_mm.z + 0.5 * h,
    };
    const double i_radial = vol * (3.0 * r * r + h * h) / 12.0;
    const double i_axial = 0.5 * vol * r * r;
    out_props.inertia_tensor_unit_density = {
        i_radial,
        i_radial,
        i_axial,
        0.0,
        0.0,
        0.0,
    };
    out_props.bbox.min_mm = {
        rec.origin_mm.x - r,
        rec.origin_mm.y - r,
        rec.origin_mm.z,
    };
    out_props.bbox.max_mm = {
        rec.origin_mm.x + r,
        rec.origin_mm.y + r,
        rec.origin_mm.z + h,
    };
    out_props.face_count = 3u;
    out_props.edge_count = 3u;
    return Status::Ok();
}

Status ReferenceAnalyticKernel::tessellate(
    ShapeHandle handle,
    const TessellationParams& params,
    TriangleMesh& out_mesh) const {
    AnalyticShapeRecord rec{};
    if (!shapes_.find_copy(handle, rec)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }

    out_mesh = TriangleMesh{};
    out_mesh.source_handle = handle;

    if (rec.kind == PrimitiveKind::Box) {
        const double x0 = rec.origin_mm.x;
        const double y0 = rec.origin_mm.y;
        const double z0 = rec.origin_mm.z;
        const double x1 = x0 + rec.p0;
        const double y1 = y0 + rec.p1;
        const double z1 = z0 + rec.p2;

        out_mesh.positions_mm.reserve(24u * 3u);
        out_mesh.normals.reserve(24u * 3u);
        out_mesh.indices.reserve(12u * 3u);
        out_mesh.face_count = 6u;

        // -Z face
        push_quad(out_mesh, {x0, y0, z0}, {x0, y1, z0}, {x1, y1, z0}, {x1, y0, z0}, {0.0, 0.0, -1.0});
        // +Z face
        push_quad(out_mesh, {x0, y0, z1}, {x1, y0, z1}, {x1, y1, z1}, {x0, y1, z1}, {0.0, 0.0, 1.0});
        // -Y face
        push_quad(out_mesh, {x0, y0, z0}, {x1, y0, z0}, {x1, y0, z1}, {x0, y0, z1}, {0.0, -1.0, 0.0});
        // +Y face
        push_quad(out_mesh, {x0, y1, z0}, {x0, y1, z1}, {x1, y1, z1}, {x1, y1, z0}, {0.0, 1.0, 0.0});
        // -X face
        push_quad(out_mesh, {x0, y0, z0}, {x0, y0, z1}, {x0, y1, z1}, {x0, y1, z0}, {-1.0, 0.0, 0.0});
        // +X face
        push_quad(out_mesh, {x1, y0, z0}, {x1, y1, z0}, {x1, y1, z1}, {x1, y0, z1}, {1.0, 0.0, 0.0});
        return Status::Ok();
    }

    const uint32_t segs = resolve_cylinder_segments(params);
    const double r = 0.5 * rec.p0;
    const double h = rec.p1;
    const double ox = rec.origin_mm.x;
    const double oy = rec.origin_mm.y;
    const double z0 = rec.origin_mm.z;
    const double z1 = z0 + h;

    out_mesh.face_count = 3u;
    out_mesh.positions_mm.reserve(static_cast<std::size_t>(segs * 4u + 2u) * 3u);
    out_mesh.normals.reserve(static_cast<std::size_t>(segs * 4u + 2u) * 3u);
    out_mesh.indices.reserve(static_cast<std::size_t>(segs * 4u) * 3u);

    // Side surface
    for (uint32_t i = 0; i < segs; ++i) {
        const double a0 = (2.0 * kPi * static_cast<double>(i)) / static_cast<double>(segs);
        const double a1 = (2.0 * kPi * static_cast<double>(i + 1u)) / static_cast<double>(segs);
        const double c0 = std::cos(a0);
        const double s0 = std::sin(a0);
        const double c1 = std::cos(a1);
        const double s1 = std::sin(a1);
        const double am = 0.5 * (a0 + a1);
        push_quad(
            out_mesh,
            {ox + r * c0, oy + r * s0, z0},
            {ox + r * c1, oy + r * s1, z0},
            {ox + r * c1, oy + r * s1, z1},
            {ox + r * c0, oy + r * s0, z1},
            {std::cos(am), std::sin(am), 0.0});
    }

    // Bottom cap (-Z)
    const uint32_t bot_center = out_mesh.vertex_count();
    out_mesh.positions_mm.insert(out_mesh.positions_mm.end(), {ox, oy, z0});
    out_mesh.normals.insert(out_mesh.normals.end(), {0.0, 0.0, -1.0});
    for (uint32_t i = 0; i < segs; ++i) {
        const double a = (2.0 * kPi * static_cast<double>(i)) / static_cast<double>(segs);
        out_mesh.positions_mm.insert(out_mesh.positions_mm.end(), {ox + r * std::cos(a), oy + r * std::sin(a), z0});
        out_mesh.normals.insert(out_mesh.normals.end(), {0.0, 0.0, -1.0});
    }
    for (uint32_t i = 0; i < segs; ++i) {
        const uint32_t cur = bot_center + 1u + i;
        const uint32_t nxt = bot_center + 1u + ((i + 1u) % segs);
        out_mesh.indices.push_back(bot_center);
        out_mesh.indices.push_back(nxt);
        out_mesh.indices.push_back(cur);
    }

    // Top cap (+Z)
    const uint32_t top_center = out_mesh.vertex_count();
    out_mesh.positions_mm.insert(out_mesh.positions_mm.end(), {ox, oy, z1});
    out_mesh.normals.insert(out_mesh.normals.end(), {0.0, 0.0, 1.0});
    for (uint32_t i = 0; i < segs; ++i) {
        const double a = (2.0 * kPi * static_cast<double>(i)) / static_cast<double>(segs);
        out_mesh.positions_mm.insert(out_mesh.positions_mm.end(), {ox + r * std::cos(a), oy + r * std::sin(a), z1});
        out_mesh.normals.insert(out_mesh.normals.end(), {0.0, 0.0, 1.0});
    }
    for (uint32_t i = 0; i < segs; ++i) {
        const uint32_t cur = top_center + 1u + i;
        const uint32_t nxt = top_center + 1u + ((i + 1u) % segs);
        out_mesh.indices.push_back(top_center);
        out_mesh.indices.push_back(cur);
        out_mesh.indices.push_back(nxt);
    }

    return Status::Ok();
}

Status ReferenceAnalyticKernel::tessellate_parallel(
    std::span<const ShapeHandle> handles,
    const TessellationParams& params,
    WorkerPool& pool,
    std::vector<TriangleMesh>& out_meshes) const {
    return pool.tessellate_batch(*this, handles, params, out_meshes, nullptr);
}

Status ReferenceAnalyticKernel::export_step_ap242(
    ShapeHandle handle,
    std::string_view path_utf8,
    std::string_view product_name_utf8) const {
    (void)product_name_utf8;
    AnalyticShapeRecord rec{};
    if (!shapes_.find_copy(handle, rec)) {
        return Status::Error(ErrorCode::InvalidHandle, "unknown shape handle");
    }
    if (path_utf8.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "export path must not be empty");
    }
    return Status::Error(
        ErrorCode::KernelUnavailable,
        "authoritative STEP AP242 export requires CAD_GEOMD_WITH_OCCT=ON (OCCT 7.9+ kernel plugin)");
}

Status ReferenceAnalyticKernel::import_step_ap242(
    std::string_view path_utf8,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
    if (path_utf8.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "import path must not be empty");
    }
    return Status::Error(
        ErrorCode::KernelUnavailable,
        "authoritative STEP AP242 import requires CAD_GEOMD_WITH_OCCT=ON (OCCT 7.9+ kernel plugin)");
}

bool ReferenceAnalyticKernel::serialize_native_brep(
    ShapeHandle handle,
    std::vector<uint8_t>& out_bytes) const {
    AnalyticShapeRecord rec{};
    if (!shapes_.find_copy(handle, rec)) {
        return false;
    }
    out_bytes.clear();
    out_bytes.reserve(sizeof(uint32_t) * 2u + sizeof(AnalyticShapeRecord));
    append_pod(out_bytes, kNativeAnalyticMagic);
    append_pod(out_bytes, kNativeAnalyticVersion);
    append_pod(out_bytes, rec);
    return true;
}

Status ReferenceAnalyticKernel::deserialize_native_brep(
    std::span<const uint8_t> bytes,
    ShapeHandle& out_handle) {
    out_handle = kInvalidShapeHandle;
    std::size_t offset = 0;
    uint32_t magic = 0;
    uint32_t version = 0;
    AnalyticShapeRecord rec{};
    if (!read_pod(bytes, offset, magic) || !read_pod(bytes, offset, version) ||
        !read_pod(bytes, offset, rec)) {
        return Status::Error(ErrorCode::InvalidArgument, "truncated native analytic brep buffer");
    }
    if (magic != kNativeAnalyticMagic || version != kNativeAnalyticVersion) {
        return Status::Error(ErrorCode::InvalidArgument, "invalid native analytic brep header");
    }
    if (rec.kind == PrimitiveKind::Box) {
        return make_box(rec.p0, rec.p1, rec.p2, rec.origin_mm, out_handle);
    }
    if (rec.kind == PrimitiveKind::Cylinder) {
        return make_cylinder(rec.p0, rec.p1, rec.origin_mm, out_handle);
    }
    return Status::Error(ErrorCode::InvalidArgument, "unknown analytic primitive kind");
}

std::size_t ReferenceAnalyticKernel::live_shape_count() const {
    return shapes_.size();
}

} // namespace cad_geomd
