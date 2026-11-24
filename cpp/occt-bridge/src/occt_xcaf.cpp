#include "occt_xcaf.h"

#include <cstdio>

#if defined(AI_NATIVE_CAD_HAS_OCCT)

#include <BRepPrimAPI_MakeBox.hxx>
#include <DESTEP_Parameters.hxx>
#include <NCollection_Sequence.hxx>
#include <Quantity_ColorRGBA.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <STEPCAFControl_Writer.hxx>
#include <STEPConstruct_ExternRefs.hxx>
#include <Standard_Failure.hxx>
#include <StepData_StepModel.hxx>
#include <StepDimTol_Datum.hxx>
#include <TCollection_HAsciiString.hxx>
#include <TDF_Label.hxx>
#include <TDataStd_Name.hxx>
#include <TDocStd_Document.hxx>
#include <TopExp_Explorer.hxx>
#include <TopLoc_Location.hxx>
#include <TopoDS_Face.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDimTolObjects_DatumObject.hxx>
#include <XCAFDimTolObjects_DimensionObject.hxx>
#include <XCAFDimTolObjects_GeomToleranceObject.hxx>
#include <XCAFDoc_ColorTool.hxx>
#include <XCAFDoc_Datum.hxx>
#include <XCAFDoc_DimTolTool.hxx>
#include <XCAFDoc_Dimension.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_GeomTolerance.hxx>
#include <XCAFDoc_GraphNode.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <gp_Trsf.hxx>

#include <algorithm>
#include <array>
#include <charconv>
#include <cmath>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <locale>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

// This source follows occt_bridge.cpp in the unity group and shares its
// kernel lock, UTF-8 helpers, and exchange messenger guard.
namespace {
namespace occt_xcaf {

size_t utf8_sequence_size(std::string_view text) {
    const auto lead = static_cast<unsigned char>(text.front());
    if (lead < 0xc2 || lead > 0xf4) return 0;
    const size_t size = lead < 0xe0 ? 2 : lead < 0xf0 ? 3 : 4;
    if (text.size() < size) return 0;
    const auto second = static_cast<unsigned char>(text[1]);
    // Exclude overlong encodings, UTF-16 surrogates, and values above U+10FFFF.
    if ((lead == 0xe0 && second < 0xa0) || (lead == 0xed && second > 0x9f) ||
        (lead == 0xf0 && second < 0x90) || (lead == 0xf4 && second > 0x8f)) {
        return 0;
    }
    for (size_t i = 1; i < size; ++i) {
        if ((static_cast<unsigned char>(text[i]) & 0xc0) != 0x80) return 0;
    }
    return size;
}

bool is_valid_utf8(std::string_view text) {
    while (!text.empty()) {
        const size_t count = static_cast<unsigned char>(text.front()) < 0x80 ? 1 : utf8_sequence_size(text);
        if (!count) return false;
        text.remove_prefix(count);
    }
    return true;
}

enum class PmiKind { Dimension, GeometricTolerance, Datum };

struct ExportPart {
    std::string name;
    std::array<double, 3> size;
    std::array<double, 3> translation;
    std::array<double, 4> color;
};

struct ExportPmi {
    std::string name;
    PmiKind kind = PmiKind::Dimension;
    std::string value;
    double magnitude = 0;
};

struct ExportSpec {
    std::string name;
    std::vector<ExportPart> parts;
    std::vector<ExportPmi> pmi;
};

// The export schema is fixed, so parse it directly without a generic JSON tree.
class ExportJson {
public:
    explicit ExportJson(std::string_view input) : rest_(input) {}

    ExportSpec parse() {
        ExportSpec spec;
        object(std::array<std::string_view, 3>{"assembly_name", "parts", "pmi_items"},
               [&](size_t field) {
            switch (field) {
                case 0: spec.name = text(); break;
                case 1: array([&] { spec.parts.push_back(part()); }); break;
                case 2: array([&] { spec.pmi.push_back(pmi()); }); break;
            }
        });
        space();
        if (!rest_.empty()) invalid("Trailing data in STEP export JSON");
        if (spec.parts.empty()) invalid("STEP export requires at least one part");
        std::unordered_set<std::string_view> datum_ids;
        for (const auto& item : spec.pmi) {
            if (item.kind == PmiKind::Datum && !datum_ids.insert(item.value).second) {
                invalid("Duplicate STEP export datum identifier");
            }
        }
        return spec;
    }

private:
    std::string_view rest_;

