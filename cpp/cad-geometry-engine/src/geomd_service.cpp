#include "geomd_service.hpp"
#include "occt_kernel_plugin.hpp"
#include "reference_analytic_kernel.hpp"

#include <cmath>
#include <istream>
#include <ostream>
#include <utility>
#include <vector>

namespace cad_geomd {

namespace {

bool read_required_double(const JsonValue* params, std::string_view key, double& out) {
    if (!params) {
        return false;
    }
    const JsonValue* v = params->find(key);
    if (!v || !v->is_number()) {
        return false;
    }
    out = *v->as_number();
    return std::isfinite(out);
}

double read_optional_double(const JsonValue* params, std::string_view key, double fallback) {
    if (!params) {
        return fallback;
    }
    const JsonValue* v = params->find(key);
    if (!v || !v->is_number() || !std::isfinite(*v->as_number())) {
        return fallback;
    }
    return *v->as_number();
}

uint64_t read_optional_u64(const JsonValue* params, std::string_view key, uint64_t fallback) {
    if (!params) {
        return fallback;
    }
    const JsonValue* v = params->find(key);
    if (!v || !v->is_number() || *v->as_number() < 0.0) {
        return fallback;
    }
    return static_cast<uint64_t>(*v->as_number());
}

std::string read_optional_string(const JsonValue* params, std::string_view key, std::string fallback) {
    if (!params) {
        return fallback;
    }
    const JsonValue* v = params->find(key);
    if (!v || !v->is_string()) {
        return fallback;
    }
    return *v->as_string();
}

JsonValue vec3_to_json(const Vec3d& v) {
    JsonValue::Array arr;
    arr.emplace_back(v.x);
    arr.emplace_back(v.y);
    arr.emplace_back(v.z);
    return JsonValue(std::move(arr));
}

} // namespace

GeomdService::GeomdService(
    std::unique_ptr<IBRepKernel> kernel,
    uint32_t worker_threads)
    : kernel_(kernel ? std::move(kernel) : std::make_unique<ReferenceAnalyticKernel>(worker_threads)),
      pool_(worker_threads) {
    plugins_.register_builtin_defaults();
}

std::string GeomdService::handle_request_json(
    std::string_view request_json,
    bool& out_should_shutdown) {
    out_should_shutdown = false;

    JsonValue req;
    const Status parse_status = parse_json(request_json, req);
    if (!parse_status.ok()) {
        return serialize_json(make_error_response(JsonValue(nullptr), parse_status));
    }
    if (!req.is_object()) {
        return serialize_json(make_error_response(
            JsonValue(nullptr),
            Status::Error(ErrorCode::ProtocolError, "request root must be a JSON object")));
    }

    const JsonValue* id_ptr = req.find("id");
    const JsonValue id = id_ptr ? *id_ptr : JsonValue(nullptr);

    const JsonValue* method_ptr = req.find("method");
    if (!method_ptr || !method_ptr->is_string() || method_ptr->as_string()->empty()) {
        return serialize_json(make_error_response(
            id,
            Status::Error(ErrorCode::ProtocolError, "request missing string 'method' field")));
    }

    const JsonValue* params = req.find("params");
    if (params != nullptr && !params->is_object() && !params->is_null()) {
        return serialize_json(make_error_response(
            id,
            Status::Error(ErrorCode::ProtocolError, "'params' must be a JSON object when provided")));
    }

    return serialize_json(dispatch_method(*method_ptr->as_string(), id, params, out_should_shutdown));
}

JsonValue GeomdService::dispatch_method(
    const std::string& method,
    const JsonValue& id,
    const JsonValue* params,
    bool& out_should_shutdown) {
    if (method == "ping") {
        JsonValue::Object res;
        res["pong"] = JsonValue(true);
        res["abi_major"] = JsonValue(CAD_GEOMD_ABI_MAJOR);
        res["abi_minor"] = JsonValue(CAD_GEOMD_ABI_MINOR);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "capabilities") {
        const KernelCapabilities caps = kernel_->capabilities();
        JsonValue::Array available_kernels;
        available_kernels.emplace_back("analytic");
        if (is_occt_plugin_compiled()) {
            available_kernels.emplace_back("occt");
        }
        JsonValue::Object res;
        res["kernel_name"] = JsonValue(caps.kernel_name);
        res["kernel_version"] = JsonValue(caps.kernel_version);
        res["abi_major"] = JsonValue(caps.abi_major);
        res["abi_minor"] = JsonValue(caps.abi_minor);
        res["authoritative_brep"] = JsonValue(caps.authoritative_brep);
        res["supports_parallel_tessellation"] = JsonValue(caps.supports_parallel_tessellation);
        res["supports_step_ap242"] = JsonValue(caps.supports_step_ap242);
        res["supports_exact_booleans"] = JsonValue(caps.supports_exact_booleans);
        res["supports_two_stage_assembly"] = JsonValue(caps.supports_two_stage_assembly);
        res["worker_threads"] = JsonValue(pool_.thread_count());
        res["occt_compiled"] = JsonValue(is_occt_plugin_compiled());
        res["available_kernels"] = JsonValue(std::move(available_kernels));
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "list_adapters") {
        std::vector<FormatAdapterDescriptor> list;
        if (params && params->find("tier") && params->find("tier")->is_number()) {
            const uint32_t tier_num = static_cast<uint32_t>(*params->find("tier")->as_number());
            if (tier_num > 2u) {
                return make_error_response(
                    id,
                    Status::Error(ErrorCode::InvalidArgument, "tier must be 0, 1, or 2"));
            }
            list = plugins_.list_by_tier(static_cast<AdapterTier>(tier_num));
        } else {
            list = plugins_.list_all();
        }

        JsonValue::Array arr;
        arr.reserve(list.size());
        for (const auto& desc : list) {
            JsonValue::Array exts;
            for (const auto& e : desc.extensions) {
                exts.emplace_back(e);
            }
            JsonValue::Object item;
            item["adapter_id"] = JsonValue(desc.adapter_id);
            item["display_name"] = JsonValue(desc.display_name);
            item["tier"] = JsonValue(static_cast<uint32_t>(desc.tier));
            item["tier_name"] = JsonValue(adapter_tier_name(desc.tier));
            item["extensions"] = JsonValue(std::move(exts));
            item["supports_import"] = JsonValue(desc.supports_import);
            item["supports_export"] = JsonValue(desc.supports_export);
            item["supports_stage1_bom"] = JsonValue(desc.supports_stage1_bom);
            item["is_exact_brep"] = JsonValue(desc.is_exact_brep);
            item["available"] = JsonValue(desc.available);
            arr.emplace_back(std::move(item));
        }

        JsonValue::Object res;
        res["adapters"] = JsonValue(std::move(arr));
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "create_box") {
        double dx = 0.0;
        double dy = 0.0;
        double dz = 0.0;
        if (!read_required_double(params, "dx", dx) ||
            !read_required_double(params, "dy", dy) ||
            !read_required_double(params, "dz", dz)) {
            return make_error_response(
                id,
                Status::Error(ErrorCode::InvalidArgument, "create_box requires numeric dx, dy, dz"));
        }
        const Vec3d origin{
            read_optional_double(params, "ox", 0.0),
            read_optional_double(params, "oy", 0.0),
            read_optional_double(params, "oz", 0.0),
        };
        ShapeHandle handle = kInvalidShapeHandle;
        const Status s = kernel_->make_box(dx, dy, dz, origin, handle);
        if (!s.ok()) {
            return make_error_response(id, s);
        }
        MassProperties props{};
        (void)kernel_->compute_mass_properties(handle, props);
        JsonValue::Object res;
        res["handle"] = JsonValue(handle);
        res["volume_mm3"] = JsonValue(props.volume_mm3);
        res["face_count"] = JsonValue(props.face_count);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "create_cylinder") {
        double diameter = 0.0;
        double height = 0.0;
        if (!read_required_double(params, "diameter", diameter) ||
            !read_required_double(params, "height", height)) {
            return make_error_response(
                id,
                Status::Error(
                    ErrorCode::InvalidArgument,
                    "create_cylinder requires numeric diameter and height"));
        }
        const Vec3d origin{
            read_optional_double(params, "ox", 0.0),
            read_optional_double(params, "oy", 0.0),
            read_optional_double(params, "oz", 0.0),
        };
        ShapeHandle handle = kInvalidShapeHandle;
        const Status s = kernel_->make_cylinder(diameter, height, origin, handle);
        if (!s.ok()) {
            return make_error_response(id, s);
        }
        MassProperties props{};
        (void)kernel_->compute_mass_properties(handle, props);
        JsonValue::Object res;
        res["handle"] = JsonValue(handle);
        res["volume_mm3"] = JsonValue(props.volume_mm3);
        res["face_count"] = JsonValue(props.face_count);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "boolean") {
        const uint64_t base_handle = read_optional_u64(params, "base_handle", 0u);
        const uint64_t tool_handle = read_optional_u64(params, "tool_handle", 0u);
        const std::string op_str = read_optional_string(params, "op", "");
        if (base_handle == 0u || tool_handle == 0u || op_str.empty()) {
            return make_error_response(
                id,
                Status::Error(
                    ErrorCode::InvalidArgument,
                    "boolean requires positive base_handle, tool_handle, and string 'op' (union|cut|intersect)"));
        }
        BooleanOpKind op_kind = BooleanOpKind::Union;
        if (op_str == "union") {
            op_kind = BooleanOpKind::Union;
        } else if (op_str == "cut" || op_str == "subtract") {
            op_kind = BooleanOpKind::Cut;
        } else if (op_str == "intersect" || op_str == "common") {
            op_kind = BooleanOpKind::Intersect;
        } else {
            return make_error_response(
                id,
                Status::Error(
                    ErrorCode::InvalidArgument,
                    "boolean 'op' must be 'union', 'cut', or 'intersect'"));
        }
        ShapeHandle handle = kInvalidShapeHandle;
        const Status s = kernel_->boolean_op(base_handle, tool_handle, op_kind, handle);
        if (!s.ok()) {
            return make_error_response(id, s);
        }
        MassProperties props{};
        (void)kernel_->compute_mass_properties(handle, props);
        JsonValue::Object res;
        res["handle"] = JsonValue(handle);
        res["volume_mm3"] = JsonValue(props.volume_mm3);
        res["face_count"] = JsonValue(props.face_count);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "release_shape") {
        const uint64_t handle = read_optional_u64(params, "handle", 0u);
        if (handle == 0u) {
            return make_error_response(
                id,
                Status::Error(ErrorCode::InvalidArgument, "release_shape requires positive 'handle'"));
        }
        const Status s = kernel_->release_shape(handle);
        if (!s.ok()) {
            return make_error_response(id, s);
        }
        JsonValue::Object res;
        res["released"] = JsonValue(true);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "tessellate") {
        const uint64_t handle = read_optional_u64(params, "handle", 0u);
        if (handle == 0u) {
            return make_error_response(
                id,
                Status::Error(ErrorCode::InvalidArgument, "tessellate requires positive 'handle'"));
        }
        TessellationParams tparams{};
        tparams.lod_level = static_cast<uint8_t>(read_optional_u64(params, "lod_level", 1u));
        tparams.linear_deflection_mm = read_optional_double(params, "linear_deflection_mm", 0.05);
        tparams.angular_deflection_rad = read_optional_double(params, "angular_deflection_rad", 0.2);
        tparams.cylinder_segments = static_cast<uint32_t>(read_optional_u64(params, "cylinder_segments", 24u));

        TriangleMesh mesh{};
        const Status s = kernel_->tessellate(handle, tparams, mesh);
        if (!s.ok()) {
            return make_error_response(id, s);
        }

        JsonValue::Array positions;
        positions.reserve(mesh.positions_mm.size());
        for (const double v : mesh.positions_mm) {
            positions.emplace_back(v);
        }
        JsonValue::Array normals;
        normals.reserve(mesh.normals.size());
        for (const double v : mesh.normals) {
            normals.emplace_back(v);
        }
        JsonValue::Array indices;
        indices.reserve(mesh.indices.size());
        for (const uint32_t idx : mesh.indices) {
            indices.emplace_back(idx);
        }

        JsonValue::Object res;
        res["handle"] = JsonValue(mesh.source_handle);
        res["vertex_count"] = JsonValue(mesh.vertex_count());
        res["triangle_count"] = JsonValue(mesh.triangle_count());
        res["face_count"] = JsonValue(mesh.face_count);
        res["positions_mm"] = JsonValue(std::move(positions));
        res["normals"] = JsonValue(std::move(normals));
        res["indices"] = JsonValue(std::move(indices));
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "tessellate_batch") {
        if (!params || !params->find("handles") || !params->find("handles")->is_array()) {
            return make_error_response(
                id,
                Status::Error(ErrorCode::InvalidArgument, "tessellate_batch requires 'handles' array"));
        }
        const auto& handle_arr = *params->find("handles")->as_array();
        std::vector<ShapeHandle> handles;
        handles.reserve(handle_arr.size());
        for (const auto& item : handle_arr) {
            if (!item.is_number() || *item.as_number() < 0.0) {
                return make_error_response(
                    id,
                    Status::Error(ErrorCode::InvalidArgument, "all handles must be non-negative integers"));
            }
            handles.push_back(static_cast<ShapeHandle>(*item.as_number()));
        }

        TessellationParams tparams{};
        tparams.lod_level = static_cast<uint8_t>(read_optional_u64(params, "lod_level", 1u));
        tparams.linear_deflection_mm = read_optional_double(params, "linear_deflection_mm", 0.05);
        tparams.angular_deflection_rad = read_optional_double(params, "angular_deflection_rad", 0.2);
        tparams.cylinder_segments = static_cast<uint32_t>(read_optional_u64(params, "cylinder_segments", 24u));

        std::vector<TriangleMesh> meshes;
        const Status s = kernel_->tessellate_parallel(handles, tparams, pool_, meshes);
        if (!s.ok()) {
            return make_error_response(id, s);
        }

        JsonValue::Array mesh_arr;
        mesh_arr.reserve(meshes.size());
        uint32_t total_triangles = 0;
        for (const auto& mesh : meshes) {
            total_triangles += mesh.triangle_count();
            JsonValue::Object m;
            m["handle"] = JsonValue(mesh.source_handle);
            m["vertex_count"] = JsonValue(mesh.vertex_count());
            m["triangle_count"] = JsonValue(mesh.triangle_count());
            m["face_count"] = JsonValue(mesh.face_count);
            mesh_arr.emplace_back(std::move(m));
        }

        JsonValue::Object res;
        res["meshes"] = JsonValue(std::move(mesh_arr));
        res["total_triangles"] = JsonValue(total_triangles);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "import_assembly_stage1") {
        Stage1AssemblySpec spec{};
        spec.source_path = read_optional_string(params, "source_path", "memory://assembly.step");
        spec.format_id = read_optional_string(params, "format_id", "step");
        spec.instance_count = static_cast<uint32_t>(read_optional_u64(params, "instance_count", 0u));
        spec.prototype_count = static_cast<uint32_t>(read_optional_u64(params, "prototype_count", 0u));
        if (spec.instance_count == 0u) {
            return make_error_response(
                id,
                Status::Error(
                    ErrorCode::InvalidArgument,
                    "import_assembly_stage1 requires positive 'instance_count'"));
        }

        Stage1AssemblyGraph graph{};
        const Status s = pool_.ingest_assembly_stage1(spec, *kernel_, graph, nullptr);
        if (!s.ok()) {
            return make_error_response(id, s);
        }

        const uint64_t session_id = next_session_id_.fetch_add(1u, std::memory_order_relaxed);
        const uint32_t inst_count = static_cast<uint32_t>(graph.instances.size());
        const uint32_t proto_count = static_cast<uint32_t>(graph.prototypes.size());
        const double elapsed_ms = graph.elapsed_ms;
        const BoundingBox3d bbox = graph.global_aabb;

        {
            std::lock_guard<std::mutex> lock(sessions_mutex_);
            assembly_sessions_.emplace(session_id, std::move(graph));
        }

        JsonValue::Object res;
        res["session_id"] = JsonValue(session_id);
        res["instance_count"] = JsonValue(inst_count);
        res["prototype_count"] = JsonValue(proto_count);
        res["elapsed_ms"] = JsonValue(elapsed_ms);
        res["bbox_min"] = vec3_to_json(bbox.min_mm);
        res["bbox_max"] = vec3_to_json(bbox.max_mm);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "import_assembly_stage2") {
        const uint64_t session_id = read_optional_u64(params, "session_id", 0u);
        if (session_id == 0u) {
            return make_error_response(
                id,
                Status::Error(ErrorCode::InvalidArgument, "import_assembly_stage2 requires 'session_id'"));
        }

        Stage1AssemblyGraph graph_snapshot{};
        {
            std::lock_guard<std::mutex> lock(sessions_mutex_);
            const auto it = assembly_sessions_.find(session_id);
            if (it == assembly_sessions_.end()) {
                return make_error_response(
                    id,
                    Status::Error(ErrorCode::NotFound, "unknown assembly session_id"));
            }
            graph_snapshot = it->second;
        }

        TessellationParams tparams{};
        tparams.lod_level = static_cast<uint8_t>(read_optional_u64(params, "lod_level", 1u));
        tparams.linear_deflection_mm = read_optional_double(params, "linear_deflection_mm", 0.05);
        tparams.angular_deflection_rad = read_optional_double(params, "angular_deflection_rad", 0.2);
        tparams.cylinder_segments = static_cast<uint32_t>(read_optional_u64(params, "cylinder_segments", 24u));

        Stage2AssemblyResult stage2{};
        const Status s = pool_.ingest_assembly_stage2(graph_snapshot, *kernel_, tparams, stage2, nullptr);
        if (!s.ok()) {
            return make_error_response(id, s);
        }

        JsonValue::Object res;
        res["session_id"] = JsonValue(session_id);
        res["tessellated_count"] = JsonValue(stage2.tessellated_count);
        res["total_triangles"] = JsonValue(stage2.total_triangles);
        res["elapsed_ms"] = JsonValue(stage2.elapsed_ms);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    if (method == "shutdown") {
        out_should_shutdown = true;
        JsonValue::Object res;
        res["shutting_down"] = JsonValue(true);
        return make_success_response(id, JsonValue(std::move(res)));
    }

    return make_error_response(
        id,
        Status::Error(ErrorCode::UnsupportedOperation, "unknown RPC method: " + method));
}

int GeomdService::run_framed_loop(std::istream& in, std::ostream& out) {
    while (true) {
        std::string payload;
        bool eof = false;
        const Status rs = read_frame(in, payload, eof);
        if (eof) {
            return 0;
        }
        if (!rs.ok()) {
            const std::string err_json = serialize_json(make_error_response(JsonValue(nullptr), rs));
            (void)write_frame(out, err_json);
            return 1;
        }

        bool should_shutdown = false;
        const std::string resp_json = handle_request_json(payload, should_shutdown);
        const Status ws = write_frame(out, resp_json);
        if (!ws.ok()) {
            return 1;
        }
        if (should_shutdown) {
            return 0;
        }
    }
}

} // namespace cad_geomd
