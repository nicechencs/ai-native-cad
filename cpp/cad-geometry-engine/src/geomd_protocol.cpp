#include "geomd_protocol.hpp"

#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <istream>
#include <limits>
#include <ostream>
#include <sstream>

namespace cad_geomd {

namespace {

void write_u32_le(uint8_t* dst, uint32_t value) noexcept {
    dst[0] = static_cast<uint8_t>(value & 0xFFu);
    dst[1] = static_cast<uint8_t>((value >> 8u) & 0xFFu);
    dst[2] = static_cast<uint8_t>((value >> 16u) & 0xFFu);
    dst[3] = static_cast<uint8_t>((value >> 24u) & 0xFFu);
}

uint32_t read_u32_le(const uint8_t* src) noexcept {
    return static_cast<uint32_t>(src[0]) |
           (static_cast<uint32_t>(src[1]) << 8u) |
           (static_cast<uint32_t>(src[2]) << 16u) |
           (static_cast<uint32_t>(src[3]) << 24u);
}

class JsonParser {
public:
    explicit JsonParser(std::string_view input) : input_(input) {}

    Status parse(JsonValue& out) {
        skip_ws();
        Status s = parse_value(out, 0);
        if (!s.ok()) {
            return s;
        }
        skip_ws();
        if (pos_ != input_.size()) {
            return Status::Error(ErrorCode::ProtocolError, "unexpected trailing characters in JSON");
        }
        return Status::Ok();
    }

private:
    static constexpr int kMaxDepth = 64;

    void skip_ws() noexcept {
        while (pos_ < input_.size() &&
               (input_[pos_] == ' ' || input_[pos_] == '\t' || input_[pos_] == '\r' ||
                input_[pos_] == '\n')) {
            ++pos_;
        }
    }

    Status parse_value(JsonValue& out, int depth) {
        if (depth > kMaxDepth) {
            return Status::Error(ErrorCode::ProtocolError, "JSON nesting depth exceeded");
        }
        skip_ws();
        if (pos_ >= input_.size()) {
            return Status::Error(ErrorCode::ProtocolError, "unexpected end of JSON input");
        }
        const char ch = input_[pos_];
        if (ch == '{') {
            return parse_object(out, depth + 1);
        }
        if (ch == '[') {
            return parse_array(out, depth + 1);
        }
        if (ch == '"') {
            std::string str;
            Status s = parse_string(str);
            if (!s.ok()) {
                return s;
            }
            out = JsonValue(std::move(str));
            return Status::Ok();
        }
        if (ch == 't') {
            if (input_.substr(pos_, 4) == "true") {
                pos_ += 4;
                out = JsonValue(true);
                return Status::Ok();
            }
            return Status::Error(ErrorCode::ProtocolError, "invalid literal");
        }
        if (ch == 'f') {
            if (input_.substr(pos_, 5) == "false") {
                pos_ += 5;
                out = JsonValue(false);
                return Status::Ok();
            }
            return Status::Error(ErrorCode::ProtocolError, "invalid literal");
        }
        if (ch == 'n') {
            if (input_.substr(pos_, 4) == "null") {
                pos_ += 4;
                out = JsonValue(nullptr);
                return Status::Ok();
            }
            return Status::Error(ErrorCode::ProtocolError, "invalid literal");
        }
        if (ch == '-' || std::isdigit(static_cast<unsigned char>(ch))) {
            return parse_number(out);
        }
        return Status::Error(ErrorCode::ProtocolError, "unexpected character in JSON value");
    }