    [[noreturn]] static void invalid(const char* message) { throw std::invalid_argument(message); }

    void space() {
        while (!rest_.empty() && std::string_view(" \t\r\n").find(rest_.front()) != std::string_view::npos) {
            rest_.remove_prefix(1);
        }
    }

    bool take(char token) {
        if (!rest_.starts_with(token)) return false;
        rest_.remove_prefix(1);
        return true;
    }

    void expect(char token) {
        space();
        if (!take(token)) invalid("Unexpected token in STEP export JSON");
    }

    template <typename Read>
    void container(char open, char close, Read read) {
        expect(open);
        space();
        if (take(close)) return;
        do {
            read();
            space();
            if (take(close)) return;
        } while (take(','));
        invalid("Expected a comma or closing delimiter in STEP export JSON");
    }

    template <typename Read>
    void array(Read read) { container('[', ']', read); }

    template <size_t N, typename Read>
    void object(const std::array<std::string_view, N>& keys, Read read) {
        std::array<bool, N> seen{};
        container('{', '}', [&] {
            const auto key = text();
            const auto it = std::find(keys.begin(), keys.end(), key);
            if (it == keys.end()) invalid("Unknown field in STEP export JSON");
            const auto index = static_cast<size_t>(it - keys.begin());
            if (std::exchange(seen[index], true)) invalid("Duplicate field in STEP export JSON");
            expect(':');
            read(index);
        });
        if (std::find(seen.begin(), seen.end(), false) != seen.end()) {
            invalid("Missing field in STEP export JSON");
        }
    }

    char next() {
        if (rest_.empty()) invalid("Unterminated string in STEP export JSON");
        const char ch = rest_.front();
        rest_.remove_prefix(1);
        return ch;
    }

    unsigned hex_unit() {
        if (rest_.size() < 4) invalid("Incomplete Unicode escape in STEP export JSON");
        unsigned unit = 0;
        const auto end = rest_.data() + 4;
        const auto result = std::from_chars(rest_.data(), end, unit, 16);
        if (result.ec != std::errc{} || result.ptr != end) invalid("Invalid Unicode escape in STEP export JSON");
        rest_.remove_prefix(4);
        return unit;
    }

    void append_unicode(std::string& value) {
        unsigned code = hex_unit();
        if (code >= 0xd800 && code <= 0xdbff) {
            if (!take('\\') || !take('u')) invalid("Missing low surrogate in STEP export JSON");
            const unsigned low = hex_unit();
            if (low < 0xdc00 || low > 0xdfff) invalid("Invalid low surrogate in STEP export JSON");
            code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
        }
        if (code == 0 || (code >= 0xdc00 && code <= 0xdfff)) invalid("Invalid Unicode in STEP export text");
        const auto continuation = [&](unsigned shift) {
            value.push_back(static_cast<char>(0x80 | ((code >> shift) & 0x3f)));
        };
        if (code < 0x80) {
            value.push_back(static_cast<char>(code));
        } else {
            if (code >= 0x10000) {
                value.push_back(static_cast<char>(0xf0 | (code >> 18)));
                continuation(12);
            } else if (code >= 0x800) {
                value.push_back(static_cast<char>(0xe0 | (code >> 12)));
            } else {
                value.push_back(static_cast<char>(0xc0 | (code >> 6)));
            }
            if (code >= 0x800) continuation(6);
            continuation(0);
        }
    }

