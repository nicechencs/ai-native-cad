#pragma once

#include "abi_version.hpp"

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <mutex>
#include <shared_mutex>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

namespace cad_geomd {

class WorkerPool;

using ShapeHandle = uint64_t;
inline constexpr ShapeHandle kInvalidShapeHandle = 0;

enum class BooleanOpKind : int32_t {
    Union = 0,
    Cut = 1,
    Intersect = 2,
};

struct Vec3d {
    double x = 0.0;
    double y = 0.0;
    double z = 0.0;
};

struct Quat4d {
    double x = 0.0;
    double y = 0.0;
    double z = 0.0;
    double w = 1.0;
};

struct Transform3d {
    Vec3d translation_mm{};
    Quat4d rotation_quat_xyzw{};
};

struct BoundingBox3d {
    Vec3d min_mm{};
    Vec3d max_mm{};
};

struct MassProperties {
    double volume_mm3 = 0.0;
    double surface_area_mm2 = 0.0;
    Vec3d centroid_mm{};
    // Inertia tensor relative to centroid divided by material density (mm^5):
    // [Ixx, Iyy, Izz, Ixy, Ixz, Iyz]
    std::array<double, 6> inertia_tensor_unit_density{};
    BoundingBox3d bbox{};
    bool watertight = false;
    uint32_t face_count = 0;
    uint32_t edge_count = 0;
};

struct TessellationParams {
    uint8_t lod_level = 1; // 0=fine, 1=standard, 2=coarse
    double linear_deflection_mm = 0.05;
    double angular_deflection_rad = 0.2;
    uint32_t cylinder_segments = 24;
};

struct TriangleMesh {
    ShapeHandle source_handle = kInvalidShapeHandle;
    std::vector<double> positions_mm; // 3 * vertex_count
    std::vector<double> normals;      // 3 * vertex_count
    std::vector<uint32_t> indices;    // 3 * triangle_count
    uint32_t face_count = 0;

    [[nodiscard]] uint32_t vertex_count() const noexcept {
        return static_cast<uint32_t>(positions_mm.size() / 3);
    }

    [[nodiscard]] uint32_t triangle_count() const noexcept {
        return static_cast<uint32_t>(indices.size() / 3);
    }
};

// Sharded thread-safe handle table replacing any single global kernel mutex.
// Concurrent readers on any handle use shared locks; writers only lock a single shard.
template <typename Entry, std::size_t NumShards = 64>
class ShardedHandleTable {
    static_assert(NumShards > 0, "NumShards must be positive");

public:
    ShapeHandle insert(Entry value) {
        const ShapeHandle handle = next_handle_.fetch_add(1, std::memory_order_relaxed);
        auto& shard = shard_for(handle);
        std::unique_lock<std::shared_mutex> lock(shard.mutex);
        shard.entries.emplace(handle, std::move(value));
        return handle;
    }

    bool find_copy(ShapeHandle handle, Entry& out_entry) const {
        if (handle == kInvalidShapeHandle) {
            return false;
        }
        const auto& shard = shard_for(handle);
        std::shared_lock<std::shared_mutex> lock(shard.mutex);
        const auto it = shard.entries.find(handle);
        if (it == shard.entries.end()) {
            return false;
        }
        out_entry = it->second;
        return true;
    }

    template <typename Fn>
    bool with_read(ShapeHandle handle, Fn&& fn) const {
        if (handle == kInvalidShapeHandle) {
            return false;
        }
        const auto& shard = shard_for(handle);
        std::shared_lock<std::shared_mutex> lock(shard.mutex);
        const auto it = shard.entries.find(handle);
        if (it == shard.entries.end()) {
            return false;
        }
        fn(it->second);
        return true;
    }

    bool erase(ShapeHandle handle) {
        if (handle == kInvalidShapeHandle) {
            return false;
        }
        auto& shard = shard_for(handle);
        std::unique_lock<std::shared_mutex> lock(shard.mutex);
        return shard.entries.erase(handle) > 0;
    }

    std::size_t size() const {
        std::size_t total = 0;
        for (const auto& shard : shards_) {
            std::shared_lock<std::shared_mutex> lock(shard.mutex);
            total += shard.entries.size();
        }
        return total;
    }

    void clear() {
        for (auto& shard : shards_) {
            std::unique_lock<std::shared_mutex> lock(shard.mutex);
            shard.entries.clear();
        }
    }

private:
    struct alignas(64) Shard {
        mutable std::shared_mutex mutex;
        std::unordered_map<ShapeHandle, Entry> entries;
    };

    Shard& shard_for(ShapeHandle handle) noexcept {
        return shards_[static_cast<std::size_t>(handle % NumShards)];
    }

    const Shard& shard_for(ShapeHandle handle) const noexcept {
        return shards_[static_cast<std::size_t>(handle % NumShards)];
    }

    std::atomic<ShapeHandle> next_handle_{1};
    std::array<Shard, NumShards> shards_{};
};

// Pure virtual C++20 B-Rep kernel slot (OCCT 7.9+ / Parasolid / C3D / Analytic reference).
class IBRepKernel {
public:
    virtual ~IBRepKernel() = default;

    [[nodiscard]] virtual KernelCapabilities capabilities() const = 0;

    virtual Status make_box(
        double dx_mm,
        double dy_mm,
        double dz_mm,
        const Vec3d& origin_mm,
        ShapeHandle& out_handle) = 0;

    virtual Status make_cylinder(
        double diameter_mm,
        double height_mm,
        const Vec3d& origin_mm,
        ShapeHandle& out_handle) = 0;

    virtual Status boolean_op(
        ShapeHandle base_handle,
        ShapeHandle tool_handle,
        BooleanOpKind op,
        ShapeHandle& out_handle) = 0;

    virtual Status release_shape(ShapeHandle handle) = 0;

    virtual Status compute_mass_properties(
        ShapeHandle handle,
        MassProperties& out_props) const = 0;

    virtual Status tessellate(
        ShapeHandle handle,
        const TessellationParams& params,
        TriangleMesh& out_mesh) const = 0;

    virtual Status tessellate_parallel(
        std::span<const ShapeHandle> handles,
        const TessellationParams& params,
        WorkerPool& pool,
        std::vector<TriangleMesh>& out_meshes) const = 0;

    virtual Status export_step_ap242(
        ShapeHandle handle,
        std::string_view path_utf8,
        std::string_view product_name_utf8) const = 0;

    virtual Status import_step_ap242(
        std::string_view path_utf8,
        ShapeHandle& out_handle) = 0;

    virtual bool serialize_native_brep(
        ShapeHandle handle,
        std::vector<uint8_t>& out_bytes) const = 0;

    virtual Status deserialize_native_brep(
        std::span<const uint8_t> bytes,
        ShapeHandle& out_handle) = 0;
};

} // namespace cad_geomd