    Status parse_object(JsonValue& out, int depth) {
        ++pos_; // '{'
        JsonValue::Object obj;
        skip_ws();
        if (pos_ < input_.size() && input_[pos_] == '}') {
            ++pos_;
            out = JsonValue(std::move(obj));
            return Status::Ok();
        }
        while (true) {
            skip_ws();
            if (pos_ >= input_.size() || input_[pos_] != '"') {
                return Status::Error(ErrorCode::ProtocolError, "expected string key in JSON object");
            }
            std::string key;
            Status s = parse_string(key);
            if (!s.ok()) {
                return s;
            }
            skip_ws();
            if (pos_ >= input_.size() || input_[pos_] != ':') {
                return Status::Error(ErrorCode::ProtocolError, "expected ':' after JSON object key");
            }
            ++pos_;
            JsonValue val;
            s = parse_value(val, depth);
            if (!s.ok()) {
                return s;
            }
            obj.insert_or_assign(std::move(key), std::move(val));
            skip_ws();
            if (pos_ < input_.size() && input_[pos_] == ',') {
                ++pos_;
                continue;
            }
            if (pos_ < input_.size() && input_[pos_] == '}') {
                ++pos_;
                out = JsonValue(std::move(obj));
                return Status::Ok();
            }
            return Status::Error(ErrorCode::ProtocolError, "expected ',' or '}' in JSON object");
        }
    }

    Status parse_array(JsonValue& out, int depth) {
        ++pos_; // '['
        JsonValue::Array arr;
        skip_ws();
        if (pos_ < input_.size() && input_[pos_] == ']') {
            ++pos_;
            out = JsonValue(std::move(arr));
            return Status::Ok();
        }
        while (true) {
            JsonValue item;
            Status s = parse_value(item, depth);
            if (!s.ok()) {
                return s;
            }
            arr.push_back(std::move(item));
            skip_ws();
            if (pos_ < input_.size() && input_[pos_] == ',') {
                ++pos_;
                continue;
            }
            if (pos_ < input_.size() && input_[pos_] == ']') {
                ++pos_;
                out = JsonValue(std::move(arr));
                return Status::Ok();
            }
            return Status::Error(ErrorCode::ProtocolError, "expected ',' or ']' in JSON array");
        }
    }

    Status parse_string(std::string& out) {
        ++pos_; // '"'
        out.clear();
        while (pos_ < input_.size()) {
            const char ch = input_[pos_++];
            if (ch == '"') {
                return Status::Ok();
            }
            if (static_cast<unsigned char>(ch) < 0x20u) {
                return Status::Error(ErrorCode::ProtocolError, "unescaped control character in JSON string");
            }
            if (ch != '\\') {
                out.push_back(ch);
                continue;
            }
            if (pos_ >= input_.size()) {
                return Status::Error(ErrorCode::ProtocolError, "unterminated escape in JSON string");
            }
            const char esc = input_[pos_++];
            switch (esc) {
                case '"': out.push_back('"'); break;
                case '\\': out.push_back('\\'); break;
                case '/': out.push_back('/'); break;
                case 'b': out.push_back('\b'); break;
                case 'f': out.push_back('\f'); break;
                case 'n': out.push_back('\n'); break;
                case 'r': out.push_back('\r'); break;
                case 't': out.push_back('\t'); break;
                default:
                    return Status::Error(ErrorCode::ProtocolError, "unsupported escape sequence in JSON string");
            }
        }
        return Status::Error(ErrorCode::ProtocolError, "unterminated JSON string");
    }

    Status parse_number(JsonValue& out) {
        const std::size_t start = pos_;
        if (input_[pos_] == '-') {
            ++pos_;
        }
        if (pos_ >= input_.size() || !std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
            return Status::Error(ErrorCode::ProtocolError, "invalid JSON number");
        }
        while (pos_ < input_.size() && std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
            ++pos_;
        }
        if (pos_ < input_.size() && input_[pos_] == '.') {
            ++pos_;
            if (pos_ >= input_.size() || !std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
                return Status::Error(ErrorCode::ProtocolError, "invalid fractional JSON number");
            }
            while (pos_ < input_.size() && std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
                ++pos_;
            }
        }
        if (pos_ < input_.size() && (input_[pos_] == 'e' || input_[pos_] == 'E')) {
            ++pos_;
            if (pos_ < input_.size() && (input_[pos_] == '+' || input_[pos_] == '-')) {
                ++pos_;
            }
            if (pos_ >= input_.size() || !std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
                return Status::Error(ErrorCode::ProtocolError, "invalid exponent in JSON number");
            }
            while (pos_ < input_.size() && std::isdigit(static_cast<unsigned char>(input_[pos_]))) {
                ++pos_;
            }
        }
        const std::string token(input_.substr(start, pos_ - start));
        char* end_ptr = nullptr;
        const double val = std::strtod(token.c_str(), &end_ptr);
        if (end_ptr != token.c_str() + token.size() || !std::isfinite(val)) {
            return Status::Error(ErrorCode::ProtocolError, "out-of-range JSON number");
        }
        out = JsonValue(val);
        return Status::Ok();
    }

