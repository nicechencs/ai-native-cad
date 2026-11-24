#pragma once

#include "ibrep_kernel.hpp"

#include <cstddef>
#include <cstdint>
#include <memory>
#include <span>
#include <string_view>
#include <vector>

namespace cad_geomd {

enum class KernelBackendKind : int32_t {
    Auto = 0,
    Analytic = 1,
    Occt = 2,
};

[[nodiscard]] bool is_occt_plugin_compiled() noexcept;

[[nodiscard]] std::string_view kernel_backend_kind_name(KernelBackendKind kind) noexcept;

[[nodiscard]] Status parse_kernel_backend_kind(
    std::string_view name,
    KernelBackendKind& out_kind);

// Optional OpenCASCADE (OCCT 7.9+) B-Rep kernel plugin implementing IBRepKernel.
// When CAD_GEOMD_WITH_OCCT=ON (CAD_GEOMD_HAS_OCCT=1), delegates to real OCCT
// modeling/boolean/tessellation/STEP APIs backed by a sharded handle table.
// When CAD_GEOMD_WITH_OCCT=OFF (CAD_GEOMD_HAS_OCCT=0), compiles with zero external
// dependencies and honestly reports unavailable capabilities + KernelUnavailable errors.
class OcctKernelPlugin final : public IBRepKernel {
public:
    explicit OcctKernelPlugin(uint32_t max_workers = 0);
    ~OcctKernelPlugin() override;

    OcctKernelPlugin(const OcctKernelPlugin&) = delete;
    OcctKernelPlugin& operator=(const OcctKernelPlugin&) = delete;
    OcctKernelPlugin(OcctKernelPlugin&&) noexcept;
    OcctKernelPlugin& operator=(OcctKernelPlugin&&) noexcept;

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
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

// Factory for pluggable IBRepKernel backend selection (analytic / occt / auto).
[[nodiscard]] std::unique_ptr<IBRepKernel> create_kernel_backend(
    KernelBackendKind kind,
    uint32_t max_workers = 0);

} // namespace cad_geomd
