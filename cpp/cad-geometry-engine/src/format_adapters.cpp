#include "iformat_adapter.hpp"

#include <algorithm>
#include <cctype>
#include <mutex>
#include <utility>

namespace cad_geomd {

namespace {

std::string to_lower_ascii(std::string_view input) {
    std::string out;
    out.reserve(input.size());
    for (const char ch : input) {
        out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(ch))));
    }
    return out;
}

std::vector<std::string> split_csv_extensions(std::string_view csv) {
    std::vector<std::string> exts;
    std::size_t start = 0;
    while (start <= csv.size()) {
        const std::size_t comma = csv.find(',', start);
        const std::string_view token =
            (comma == std::string_view::npos) ? csv.substr(start) : csv.substr(start, comma - start);
        std::size_t b = 0;
        while (b < token.size() && std::isspace(static_cast<unsigned char>(token[b]))) {
            ++b;
        }
        std::size_t e = token.size();
        while (e > b && std::isspace(static_cast<unsigned char>(token[e - 1]))) {
            --e;
        }
        const std::string norm = PluginRegistry::normalize_extension(token.substr(b, e - b));
        if (!norm.empty()) {
            exts.push_back(norm);
        }
        if (comma == std::string_view::npos) {
            break;
        }
        start = comma + 1;
    }
    return exts;
}

class BuiltinFormatAdapter final : public IFormatAdapter {
public:
    explicit BuiltinFormatAdapter(FormatAdapterDescriptor desc) : desc_(std::move(desc)) {}

    FormatAdapterDescriptor descriptor() const override { return desc_; }

    bool can_handle(
        std::string_view ext_lower,
        std::span<const uint8_t> header_magic) const override {
        (void)header_magic;
        const std::string norm = PluginRegistry::normalize_extension(ext_lower);
        return std::find(desc_.extensions.begin(), desc_.extensions.end(), norm) !=
               desc_.extensions.end();
    }

    Status probe_stream(
        std::string_view file_path_utf8,
        FormatProbeResult& out) const override {
        if (file_path_utf8.empty()) {
            return Status::Error(ErrorCode::InvalidArgument, "file path must not be empty");
        }
        out = FormatProbeResult{};
        out.format_id = desc_.adapter_id;
        out.unit_detected = "mm";
        out.estimated_instances = 1u;
        out.supports_stage1_bom_stream = desc_.supports_stage1_bom;
        out.is_exact_brep = desc_.is_exact_brep;
        return Status::Ok();
    }

    Status import_stage1_bom(
        std::string_view file_path_utf8,
        IBRepKernel& kernel,
        Stage1AssemblyGraph& out_graph) const override {
        if (file_path_utf8.empty()) {
            return Status::Error(ErrorCode::InvalidArgument, "file path must not be empty");
        }
        if (!desc_.available) {
            return Status::Error(
                ErrorCode::KernelUnavailable,
                "format adapter is registered as an external plugin slot and is not loaded");
        }
        if (desc_.is_exact_brep && !kernel.capabilities().authoritative_brep) {
            return Status::Error(
                ErrorCode::KernelUnavailable,
                "exact B-Rep Stage 1 import requires an authoritative B-Rep kernel plugin (e.g. OCCT 7.9+)");
        }
        out_graph = Stage1AssemblyGraph{};
        out_graph.source_path = std::string(file_path_utf8);
        out_graph.format_id = desc_.adapter_id;
        out_graph.unit_detected = "mm";
        return Status::Ok();
    }

    Status export_shape(
        ShapeHandle handle,
        const IBRepKernel& kernel,
        std::string_view out_path_utf8) const override {
        if (!desc_.supports_export) {
            return Status::Error(
                ErrorCode::UnsupportedOperation,
                "format adapter does not support export");
        }
        if (desc_.adapter_id == "t0-step") {
            return kernel.export_step_ap242(handle, out_path_utf8, "part");
        }
        return Status::Error(
            ErrorCode::UnsupportedOperation,
            "export not implemented in reference adapter for " + desc_.adapter_id);
    }

private:
    FormatAdapterDescriptor desc_;
};