    std::string text() {
        expect('"');
        std::string value;
        while (!rest_.empty()) {
            const auto byte = static_cast<unsigned char>(rest_.front());
            if (byte >= 0x80) {
                const size_t count = utf8_sequence_size(rest_);
                if (!count) invalid("Invalid UTF-8 in STEP export JSON");
                value.append(rest_.substr(0, count));
                rest_.remove_prefix(count);
                continue;
            }
            const char ch = next();
            if (ch == '"') {
                if (value.empty()) invalid("Empty string in STEP export specification");
                return value;
            }
            if (byte < 0x20) invalid("Unescaped control character in STEP export JSON");
            if (ch != '\\') {
                value.push_back(ch);
                continue;
            }
            const char escape = next();
            constexpr std::string_view escapes = "\"\\/bfnrt";
            const auto index = escapes.find(escape);
            if (index != std::string_view::npos) value.push_back("\"\\/\b\f\n\r\t"[index]);
            else if (escape == 'u') append_unicode(value);
            else invalid("Invalid escape in STEP export JSON");
        }
        invalid("Unterminated string in STEP export JSON");
    }

    static double decimal(std::string_view token) {
        if (token.starts_with('+') && token.size() > 1 && token[1] != '-') token.remove_prefix(1);
        double value;
        const auto end = token.data() + token.size();
        const auto result = std::from_chars(token.data(), end, value);
        if (result.ec != std::errc{} || result.ptr != end || !std::isfinite(value)) {
            invalid("Expected a finite number in STEP export specification");
        }
        return value;
    }

    void digits() {
        const auto start = rest_.size();
        while (!rest_.empty() && rest_.front() >= '0' && rest_.front() <= '9') rest_.remove_prefix(1);
        if (rest_.size() == start) invalid("Missing digits in STEP export JSON number");
    }

    double number() {
        space();
        const auto start = rest_;
        take('-');
        if (!take('0')) digits();
        if (take('.')) digits();
        if (take('e') || take('E')) {
            if (!take('+')) take('-');
            digits();
        }
        return decimal(start.substr(0, start.size() - rest_.size()));
    }

    template <size_t N>
    std::array<double, N> numbers() {
        std::array<double, N> result{};
        size_t count = 0;
        array([&] {
            if (count == N) invalid("Too many coordinates in STEP export JSON");
            result[count++] = number();
        });
        if (count != N) invalid("Missing coordinates in STEP export JSON");
        return result;
    }

    ExportPart part() {
        ExportPart part{};
        object(std::array<std::string_view, 4>{"name", "size_mm", "translation_mm", "color_rgba"},
               [&](size_t field) {
            switch (field) {
                case 0: part.name = text(); break;
                case 1: part.size = numbers<3>(); break;
                case 2: part.translation = numbers<3>(); break;
                case 3: part.color = numbers<4>(); break;
            }
        });
        for (double size : part.size) {
            if (size <= 0) invalid("STEP export box dimensions must be positive");
        }
        for (double channel : part.color) {
            if (channel < 0 || channel > 1) invalid("STEP export color channels must be in [0,1]");
        }
        return part;
    }

