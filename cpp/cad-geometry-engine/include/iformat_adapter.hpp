#pragma once

#include "abi_version.hpp"
#include "ibrep_kernel.hpp"

#include <cstdint>
#include <limits>
#include <memory>
#include <shared_mutex>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace cad_geomd {

enum class AdapterTier : uint32_t {
    Tier0BuiltIn = 0,
    Tier1Simulation = 1,
    Tier2CommercialPlugin = 2,
};

inline constexpr std::string_view adapter_tier_name(AdapterTier tier) noexcept {
    switch (tier) {
        case AdapterTier::Tier0BuiltIn:
            return "T0";
        case AdapterTier::Tier1Simulation:
            return "T1";
        case AdapterTier::Tier2CommercialPlugin:
            return "T2";
    }
    return "UNKNOWN";
}

struct FormatAdapterDescriptor {
    std::string adapter_id;
    std::string display_name;
    AdapterTier tier = AdapterTier::Tier0BuiltIn;
    std::vector<std::string> extensions; // lowercase, without leading '.'
    bool supports_import = true;
    bool supports_export = false;
    bool supports_stage1_bom = false;
    bool is_exact_brep = false;
    bool available = true;
};

struct FormatProbeResult {
    std::string format_id;
    std::string unit_detected = "mm";
    uint64_t estimated_instances = 0;
    bool supports_stage1_bom_stream = false;
    bool is_exact_brep = false;
};

struct Stage1PrototypeEntry {
    uint32_t prototype_index = 0;
    std::string prototype_name;
    ShapeHandle shape_handle = kInvalidShapeHandle;
    BoundingBox3d local_aabb{};
};

struct Stage1InstanceNode {
    uint32_t instance_id = 0;
    uint32_t parent_index = std::numeric_limits<uint32_t>::max();
    uint32_t prototype_index = std::numeric_limits<uint32_t>::max();
    std::string name;
    Transform3d transform{};
    BoundingBox3d world_aabb{};
};

struct Stage1AssemblyGraph {
    std::string source_path;
    std::string format_id;
    std::string unit_detected = "mm";
    std::vector<Stage1PrototypeEntry> prototypes;
    std::vector<Stage1InstanceNode> instances;
    BoundingBox3d global_aabb{};
    double elapsed_ms = 0.0;
};

struct Stage2AssemblyResult {
    std::vector<TriangleMesh> prototype_meshes;
    uint32_t tessellated_count = 0;
    uint32_t total_triangles = 0;
    double elapsed_ms = 0.0;
};

// C-ABI descriptor contract for Tier 2 dynamically loaded commercial format plugins.
extern "C" {
struct CadFormatPluginAbiV1 {
    uint32_t abi_major;
    uint32_t abi_minor;
    const char* adapter_id;
    const char* display_name;
    const char* extensions_csv;
    int32_t (*probe_fn)(const char* file_path_utf8);
};
}

class IFormatAdapter {
public:
    virtual ~IFormatAdapter() = default;

    [[nodiscard]] virtual FormatAdapterDescriptor descriptor() const = 0;

    [[nodiscard]] virtual bool can_handle(
        std::string_view ext_lower,
        std::span<const uint8_t> header_magic) const = 0;

    virtual Status probe_stream(
        std::string_view file_path_utf8,
        FormatProbeResult& out) const = 0;

    virtual Status import_stage1_bom(
        std::string_view file_path_utf8,
        IBRepKernel& kernel,
        Stage1AssemblyGraph& out_graph) const = 0;

    virtual Status export_shape(
        ShapeHandle handle,
        const IBRepKernel& kernel,
        std::string_view out_path_utf8) const = 0;
};

class PluginRegistry {
public:
    PluginRegistry() = default;

    Status register_adapter(std::shared_ptr<IFormatAdapter> adapter);
    Status register_commercial_plugin_v1(const CadFormatPluginAbiV1& plugin);

    [[nodiscard]] std::shared_ptr<IFormatAdapter> find_by_extension(std::string_view ext) const;
    [[nodiscard]] std::shared_ptr<IFormatAdapter> find_by_id(std::string_view adapter_id) const;
    [[nodiscard]] std::vector<FormatAdapterDescriptor> list_by_tier(AdapterTier tier) const;
    [[nodiscard]] std::vector<FormatAdapterDescriptor> list_all() const;
    [[nodiscard]] std::size_t adapter_count() const;

    void register_builtin_defaults();

    static std::string normalize_extension(std::string_view ext);

private:
    mutable std::shared_mutex mutex_;
    std::unordered_map<std::string, std::shared_ptr<IFormatAdapter>> by_id_;
    std::unordered_map<std::string, std::shared_ptr<IFormatAdapter>> by_ext_;
};

} // namespace cad_geomd