class CommercialPluginAdapter final : public IFormatAdapter {
public:
    explicit CommercialPluginAdapter(const CadFormatPluginAbiV1& plugin)
        : probe_fn_(plugin.probe_fn) {
        desc_.adapter_id = plugin.adapter_id ? plugin.adapter_id : "";
        desc_.display_name = plugin.display_name ? plugin.display_name : desc_.adapter_id;
        desc_.tier = AdapterTier::Tier2CommercialPlugin;
        desc_.extensions = split_csv_extensions(plugin.extensions_csv ? plugin.extensions_csv : "");
        desc_.supports_import = true;
        desc_.supports_export = false;
        desc_.supports_stage1_bom = true;
        desc_.is_exact_brep = true;
        desc_.available = (probe_fn_ != nullptr);
    }

    FormatAdapterDescriptor descriptor() const override { return desc_; }

    bool can_handle(
        std::string_view ext_lower,
        std::span<const uint8_t> header_magic) const override {
        (void)header_magic;
        const std::string norm = PluginRegistry::normalize_extension(ext_lower);
        return std::find(desc_.extensions.begin(), desc_.extensions.end(), norm) !=
               desc_.extensions.end();
    }

    Status probe_stream(
        std::string_view file_path_utf8,
        FormatProbeResult& out) const override {
        if (!probe_fn_) {
            return Status::Error(ErrorCode::KernelUnavailable, "commercial plugin probe_fn is null");
        }
        const std::string path(file_path_utf8);
        const int32_t rc = probe_fn_(path.c_str());
        if (rc != 0) {
            return Status::Error(ErrorCode::UnsupportedFormat, "commercial plugin rejected file");
        }
        out = FormatProbeResult{};
        out.format_id = desc_.adapter_id;
        out.unit_detected = "mm";
        out.supports_stage1_bom_stream = true;
        out.is_exact_brep = true;
        return Status::Ok();
    }

    Status import_stage1_bom(
        std::string_view file_path_utf8,
        IBRepKernel& kernel,
        Stage1AssemblyGraph& out_graph) const override {
        (void)file_path_utf8;
        (void)kernel;
        (void)out_graph;
        return Status::Error(
            ErrorCode::UnsupportedOperation,
            "commercial plugin Stage 1 BOM import requires licensed native middleware implementation");
    }

    Status export_shape(
        ShapeHandle handle,
        const IBRepKernel& kernel,
        std::string_view out_path_utf8) const override {
        (void)handle;
        (void)kernel;
        (void)out_path_utf8;
        return Status::Error(
            ErrorCode::UnsupportedOperation,
            "commercial plugin export is not supported");
    }

private:
    FormatAdapterDescriptor desc_;
    int32_t (*probe_fn_)(const char*) = nullptr;
};

} // namespace

std::string PluginRegistry::normalize_extension(std::string_view ext) {
    while (!ext.empty() && ext.front() == '.') {
        ext.remove_prefix(1);
    }
    return to_lower_ascii(ext);
}

Status PluginRegistry::register_adapter(std::shared_ptr<IFormatAdapter> adapter) {
    if (!adapter) {
        return Status::Error(ErrorCode::InvalidArgument, "adapter pointer must not be null");
    }
    const FormatAdapterDescriptor desc = adapter->descriptor();
    if (desc.adapter_id.empty()) {
        return Status::Error(ErrorCode::InvalidArgument, "adapter_id must not be empty");
    }

    std::unique_lock<std::shared_mutex> lock(mutex_);
    by_id_[desc.adapter_id] = adapter;
    for (const auto& ext : desc.extensions) {
        const std::string norm = normalize_extension(ext);
        if (!norm.empty()) {
            by_ext_[norm] = adapter;
        }
    }
    return Status::Ok();
}

Status PluginRegistry::register_commercial_plugin_v1(const CadFormatPluginAbiV1& plugin) {
    if (plugin.abi_major != CAD_GEOMD_ABI_MAJOR) {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "incompatible commercial plugin ABI major version");
    }
    if (plugin.adapter_id == nullptr || plugin.adapter_id[0] == '\0') {
        return Status::Error(
            ErrorCode::InvalidArgument,
            "commercial plugin adapter_id must not be empty");
    }
    return register_adapter(std::make_shared<CommercialPluginAdapter>(plugin));
}