    ExportPmi pmi() {
        ExportPmi pmi;
        object(std::array<std::string_view, 3>{"name", "kind", "value_text"}, [&](size_t field) {
            switch (field) {
                case 0: pmi.name = text(); break;
                case 1: {
                    const auto kind = text();
                    if (kind == "dimension") pmi.kind = PmiKind::Dimension;
                    else if (kind == "geometric_tolerance") pmi.kind = PmiKind::GeometricTolerance;
                    else if (kind == "datum") pmi.kind = PmiKind::Datum;
                    else invalid("Unknown PMI kind in STEP export JSON");
                    break;
                }
                case 2: pmi.value = text(); break;
            }
        });
        if (pmi.kind != PmiKind::Datum) {
            pmi.magnitude = decimal(pmi.value);
            if (pmi.magnitude < 0) invalid("STEP export PMI magnitude must be nonnegative");
        }
        return pmi;
    }
};

std::string ascii_text(const Handle(TCollection_HAsciiString)& text) {
    if (text.IsNull()) return {};
    return {text->ToCString(), static_cast<size_t>(text->Length())};
}

template <typename Visit>
void visit_step_datums(const StepData_StepModel& model, Visit visit) {
    for (int i = 1; i <= model.NbEntities(); ++i) {
        const auto datum = Handle(StepDimTol_Datum)::DownCast(model.Entity(i));
        if (!datum.IsNull()) visit(*datum);
    }
}

void write_step_datum_names(const StepData_StepModel& model, const std::vector<ExportPmi>& items) {
    std::unordered_map<std::string_view, std::string_view> names;
    for (const auto& item : items) {
        if (item.kind == PmiKind::Datum) names.emplace(item.value, item.name);
    }
    if (names.empty()) return;

    // The XCAF writer transfers datum identifiers and features and leaves
    // AP242's datum name empty. Set that field on the transferred entities.
    visit_step_datums(model, [&](StepDimTol_Datum& datum) {
        const auto name = names.find(ascii_text(datum.Identification()));
        if (name != names.end()) {
            datum.SetName(new TCollection_HAsciiString(name->second.data()));
            names.erase(name);
        }
    });
    if (!names.empty()) throw std::runtime_error("Could not transfer STEP datum metadata");
}

void add_export_pmi(const TDocStd_Document& doc, const XCAFDoc_ShapeTool& shapes,
                    const TDF_Label& part, BRepPrimAPI_MakeBox& box,
                    const std::vector<ExportPmi>& items) {
    const auto tool = XCAFDoc_DocumentTool::DimTolTool(doc.Main());
    const auto edge = TopExp_Explorer(box.BottomFace(), TopAbs_EDGE).Current();
    const auto dimension_feature = shapes.AddSubShape(part, edge);
    const auto tolerance_feature = shapes.AddSubShape(part, box.LeftFace());
    const auto datum_feature = shapes.AddSubShape(part, box.TopFace());
    for (const auto& item : items) {
        const Handle(TCollection_HAsciiString) name = new TCollection_HAsciiString(item.name.c_str());
        // AP242 carries the supplied name in the annotation callout. The
        // semantic dimension name remains the standardized dimension type.
        switch (item.kind) {
            case PmiKind::Dimension: {
                const auto label = tool->AddDimension();
                Handle(XCAFDimTolObjects_DimensionObject) object = new XCAFDimTolObjects_DimensionObject;
                object->SetType(XCAFDimTolObjects_DimensionType_Size_CurveLength);
                object->SetValue(item.magnitude);
                object->SetPresentation(edge, name);
                XCAFDoc_Dimension::Set(label)->SetObject(object);
                tool->SetDimension(dimension_feature, label);
                break;
            }
            case PmiKind::GeometricTolerance: {
                const auto label = tool->AddGeomTolerance();
                Handle(XCAFDimTolObjects_GeomToleranceObject) object = new XCAFDimTolObjects_GeomToleranceObject;
                object->SetType(XCAFDimTolObjects_GeomToleranceType_Flatness);
                object->SetValue(item.magnitude);
                object->SetPresentation(TopExp_Explorer(box.LeftFace(), TopAbs_EDGE).Current(), name);
                XCAFDoc_GeomTolerance::Set(label)->SetObject(object);
                tool->SetGeomTolerance(tolerance_feature, label);
                break;
            }
            case PmiKind::Datum: {
                const Handle(TCollection_HAsciiString) identifier = new TCollection_HAsciiString(item.value.c_str());
                const auto label = tool->AddDatum(name, new TCollection_HAsciiString, identifier);
                Handle(XCAFDimTolObjects_DatumObject) object = new XCAFDimTolObjects_DatumObject;
                object->SetName(identifier);
                XCAFDoc_Datum::Set(label)->SetObject(object);
                NCollection_Sequence<TDF_Label> features;
                features.Append(datum_feature);
                tool->SetDatum(features, label);
                break;
            }
        }
    }
}

void export_document(const std::filesystem::path& path, const ExportSpec& spec) {
    Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
    XCAFApp_Application::GetApplication()->InitDocument(doc);
    XCAFDoc_DocumentTool::SetLengthUnit(doc, 0.001);
    const auto shapes = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
    const auto colors = XCAFDoc_DocumentTool::ColorTool(doc->Main());
    const auto root = shapes->NewShape();
    TDataStd_Name::Set(root, TCollection_ExtendedString(spec.name.c_str(), true));
    for (const auto& part : spec.parts) {
        const auto [dx, dy, dz] = part.size;
        BRepPrimAPI_MakeBox box(dx, dy, dz);
        const auto definition = shapes->AddShape(box.Shape(), false);
        const TCollection_ExtendedString name(part.name.c_str(), true);
        TDataStd_Name::Set(definition, name);
        const auto [r, g, b, a] = part.color;
        colors->SetColor(definition, Quantity_ColorRGBA(static_cast<float>(r), static_cast<float>(g),
                                                        static_cast<float>(b), static_cast<float>(a)),
                         XCAFDoc_ColorSurf);
        const auto [x, y, z] = part.translation;
        gp_Trsf transform;
        transform.SetTranslation(gp_Vec(x, y, z));
        const auto occurrence = shapes->AddComponent(root, definition, TopLoc_Location(transform));
        if (occurrence.IsNull()) throw std::runtime_error("Could not create STEP assembly component");
        TDataStd_Name::Set(occurrence, name);
        if (&part == &spec.parts.front() && !spec.pmi.empty()) {
            add_export_pmi(*doc, *shapes, definition, box, spec.pmi);
        }
    }
    shapes->UpdateAssemblies();

    STEPCAFControl_Writer writer;
    writer.SetColorMode(true);
    writer.SetNameMode(true);
    writer.SetDimTolMode(true);
    DESTEP_Parameters parameters;
    parameters.WriteSchema = DESTEP_Parameters::WriteMode_StepSchema_AP242DIS;
    if (!writer.Transfer(doc, parameters)) throw std::runtime_error("Could not transfer STEP assembly");
    write_step_datum_names(*writer.ChangeWriter().Model(), spec.pmi);

    std::ofstream output(path, std::ios::binary);
    if (!output) throw std::runtime_error("Could not open STEP output file");
    const auto status = writer.WriteStream(output);
    if (status != IFSelect_RetDone) throw std::runtime_error(step_write_failure(status));
    output.close();
    if (!output) throw std::runtime_error("Could not finish writing STEP output file");
}

void write_string(std::ostream& json, std::string_view text) {
    constexpr char hex[] = "0123456789abcdef";
    json << '"';
    while (!text.empty()) {
        const auto byte = static_cast<unsigned char>(text.front());
        size_t consumed = 1;
        if (byte < 0x20) {
            json << "\\u00" << hex[byte >> 4] << hex[byte & 0xf];
        } else if (byte == '"' || byte == '\\') {
            json << '\\' << text.front();
        } else if (byte < 0x80) {
            json << text.front();
        } else if (const size_t length = utf8_sequence_size(text)) {
            json << text.substr(0, length);
            consumed = length;
        } else {
            json << "\\ufffd";
        }
        text.remove_prefix(consumed);
    }
    json << '"';
}

std::string number_text(double value) {
    if (!std::isfinite(value)) return {};
    char buffer[64];
    const auto result = std::to_chars(buffer, buffer + sizeof buffer, value);
    if (result.ec != std::errc{}) throw std::runtime_error("Could not format STEP numeric metadata");
    return {buffer, result.ptr};
}

void write_number(std::ostream& json, double value) {
    const auto text = number_text(value);
    if (text.empty()) throw std::runtime_error("Non-finite STEP placement or color");
    json << text;
}

void write_location(std::ostream& json, const TopLoc_Location& location) {
    const gp_Trsf& transform = location.Transformation();
    json << '[';
    for (int col = 1; col <= 4; ++col) {
        if (col > 1) json << ',';
        for (int row = 1; row <= 3; ++row) {
            write_number(json, transform.Value(row, col));
            json << ',';
        }
        json << (col == 4 ? 1 : 0);
    }
    json << ']';
}

bool read_color(const TDF_Label& label, Quantity_ColorRGBA& color) {
    return XCAFDoc_ColorTool::GetColor(label, XCAFDoc_ColorSurf, color) ||
           XCAFDoc_ColorTool::GetColor(label, XCAFDoc_ColorGen, color) ||
           XCAFDoc_ColorTool::GetColor(label, XCAFDoc_ColorCurv, color);
}

bool read_usage_color(const NCollection_Sequence<TDF_Label>& path, Quantity_ColorRGBA& color) {
    if (path.Length() < 2) return false;
    // Higher-usage styles can target a nested occurrence even when two
    // occurrences of the definition have identical world placements.
    NCollection_Sequence<TDF_Label> usage(path);
    while (usage.Length() > 1) {
        Handle(XCAFDoc_GraphNode) style;
        if (XCAFDoc_ShapeTool::FindSHUO(usage, style) && read_color(style->Label(), color)) return true;
        usage.Remove(1);
    }
    return false;
}

struct AssemblyWriter {
    std::ostream& json;
    NCollection_Sequence<TDF_Label> component_path;
    size_t total = 0;
    size_t colored = 0;

