#pragma once

#include <cstdint>
#include <string>
#include <string_view>

namespace cad_geomd {

inline constexpr uint32_t CAD_GEOMD_ABI_MAJOR = 1;
inline constexpr uint32_t CAD_GEOMD_ABI_MINOR = 0;

// 4-byte little-endian magic header ("CGMD") for framed IPC messages.
inline constexpr uint32_t CAD_GEOMD_PROTOCOL_MAGIC = 0x43474D44u;
inline constexpr uint32_t CAD_GEOMD_MAX_FRAME_BYTES = 64u * 1024u * 1024u;

enum class ErrorCode : int32_t {
    Ok = 0,
    InvalidArgument = 1,
    NotFound = 2,
    InvalidHandle = 3,
    UnsupportedOperation = 4,
    UnsupportedFormat = 5,
    Cancelled = 6,
    ProtocolError = 7,
    IoError = 8,
    GeometryFailure = 9,
    KernelUnavailable = 10,
    InternalError = 11,
};

inline constexpr std::string_view error_code_name(ErrorCode code) noexcept {
    switch (code) {
        case ErrorCode::Ok:
            return "OK";
        case ErrorCode::InvalidArgument:
            return "INVALID_ARGUMENT";
        case ErrorCode::NotFound:
            return "NOT_FOUND";
        case ErrorCode::InvalidHandle:
            return "INVALID_HANDLE";
        case ErrorCode::UnsupportedOperation:
            return "UNSUPPORTED_OPERATION";
        case ErrorCode::UnsupportedFormat:
            return "UNSUPPORTED_FORMAT";
        case ErrorCode::Cancelled:
            return "CANCELLED";
        case ErrorCode::ProtocolError:
            return "PROTOCOL_ERROR";
        case ErrorCode::IoError:
            return "IO_ERROR";
        case ErrorCode::GeometryFailure:
            return "GEOMETRY_FAILURE";
        case ErrorCode::KernelUnavailable:
            return "KERNEL_UNAVAILABLE";
        case ErrorCode::InternalError:
            return "INTERNAL_ERROR";
    }
    return "UNKNOWN_ERROR";
}

struct Status {
    ErrorCode code = ErrorCode::Ok;
    std::string message;

    [[nodiscard]] bool ok() const noexcept { return code == ErrorCode::Ok; }
    explicit operator bool() const noexcept { return ok(); }

    static Status Ok() { return Status{ErrorCode::Ok, {}}; }

    static Status Error(ErrorCode c, std::string msg) {
        return Status{c, std::move(msg)};
    }
};

struct KernelCapabilities {
    std::string kernel_name;
    std::string kernel_version;
    uint32_t abi_major = CAD_GEOMD_ABI_MAJOR;
    uint32_t abi_minor = CAD_GEOMD_ABI_MINOR;
    bool authoritative_brep = false;
    bool supports_parallel_tessellation = true;
    bool supports_step_ap242 = false;
    bool supports_exact_booleans = false;
    bool supports_two_stage_assembly = true;
    uint32_t max_worker_threads = 1;
};

} // namespace cad_geomd
