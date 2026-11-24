#pragma once

#include "abi_version.hpp"

#include <cstddef>
#include <cstdint>
#include <iosfwd>
#include <map>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace cad_geomd {

// Minimal self-contained JSON value tree for zero-dependency framed RPC.
struct JsonValue {
    using Object = std::map<std::string, JsonValue>;
    using Array = std::vector<JsonValue>;
    using Storage = std::variant<std::nullptr_t, bool, double, std::string, Array, Object>;

    Storage data = nullptr;

    JsonValue() = default;
    JsonValue(std::nullptr_t) : data(nullptr) {}
    JsonValue(bool b) : data(b) {}
    JsonValue(int v) : data(static_cast<double>(v)) {}
    JsonValue(int64_t v) : data(static_cast<double>(v)) {}
    JsonValue(uint32_t v) : data(static_cast<double>(v)) {}
    JsonValue(uint64_t v) : data(static_cast<double>(v)) {}
    JsonValue(double v) : data(v) {}
    JsonValue(const char* s) : data(std::string(s)) {}
    JsonValue(std::string s) : data(std::move(s)) {}
    JsonValue(std::string_view s) : data(std::string(s)) {}
    JsonValue(Array a) : data(std::move(a)) {}
    JsonValue(Object o) : data(std::move(o)) {}

    [[nodiscard]] bool is_null() const noexcept { return std::holds_alternative<std::nullptr_t>(data); }
    [[nodiscard]] bool is_bool() const noexcept { return std::holds_alternative<bool>(data); }
    [[nodiscard]] bool is_number() const noexcept { return std::holds_alternative<double>(data); }
    [[nodiscard]] bool is_string() const noexcept { return std::holds_alternative<std::string>(data); }
    [[nodiscard]] bool is_array() const noexcept { return std::holds_alternative<Array>(data); }
    [[nodiscard]] bool is_object() const noexcept { return std::holds_alternative<Object>(data); }

    [[nodiscard]] const Object* as_object() const noexcept { return std::get_if<Object>(&data); }
    [[nodiscard]] const Array* as_array() const noexcept { return std::get_if<Array>(&data); }
    [[nodiscard]] const std::string* as_string() const noexcept { return std::get_if<std::string>(&data); }
    [[nodiscard]] const double* as_number() const noexcept { return std::get_if<double>(&data); }
    [[nodiscard]] const bool* as_bool() const noexcept { return std::get_if<bool>(&data); }

    [[nodiscard]] const JsonValue* find(std::string_view key) const {
        const auto* obj = as_object();
        if (!obj) {
            return nullptr;
        }
        const auto it = obj->find(std::string(key));
        return it == obj->end() ? nullptr : &it->second;
    }
};

Status parse_json(std::string_view text, JsonValue& out_value);
std::string serialize_json(const JsonValue& value);

// Framed wire encoding: [4B magic "CGMD" LE][4B payload_len LE][UTF-8 JSON payload].
std::vector<uint8_t> encode_frame(std::string_view json_payload);
Status decode_frame(
    std::span<const uint8_t> bytes,
    std::string& out_json_payload,
    std::size_t& out_consumed);

Status write_frame(std::ostream& out, std::string_view json_payload);
// Returns Status::Ok() with out_eof=true on clean EOF before a frame header.
Status read_frame(std::istream& in, std::string& out_json_payload, bool& out_eof);

JsonValue make_success_response(const JsonValue& id, JsonValue result);
JsonValue make_error_response(const JsonValue& id, const Status& status);

} // namespace cad_geomd