    void append(const TDF_Label& occurrence, std::optional<size_t> parent, unsigned depth) {
        if (depth >= 256) throw std::runtime_error("STEP assembly hierarchy is too deep or cyclic");
        TDF_Label referred;
        const bool is_reference = XCAFDoc_ShapeTool::GetReferredShape(occurrence, referred);
        const auto& definition = is_reference ? referred : occurrence;
        const bool is_assembly = XCAFDoc_ShapeTool::IsAssembly(definition);
        const bool is_component = XCAFDoc_ShapeTool::IsComponent(occurrence);
        if (is_component) component_path.Append(occurrence);
        auto name = label_name(occurrence);
        if (name.empty() && is_reference) name = label_name(definition);

        const size_t id = total++;
        if (id != 0) json << ',';
        json << "{\"id\":" << id << ",\"parent_id\":";
        if (parent) json << *parent;
        else json << "null";
        json << ",\"name\":";
        write_string(json, name);
        json << ",\"is_assembly\":" << is_assembly << ",\"is_reference\":" << is_reference
             << ",\"location_matrix_4x4\":";
        write_location(json, XCAFDoc_ShapeTool::GetLocation(occurrence));
        json << ",\"color_rgba\":";
        Quantity_ColorRGBA color;
        if ((is_component && read_usage_color(component_path, color)) || read_color(occurrence, color) ||
            (is_reference && read_color(definition, color))) {
            ++colored;
            const auto& rgb = color.GetRGB();
            const double channels[] = {rgb.Red(), rgb.Green(), rgb.Blue(), color.Alpha()};
            json << '[';
            for (int i = 0; i < 4; ++i) {
                if (i != 0) json << ',';
                write_number(json, channels[i]);
            }
            json << ']';
        } else {
            json << "null";
        }
        json << '}';

        NCollection_Sequence<TDF_Label> children;
        if (is_assembly && XCAFDoc_ShapeTool::GetComponents(definition, children)) {
            for (const auto& child : children) append(child, id, depth + 1);
        }
        if (is_component) component_path.Remove(component_path.Length());
    }
};

template <typename Object>
std::string pmi_name(const TDF_Label& label, const Handle(Object)& object) {
    auto name = ascii_text(object->GetPresentationName());
    if (name.empty()) name = ascii_text(object->GetSemanticName());
    return name.empty() ? label_name(label) : name;
}

std::string dimension_text(const XCAFDimTolObjects_DimensionObject& dimension) {
    if (dimension.GetValues().IsNull()) return {};
    if (dimension.IsDimWithRange()) {
        return number_text(dimension.GetLowerBound()) + " .. " + number_text(dimension.GetUpperBound());
    }
    auto text = number_text(dimension.GetValue());
    if (dimension.IsDimWithPlusMinusTolerance()) {
        text += " (lower tolerance " + number_text(dimension.GetLowerTolValue()) +
                ", upper tolerance " + number_text(dimension.GetUpperTolValue()) + ')';
    }
    return text;
}

struct PmiWriter {
    std::ostream& json;
    size_t count = 0;

