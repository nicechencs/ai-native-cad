#include "worker_pool.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <limits>
#include <utility>

namespace cad_geomd {

namespace {

uint32_t default_worker_count() noexcept {
    const unsigned int hw = std::thread::hardware_concurrency();
    if (hw == 0u) {
        return 4u;
    }
    return std::clamp(static_cast<uint32_t>(hw), 2u, 16u);
}

void expand_bbox(BoundingBox3d& dst, const BoundingBox3d& src) noexcept {
    dst.min_mm.x = std::min(dst.min_mm.x, src.min_mm.x);
    dst.min_mm.y = std::min(dst.min_mm.y, src.min_mm.y);
    dst.min_mm.z = std::min(dst.min_mm.z, src.min_mm.z);
    dst.max_mm.x = std::max(dst.max_mm.x, src.max_mm.x);
    dst.max_mm.y = std::max(dst.max_mm.y, src.max_mm.y);
    dst.max_mm.z = std::max(dst.max_mm.z, src.max_mm.z);
}

} // namespace

WorkerPool::WorkerPool(uint32_t num_threads)
    : thread_count_(num_threads == 0u ? default_worker_count() : std::clamp(num_threads, 1u, 64u)) {
    workers_.reserve(thread_count_);
    for (uint32_t i = 0; i < thread_count_; ++i) {
        workers_.emplace_back([this](std::stop_token st) { worker_loop(std::move(st)); });
    }
}

WorkerPool::~WorkerPool() {
    {
        std::lock_guard<std::mutex> lock(queue_mutex_);
        stopping_ = true;
    }
    for (auto& w : workers_) {
        w.request_stop();
    }
    queue_cv_.notify_all();
    workers_.clear();
}

void WorkerPool::worker_loop(std::stop_token stop_token) {
    while (true) {
        std::function<void()> task;
        {
            std::unique_lock<std::mutex> lock(queue_mutex_);
            queue_cv_.wait(lock, stop_token, [this]() {
                return stopping_ || !tasks_.empty();
            });
            if (tasks_.empty()) {
                if (stopping_ || stop_token.stop_requested()) {
                    return;
                }
                continue;
            }
            task = std::move(tasks_.front());
            tasks_.pop_front();
        }
        task();
    }
}

Status WorkerPool::parallel_for(
    std::size_t count,
    const std::function<Status(std::size_t)>& fn,
    const CancellationToken* cancel) {
    if (count == 0u) {
        return Status::Ok();
    }
    if (cancel && cancel->is_cancelled()) {
        return Status::Error(ErrorCode::Cancelled, "operation cancelled before execution");
    }

    struct SharedBatchState {
        std::atomic<std::size_t> next_index{0};
        std::atomic<std::size_t> remaining_participants{0};
        std::atomic<bool> has_error{false};
        std::atomic<std::size_t> first_error_index{std::numeric_limits<std::size_t>::max()};
        std::mutex error_mutex;
        Status first_error = Status::Ok();
        std::mutex done_mutex;
        std::condition_variable done_cv;
    };

    const uint32_t active_workers = static_cast<uint32_t>(
        std::min<std::size_t>(count, static_cast<std::size_t>(thread_count_)));
    // Calling thread participates as 1 runner; enqueue (active_workers - 1) helper tasks.
    const uint32_t helper_tasks = active_workers > 1u ? (active_workers - 1u) : 0u;

    auto state = std::make_shared<SharedBatchState>();
    state->remaining_participants.store(helper_tasks + 1u, std::memory_order_release);

    auto run_shard = [state, count, &fn, cancel]() {
        while (true) {
            if (cancel && cancel->is_cancelled()) {
                std::lock_guard<std::mutex> lock(state->error_mutex);
                if (!state->has_error.load(std::memory_order_acquire)) {
                    state->first_error = Status::Error(ErrorCode::Cancelled, "parallel task cancelled");
                    state->has_error.store(true, std::memory_order_release);
                }
                break;
            }
            const std::size_t idx = state->next_index.fetch_add(1u, std::memory_order_relaxed);
            if (idx >= count) {
                break;
            }
            Status s = fn(idx);
            if (!s.ok()) {
                std::lock_guard<std::mutex> lock(state->error_mutex);
                const std::size_t prev_idx = state->first_error_index.load(std::memory_order_relaxed);
                if (idx < prev_idx) {
                    state->first_error_index.store(idx, std::memory_order_relaxed);
                    state->first_error = std::move(s);
                }
                state->has_error.store(true, std::memory_order_release);
            }
        }
        if (state->remaining_participants.fetch_sub(1u, std::memory_order_acq_rel) == 1u) {
            std::lock_guard<std::mutex> lock(state->done_mutex);
            state->done_cv.notify_one();
        }
    };

    if (helper_tasks > 0u) {
        {
            std::lock_guard<std::mutex> lock(queue_mutex_);
            for (uint32_t i = 0; i < helper_tasks; ++i) {
                tasks_.emplace_back(run_shard);
            }
        }
        queue_cv_.notify_all();
    }

    // Calling thread executes work directly so nested calls never deadlock.
    run_shard();

    {
        std::unique_lock<std::mutex> lock(state->done_mutex);
        state->done_cv.wait(lock, [&state]() {
            return state->remaining_participants.load(std::memory_order_acquire) == 0u;
        });
    }

    if (state->has_error.load(std::memory_order_acquire)) {
        return state->first_error;
    }
    return Status::Ok();
}

Status WorkerPool::tessellate_batch(
    const IBRepKernel& kernel,
    std::span<const ShapeHandle> handles,
    const TessellationParams& params,
    std::vector<TriangleMesh>& out_meshes,
    const CancellationToken* cancel) {
    out_meshes.clear();
    out_meshes.resize(handles.size());
    if (handles.empty()) {
        return Status::Ok();
    }

    return parallel_for(
        handles.size(),
        [&kernel, handles, &params, &out_meshes](std::size_t i) -> Status {
            return kernel.tessellate(handles[i], params, out_meshes[i]);
        },
        cancel);
}

Status WorkerPool::ingest_assembly_stage1(
    const Stage1AssemblySpec& spec,
    IBRepKernel& kernel,
    Stage1AssemblyGraph& out_graph,
    const CancellationToken* cancel) {
    if (spec.instance_count == 0u) {
        return Status::Error(ErrorCode::InvalidArgument, "instance_count must be positive");
    }

    const auto t0 = std::chrono::steady_clock::now();
    out_graph = Stage1AssemblyGraph{};
    out_graph.source_path = spec.source_path;
    out_graph.format_id = spec.format_id.empty() ? "step" : spec.format_id;
    out_graph.unit_detected = "mm";

    const uint32_t proto_count = std::clamp(
        spec.prototype_count == 0u ? std::min<uint32_t>(spec.instance_count, 16u) : spec.prototype_count,
        1u,
        spec.instance_count);

    out_graph.prototypes.reserve(proto_count);
    for (uint32_t p = 0; p < proto_count; ++p) {
        if (cancel && cancel->is_cancelled()) {
            return Status::Error(ErrorCode::Cancelled, "stage 1 cancelled during prototype creation");
        }
        ShapeHandle h = kInvalidShapeHandle;
        Status s = Status::Ok();
        const double size_base = 10.0 + static_cast<double>(p % 8u) * 2.5;
        if ((p % 2u) == 0u) {
            s = kernel.make_box(size_base, size_base * 0.8, size_base * 0.5, {0.0, 0.0, 0.0}, h);
        } else {
            s = kernel.make_cylinder(size_base * 0.75, size_base * 1.2, {0.0, 0.0, 0.0}, h);
        }
        if (!s.ok()) {
            return s;
        }

        MassProperties props{};
        s = kernel.compute_mass_properties(h, props);
        if (!s.ok()) {
            return s;
        }

        Stage1PrototypeEntry entry{};
        entry.prototype_index = p;
        entry.prototype_name = "proto_" + std::to_string(p);
        entry.shape_handle = h;
        entry.local_aabb = props.bbox;
        out_graph.prototypes.push_back(std::move(entry));
    }

    out_graph.instances.reserve(spec.instance_count);
    BoundingBox3d global_aabb{};
    global_aabb.min_mm = {
        std::numeric_limits<double>::infinity(),
        std::numeric_limits<double>::infinity(),
        std::numeric_limits<double>::infinity(),
    };
    global_aabb.max_mm = {
        -std::numeric_limits<double>::infinity(),
        -std::numeric_limits<double>::infinity(),
        -std::numeric_limits<double>::infinity(),
    };

    constexpr uint32_t kGridCols = 32u;
    constexpr double kGridSpacingMm = 40.0;

    for (uint32_t i = 0; i < spec.instance_count; ++i) {
        if ((i & 255u) == 0u && cancel && cancel->is_cancelled()) {
            return Status::Error(ErrorCode::Cancelled, "stage 1 cancelled during BOM tree build");
        }
        const uint32_t p = i % proto_count;
        const auto& proto = out_graph.prototypes[p];

        const double tx = static_cast<double>(i % kGridCols) * kGridSpacingMm;
        const double ty = static_cast<double>((i / kGridCols) % kGridCols) * kGridSpacingMm;
        const double tz = static_cast<double>(i / (kGridCols * kGridCols)) * kGridSpacingMm;

        Stage1InstanceNode node{};
        node.instance_id = i;
        node.parent_index = std::numeric_limits<uint32_t>::max();
        node.prototype_index = p;
        node.name = "inst_" + std::to_string(i);
        node.transform.translation_mm = {tx, ty, tz};
        node.transform.rotation_quat_xyzw = {0.0, 0.0, 0.0, 1.0};
        node.world_aabb.min_mm = {
            proto.local_aabb.min_mm.x + tx,
            proto.local_aabb.min_mm.y + ty,
            proto.local_aabb.min_mm.z + tz,
        };
        node.world_aabb.max_mm = {
            proto.local_aabb.max_mm.x + tx,
            proto.local_aabb.max_mm.y + ty,
            proto.local_aabb.max_mm.z + tz,
        };

        expand_bbox(global_aabb, node.world_aabb);
        out_graph.instances.push_back(std::move(node));
    }

    out_graph.global_aabb = global_aabb;
    const auto t1 = std::chrono::steady_clock::now();
    out_graph.elapsed_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    return Status::Ok();
}

Status WorkerPool::ingest_assembly_stage2(
    const Stage1AssemblyGraph& graph,
    const IBRepKernel& kernel,
    const TessellationParams& params,
    Stage2AssemblyResult& out_result,
    const CancellationToken* cancel) {
    const auto t0 = std::chrono::steady_clock::now();
    out_result = Stage2AssemblyResult{};

    std::vector<ShapeHandle> handles;
    handles.reserve(graph.prototypes.size());
    for (const auto& proto : graph.prototypes) {
        handles.push_back(proto.shape_handle);
    }

    const Status s = tessellate_batch(kernel, handles, params, out_result.prototype_meshes, cancel);
    if (!s.ok()) {
        return s;
    }

    uint32_t total_tris = 0;
    for (const auto& mesh : out_result.prototype_meshes) {
        total_tris += mesh.triangle_count();
    }
    out_result.tessellated_count = static_cast<uint32_t>(out_result.prototype_meshes.size());
    out_result.total_triangles = total_tris;
    const auto t1 = std::chrono::steady_clock::now();
    out_result.elapsed_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    return Status::Ok();
}

} // namespace cad_geomd
