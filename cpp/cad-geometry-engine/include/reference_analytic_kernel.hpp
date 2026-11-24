#pragma once

#include "ibrep_kernel.hpp"

#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

namespace cad_geomd {

// Zero-external-dependency reference analytic kernel implementing IBRepKernel
// with sharded lock-free-read handle storage. Explicitly reports
// authoritative_brep = false to preserve zero-trust verification semantics.
class ReferenceAnalyticKernel final : public IBRepKernel {
public:
    explicit ReferenceAnalyticKernel(uint32_t max_workers = 0);
    ~ReferenceAnalyticKernel() override = default;

    [[nodiscard]] KernelCapabilities capabilities() const override;

    Status make_box(
        double dx_mm,
        double dy_mm,
        double dz_mm,
        const Vec3d& origin_mm,
        ShapeHandle& out_handle) override;

    Status make_cylinder(
        double diameter_mm,
        double height_mm,
        const Vec3d& origin_mm,
        ShapeHandle& out_handle) override;

    Status boolean_op(
        ShapeHandle base_handle,
        ShapeHandle tool_handle,
        BooleanOpKind op,
        ShapeHandle& out_handle) override;

    Status release_shape(ShapeHandle handle) override;

    Status compute_mass_properties(
        ShapeHandle handle,
        MassProperties& out_props) const override;

    Status tessellate(
        ShapeHandle handle,
        const TessellationParams& params,
        TriangleMesh& out_mesh) const override;

    Status tessellate_parallel(
        std::span<const ShapeHandle> handles,
        const TessellationParams& params,
        WorkerPool& pool,
        std::vector<TriangleMesh>& out_meshes) const override;

    Status export_step_ap242(
        ShapeHandle handle,
        std::string_view path_utf8,
        std::string_view product_name_utf8) const override;

    Status import_step_ap242(
        std::string_view path_utf8,
        ShapeHandle& out_handle) override;

    bool serialize_native_brep(
        ShapeHandle handle,
        std::vector<uint8_t>& out_bytes) const override;

    Status deserialize_native_brep(
        std::span<const uint8_t> bytes,
        ShapeHandle& out_handle) override;

    [[nodiscard]] std::size_t live_shape_count() const;

private:
    enum class PrimitiveKind : uint8_t {
        Box = 1,
        Cylinder = 2,
    };

    struct AnalyticShapeRecord {
        PrimitiveKind kind = PrimitiveKind::Box;
        Vec3d origin_mm{};
        // Box: (dx, dy, dz). Cylinder: (diameter, height, 0).
        double p0 = 0.0;
        double p1 = 0.0;
        double p2 = 0.0;
    };

    uint32_t max_workers_ = 1;
    ShardedHandleTable<AnalyticShapeRecord, 64> shapes_;
};

} // namespace cad_geomd