    void append(std::string_view name, std::string_view kind, std::string_view value) {
        if (count++ != 0) json << ',';
        json << "{\"name\":";
        write_string(json, name);
        json << ",\"kind\":";
        write_string(json, kind);
        json << ",\"value_text\":";
        write_string(json, value);
        json << '}';
    }

    void write(const XCAFDoc_DimTolTool& tool, const StepData_StepModel& model) {
        NCollection_Sequence<TDF_Label> labels;
        tool.GetDimensionLabels(labels);
        for (const auto& label : labels) {
            Handle(XCAFDoc_Dimension) attribute;
            if (!label.FindAttribute(XCAFDoc_Dimension::GetID(), attribute)) continue;
            const auto object = attribute->GetObject();
            if (!object.IsNull()) append(pmi_name(label, object), "dimension", dimension_text(*object));
        }
        labels.Clear();
        tool.GetGeomToleranceLabels(labels);
        for (const auto& label : labels) {
            Handle(XCAFDoc_GeomTolerance) attribute;
            if (!label.FindAttribute(XCAFDoc_GeomTolerance::GetID(), attribute)) continue;
            const auto object = attribute->GetObject();
            if (object.IsNull()) continue;
            const auto value = object->GetType() == XCAFDimTolObjects_GeomToleranceType_None
                ? std::string{} : number_text(object->GetValue());
            append(pmi_name(label, object), "geometric_tolerance", value);
        }
        labels.Clear();
        tool.GetDatumLabels(labels);
        std::unordered_set<std::string> imported_ids;
        for (const auto& label : labels) {
            Handle(XCAFDoc_Datum) attribute;
            if (!label.FindAttribute(XCAFDoc_Datum::GetID(), attribute)) continue;
            const auto object = attribute->GetObject();
            auto value = ascii_text(attribute->GetIdentification());
            if (value.empty() && !object.IsNull()) value = ascii_text(object->GetName());
            auto name = ascii_text(attribute->GetName());
            if (name.empty()) name = value;
            if (name.empty()) name = object.IsNull() ? label_name(label) : pmi_name(label, object);
            append(name, "datum", value);
            if (!value.empty()) imported_ids.insert(std::move(value));
        }
        // OCCT populates XCAF datum labels when a tolerance references them.
        // Include standalone AP242 datum features directly from the STEP model.
        visit_step_datums(model, [&](const StepDimTol_Datum& datum) {
            const auto value = ascii_text(datum.Identification());
            if (value.empty() || imported_ids.contains(value)) return;
            const auto name = ascii_text(datum.Name());
            append(name.empty() ? value : name, "datum", value);
        });
    }
};

std::string document_json(const TDocStd_Document& doc, const StepData_StepModel& model) {
    const auto shapes = XCAFDoc_DocumentTool::ShapeTool(doc.Main());
    XCAFDoc_DocumentTool::ColorTool(doc.Main());
    const auto dim_tol = XCAFDoc_DocumentTool::DimTolTool(doc.Main());
    NCollection_Sequence<TDF_Label> roots;
    shapes->GetFreeShapes(roots);

    std::ostringstream json;
    json.imbue(std::locale::classic());
    json.exceptions(std::ios::badbit | std::ios::failbit);
    json << std::boolalpha << "{\"assembly_nodes\":[";
    AssemblyWriter assembly{json};
    for (const auto& root : roots) assembly.append(root, std::nullopt, 0);
    json << "],\"pmi_items\":[";
    PmiWriter pmi{json};
    pmi.write(*dim_tol, model);
    json << "],\"stats\":{\"total_shapes\":" << assembly.total
         << ",\"colored_shapes\":" << assembly.colored << ",\"pmi_count\":" << pmi.count << "}}";
    return std::move(json).str();
}

} // namespace occt_xcaf
} // namespace