    std::string_view input_;
    std::size_t pos_ = 0;
};

void escape_json_string(std::string_view s, std::string& out) {
    out.push_back('"');
    for (const char ch : s) {
        switch (ch) {
            case '"': out.append("\\\""); break;
            case '\\': out.append("\\\\"); break;
            case '\b': out.append("\\b"); break;
            case '\f': out.append("\\f"); break;
            case '\n': out.append("\\n"); break;
            case '\r': out.append("\\r"); break;
            case '\t': out.append("\\t"); break;
            default:
                if (static_cast<unsigned char>(ch) < 0x20u) {
                    char buf[8];
                    std::snprintf(buf, sizeof(buf), "\\u%04x", static_cast<unsigned int>(static_cast<unsigned char>(ch)));
                    out.append(buf);
                } else {
                    out.push_back(ch);
                }
                break;
        }
    }
    out.push_back('"');
}

void serialize_into(const JsonValue& value, std::string& out) {
    if (value.is_null()) {
        out.append("null");
        return;
    }
    if (const auto* b = value.as_bool()) {
        out.append(*b ? "true" : "false");
        return;
    }
    if (const auto* num = value.as_number()) {
        if (!std::isfinite(*num)) {
            out.append("null");
            return;
        }
        const double rounded = std::trunc(*num);
        if (rounded == *num && *num >= -9007199254740992.0 && *num <= 9007199254740992.0) {
            out.append(std::to_string(static_cast<int64_t>(rounded)));
        } else {
            char buf[64];
            std::snprintf(buf, sizeof(buf), "%.15g", *num);
            out.append(buf);
        }
        return;
    }
    if (const auto* s = value.as_string()) {
        escape_json_string(*s, out);
        return;
    }
    if (const auto* arr = value.as_array()) {
        out.push_back('[');
        for (std::size_t i = 0; i < arr->size(); ++i) {
            if (i > 0u) {
                out.push_back(',');
            }
            serialize_into((*arr)[i], out);
        }
        out.push_back(']');
        return;
    }
    if (const auto* obj = value.as_object()) {
        out.push_back('{');
        bool first = true;
        for (const auto& kv : *obj) {
            if (!first) {
                out.push_back(',');
            }
            first = false;
            escape_json_string(kv.first, out);
            out.push_back(':');
            serialize_into(kv.second, out);
        }
        out.push_back('}');
    }
}

} // namespace

Status parse_json(std::string_view text, JsonValue& out_value) {
    JsonParser parser(text);
    return parser.parse(out_value);
}

std::string serialize_json(const JsonValue& value) {
    std::string out;
    out.reserve(128);
    serialize_into(value, out);
    return out;
}

std::vector<uint8_t> encode_frame(std::string_view json_payload) {
    std::vector<uint8_t> frame(8u + json_payload.size());
    write_u32_le(frame.data(), CAD_GEOMD_PROTOCOL_MAGIC);
    write_u32_le(frame.data() + 4u, static_cast<uint32_t>(json_payload.size()));
    if (!json_payload.empty()) {
        std::copy(json_payload.begin(), json_payload.end(), frame.begin() + 8);
    }
    return frame;
}

Status decode_frame(
    std::span<const uint8_t> bytes,
    std::string& out_json_payload,
    std::size_t& out_consumed) {
    out_consumed = 0;
    if (bytes.size() < 8u) {
        return Status::Error(ErrorCode::ProtocolError, "incomplete frame header");
    }
    const uint32_t magic = read_u32_le(bytes.data());
    if (magic != CAD_GEOMD_PROTOCOL_MAGIC) {
        return Status::Error(ErrorCode::ProtocolError, "invalid frame magic");
    }
    const uint32_t payload_len = read_u32_le(bytes.data() + 4u);
    if (payload_len > CAD_GEOMD_MAX_FRAME_BYTES) {
        return Status::Error(ErrorCode::ProtocolError, "frame payload exceeds maximum size");
    }
    if (bytes.size() < 8u + static_cast<std::size_t>(payload_len)) {
        return Status::Error(ErrorCode::ProtocolError, "truncated frame payload");
    }
    out_json_payload.assign(
        reinterpret_cast<const char*>(bytes.data() + 8u),
        static_cast<std::size_t>(payload_len));
    out_consumed = 8u + static_cast<std::size_t>(payload_len);
    return Status::Ok();
}

Status write_frame(std::ostream& out, std::string_view json_payload) {
    if (json_payload.size() > CAD_GEOMD_MAX_FRAME_BYTES) {
        return Status::Error(ErrorCode::ProtocolError, "outgoing frame exceeds maximum size");
    }
    uint8_t header[8];
    write_u32_le(header, CAD_GEOMD_PROTOCOL_MAGIC);
    write_u32_le(header + 4u, static_cast<uint32_t>(json_payload.size()));
    out.write(reinterpret_cast<const char*>(header), 8);
    if (!json_payload.empty()) {
        out.write(json_payload.data(), static_cast<std::streamsize>(json_payload.size()));
    }
    out.flush();
    if (!out.good()) {
        return Status::Error(ErrorCode::IoError, "failed to write framed response");
    }
    return Status::Ok();
}

Status read_frame(std::istream& in, std::string& out_json_payload, bool& out_eof) {
    out_eof = false;
    out_json_payload.clear();

    uint8_t header[8];
    in.read(reinterpret_cast<char*>(header), 8);
    const std::streamsize got = in.gcount();
    if (got == 0 && in.eof()) {
        out_eof = true;
        return Status::Ok();
    }
    if (got != 8) {
        return Status::Error(ErrorCode::ProtocolError, "truncated frame header on stream");
    }
    const uint32_t magic = read_u32_le(header);
    if (magic != CAD_GEOMD_PROTOCOL_MAGIC) {
        return Status::Error(ErrorCode::ProtocolError, "invalid frame magic on stream");
    }
    const uint32_t payload_len = read_u32_le(header + 4u);
    if (payload_len > CAD_GEOMD_MAX_FRAME_BYTES) {
        return Status::Error(ErrorCode::ProtocolError, "incoming frame exceeds maximum size");
    }
    out_json_payload.resize(payload_len);
    if (payload_len > 0u) {
        in.read(out_json_payload.data(), static_cast<std::streamsize>(payload_len));
        if (in.gcount() != static_cast<std::streamsize>(payload_len)) {
            return Status::Error(ErrorCode::ProtocolError, "truncated frame payload on stream");
        }
    }
    return Status::Ok();
}

JsonValue make_success_response(const JsonValue& id, JsonValue result) {
    JsonValue::Object resp;
    resp["ok"] = JsonValue(true);
    resp["id"] = id;
    resp["result"] = std::move(result);
    return JsonValue(std::move(resp));
}

JsonValue make_error_response(const JsonValue& id, const Status& status) {
    JsonValue::Object err;
    err["code"] = JsonValue(static_cast<int>(status.code));
    err["name"] = JsonValue(error_code_name(status.code));
    err["message"] = JsonValue(status.message);

    JsonValue::Object resp;
    resp["ok"] = JsonValue(false);
    resp["id"] = id;
    resp["error"] = JsonValue(std::move(err));
    return JsonValue(std::move(resp));
}

} // namespace cad_geomd
