#pragma once

#include "abi_version.hpp"
#include "ibrep_kernel.hpp"
#include "iformat_adapter.hpp"

#include <atomic>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <span>
#include <string>
#include <thread>
#include <vector>

namespace cad_geomd {

class CancellationToken {
public:
    CancellationToken() : cancelled_(std::make_shared<std::atomic<bool>>(false)) {}

    void cancel() noexcept {
        if (cancelled_) {
            cancelled_->store(true, std::memory_order_release);
        }
    }

    [[nodiscard]] bool is_cancelled() const noexcept {
        return cancelled_ && cancelled_->load(std::memory_order_acquire);
    }

    static CancellationToken create() { return CancellationToken{}; }

private:
    std::shared_ptr<std::atomic<bool>> cancelled_;
};

struct Stage1AssemblySpec {
    std::string source_path;
    std::string format_id = "step";
    uint32_t instance_count = 0;
    uint32_t prototype_count = 0;
};

// Bounded multi-core C++20 worker pool with lock-free atomic work claiming for
// parallel batch tessellation and two-stage assembly ingestion.
class WorkerPool {
public:
    explicit WorkerPool(uint32_t num_threads = 0);
    ~WorkerPool();

    WorkerPool(const WorkerPool&) = delete;
    WorkerPool& operator=(const WorkerPool&) = delete;
    WorkerPool(WorkerPool&&) = delete;
    WorkerPool& operator=(WorkerPool&&) = delete;

    [[nodiscard]] uint32_t thread_count() const noexcept { return thread_count_; }

    // Execute fn(i) for i in [0, count) across worker threads using lock-free
    // atomic index claiming. Deterministic error reporting (lowest failing index).
    Status parallel_for(
        std::size_t count,
        const std::function<Status(std::size_t)>& fn,
        const CancellationToken* cancel = nullptr);

    // Parallel batch tessellation preserving exact input order.
    Status tessellate_batch(
        const IBRepKernel& kernel,
        std::span<const ShapeHandle> handles,
        const TessellationParams& params,
        std::vector<TriangleMesh>& out_meshes,
        const CancellationToken* cancel = nullptr);

    // Stage 1: fast metadata BOM tree & deduplicated prototype allocation (zero tessellation).
    Status ingest_assembly_stage1(
        const Stage1AssemblySpec& spec,
        IBRepKernel& kernel,
        Stage1AssemblyGraph& out_graph,
        const CancellationToken* cancel = nullptr);

    // Stage 2: parallel per-prototype tessellation with deterministic prototype ordering.
    Status ingest_assembly_stage2(
        const Stage1AssemblyGraph& graph,
        const IBRepKernel& kernel,
        const TessellationParams& params,
        Stage2AssemblyResult& out_result,
        const CancellationToken* cancel = nullptr);

private:
    void worker_loop(std::stop_token stop_token);

    uint32_t thread_count_ = 1;
    std::vector<std::jthread> workers_;
    std::mutex queue_mutex_;
    std::condition_variable_any queue_cv_;
    std::deque<std::function<void()>> tasks_;
    bool stopping_ = false;
};

} // namespace cad_geomd