#endif

extern "C" {

int occt_bridge_import_step_xcaf_json(
    const char* step_path, char** out_json, char* err_buf, size_t err_buf_len) {
    const BridgeError last_error("import_step_xcaf_json");
    if (out_json) *out_json = nullptr;
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    const auto error = [&](int status, std::string_view message) {
        copy_utf8(err_buf, err_buf_len, message);
        return last_error.fail(status, message);
    };
    copy_utf8(err_buf, err_buf_len, "");
    if (!step_path || !*step_path || !out_json) return error(-1, "Missing STEP path or JSON output");

    try {
        std::ifstream input(utf8_path(step_path), std::ios::binary);
        if (!input) return error(-2, "Could not open STEP file");

        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const QuietExchangeTranslation quiet;
        STEPCAFControl_Reader reader;
        reader.SetColorMode(true);
        reader.SetNameMode(true);
        reader.SetLayerMode(true);
        reader.SetGDTMode(true);
        reader.SetSHUOMode(true);
        if (reader.ReadStream(step_path, input) != IFSelect_RetDone) {
            return error(-2, "Could not read STEP file");
        }

        auto& step = reader.ChangeReader();
        STEPConstruct_ExternRefs external(step.WS());
        external.LoadExternRefs();
        if (external.NbExternRefs() != 0) return error(-3, "External STEP references are not supported");

        // Initialize a transient document without registering it in the application session.
        Handle(TDocStd_Document) doc = new TDocStd_Document("MDTV-XCAF");
        XCAFApp_Application::GetApplication()->InitDocument(doc);
        step.SetSystemLengthUnit(1.0);
        XCAFDoc_DocumentTool::SetLengthUnit(doc, 0.001);
        if (!reader.Transfer(doc)) return error(-3, "Could not transfer STEP metadata");

        const auto json = occt_xcaf::document_json(*doc, *step.StepModel());
        auto result = std::make_unique<char[]>(json.size() + 1);
        std::memcpy(result.get(), json.c_str(), json.size() + 1);
        *out_json = result.release();
        return OCCT_OK;
    } catch (const Standard_Failure& failure) {
        const char* message = failure.GetMessageString();
        return error(-3, message && *message ? message : "OCCT STEP metadata import failed");
    } catch (const std::exception& failure) {
        return error(-3, failure.what());
    } catch (...) {
        return error(-3, "STEP metadata import failed");
    }
#else
    (void)step_path;
    if (err_buf && err_buf_len) std::snprintf(err_buf, err_buf_len, "OCCT backend not enabled");
    return last_error.fail(-4, "OCCT backend not enabled");
#endif
}

int occt_bridge_export_step_xcaf_json(
    const char* step_path, const char* spec_json, char* err_buf, size_t err_buf_len) {
    const BridgeError last_error("export_step_xcaf_json");
#if defined(AI_NATIVE_CAD_HAS_OCCT)
    const auto error = [&](int status, std::string_view message) {
        copy_utf8(err_buf, err_buf_len, message);
        return last_error.fail(status, message);
    };
    copy_utf8(err_buf, err_buf_len, "");
    if (!step_path || !*step_path || !spec_json) return error(-1, "Missing STEP path or JSON specification");
    if (!occt_xcaf::is_valid_utf8(step_path)) return error(-1, "STEP export path must be valid UTF-8");

    try {
        const auto spec = occt_xcaf::ExportJson(spec_json).parse();
        const auto path = utf8_path(step_path);
        std::lock_guard<std::mutex> lock(g_occt_mutex);
        const QuietExchangeTranslation quiet;
        occt_xcaf::export_document(path, spec);
        return OCCT_OK;
    } catch (const std::invalid_argument& failure) {
        return error(-1, failure.what());
    } catch (const Standard_Failure& failure) {
        const char* message = failure.GetMessageString();
        return error(-3, message && *message ? message : "OCCT STEP metadata export failed");
    } catch (const std::exception& failure) {
        return error(-3, failure.what());
    } catch (...) {
        return error(-3, "STEP metadata export failed");
    }
#else
    (void)step_path;
    (void)spec_json;
    if (err_buf && err_buf_len) std::snprintf(err_buf, err_buf_len, "OCCT backend not enabled");
    return last_error.fail(-4, "OCCT backend not enabled");
#endif
}

void occt_bridge_free_string(char* ptr) {
    delete[] ptr;
}

} // extern "C"
