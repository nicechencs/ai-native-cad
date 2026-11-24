#pragma once

#include "geomd_protocol.hpp"
#include "ibrep_kernel.hpp"
#include "iformat_adapter.hpp"
#include "worker_pool.hpp"

#include <atomic>
#include <cstdint>
#include <iosfwd>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <unordered_map>

namespace cad_geomd {

// Process-isolated geometry engine service coordinating IBRepKernel,
// PluginRegistry, WorkerPool, and two-stage assembly sessions over framed IPC.
class GeomdService {
public:
    explicit GeomdService(
        std::unique_ptr<IBRepKernel> kernel = nullptr,
        uint32_t worker_threads = 0);

    [[nodiscard]] IBRepKernel& kernel() noexcept { return *kernel_; }
    [[nodiscard]] const IBRepKernel& kernel() const noexcept { return *kernel_; }
    [[nodiscard]] PluginRegistry& plugins() noexcept { return plugins_; }
    [[nodiscard]] const PluginRegistry& plugins() const noexcept { return plugins_; }
    [[nodiscard]] WorkerPool& worker_pool() noexcept { return pool_; }

    // Dispatch one JSON-RPC request and return serialized JSON response.
    std::string handle_request_json(
        std::string_view request_json,
        bool& out_should_shutdown);

    // Run framed request/response loop over arbitrary binary streams (stdio/pipe/socket).
    int run_framed_loop(std::istream& in, std::ostream& out);

private:
    JsonValue dispatch_method(
        const std::string& method,
        const JsonValue& id,
        const JsonValue* params,
        bool& out_should_shutdown);

    std::unique_ptr<IBRepKernel> kernel_;
    PluginRegistry plugins_;
    WorkerPool pool_;

    std::atomic<uint64_t> next_session_id_{1};
    std::mutex sessions_mutex_;
    std::unordered_map<uint64_t, Stage1AssemblyGraph> assembly_sessions_;
};

} // namespace cad_geomd