std::shared_ptr<IFormatAdapter> PluginRegistry::find_by_extension(std::string_view ext) const {
    const std::string norm = normalize_extension(ext);
    if (norm.empty()) {
        return nullptr;
    }
    std::shared_lock<std::shared_mutex> lock(mutex_);
    const auto it = by_ext_.find(norm);
    return it == by_ext_.end() ? nullptr : it->second;
}

std::shared_ptr<IFormatAdapter> PluginRegistry::find_by_id(std::string_view adapter_id) const {
    std::shared_lock<std::shared_mutex> lock(mutex_);
    const auto it = by_id_.find(std::string(adapter_id));
    return it == by_id_.end() ? nullptr : it->second;
}

std::vector<FormatAdapterDescriptor> PluginRegistry::list_by_tier(AdapterTier tier) const {
    std::vector<FormatAdapterDescriptor> out;
    {
        std::shared_lock<std::shared_mutex> lock(mutex_);
        for (const auto& kv : by_id_) {
            FormatAdapterDescriptor desc = kv.second->descriptor();
            if (desc.tier == tier) {
                out.push_back(std::move(desc));
            }
        }
    }
    std::sort(out.begin(), out.end(), [](const auto& a, const auto& b) {
        return a.adapter_id < b.adapter_id;
    });
    return out;
}

std::vector<FormatAdapterDescriptor> PluginRegistry::list_all() const {
    std::vector<FormatAdapterDescriptor> out;
    {
        std::shared_lock<std::shared_mutex> lock(mutex_);
        out.reserve(by_id_.size());
        for (const auto& kv : by_id_) {
            out.push_back(kv.second->descriptor());
        }
    }
    std::sort(out.begin(), out.end(), [](const auto& a, const auto& b) {
        if (a.tier != b.tier) {
            return static_cast<uint32_t>(a.tier) < static_cast<uint32_t>(b.tier);
        }
        return a.adapter_id < b.adapter_id;
    });
    return out;
}

std::size_t PluginRegistry::adapter_count() const {
    std::shared_lock<std::shared_mutex> lock(mutex_);
    return by_id_.size();
}

void PluginRegistry::register_builtin_defaults() {
    // Tier 0: Built-in open CAD & mesh exchange formats
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t0-step",
        "STEP AP242 / AP214 / AP203",
        AdapterTier::Tier0BuiltIn,
        {"step", "stp"},
        true,
        true,
        true,
        true,
        true,
    }));
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t0-iges",
        "IGES 5.3",
        AdapterTier::Tier0BuiltIn,
        {"iges", "igs"},
        true,
        false,
        true,
        true,
        true,
    }));
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t0-brep",
        "Native B-Rep",
        AdapterTier::Tier0BuiltIn,
        {"brep", "brp"},
        true,
        true,
        false,
        true,
        true,
    }));
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t0-stl",
        "STL Triangle Mesh",
        AdapterTier::Tier0BuiltIn,
        {"stl"},
        true,
        true,
        false,
        false,
        true,
    }));
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t0-3mf",
        "3MF Manufacturing Package",
        AdapterTier::Tier0BuiltIn,
        {"3mf"},
        true,
        true,
        true,
        false,
        true,
    }));

    // Tier 1: Embodied simulation & robotics exchange adapters
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t1-openusd",
        "OpenUSD Scene & Physics",
        AdapterTier::Tier1Simulation,
        {"usd", "usda", "usdc", "usdz"},
        true,
        true,
        true,
        false,
        true,
    }));
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t1-urdf",
        "URDF / Robotics Kinematics",
        AdapterTier::Tier1Simulation,
        {"urdf"},
        true,
        true,
        true,
        false,
        true,
    }));

    // Tier 2: Commercial CAD middleware dynamic plugin slot (unloaded by default)
    (void)register_adapter(std::make_shared<BuiltinFormatAdapter>(FormatAdapterDescriptor{
        "t2-commercial-slot",
        "Commercial Native CAD Plugin Slot (SolidWorks/CATIA/NX/Creo)",
        AdapterTier::Tier2CommercialPlugin,
        {"sldprt", "sldasm", "catpart", "catproduct"},
        true,
        false,
        true,
        true,
        false,
    }));
}

} // namespace cad_geomd
