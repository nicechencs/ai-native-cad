#pragma once

#include "occt_bridge.h"

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Import STEP metadata from a UTF-8 path. On success, out_json owns a
// NUL-terminated UTF-8 JSON string. Release it with occt_bridge_free_string.
// Nodes use zero-based occurrence IDs and null parent IDs for roots. Matrices
// are flat, column-major, and relative to the parent, with translation in mm.
// color_rgba is linear RGBA in [0,1], or null without an occurrence or
// definition color.
// Counts describe the entries emitted in the assembly and PMI arrays.
// On failure, out_json is cleared. An optional error buffer is NUL-terminated
// when it has capacity. Status: -1 bad arguments, -2 unreadable STEP,
// -3 unsupported references or import failure, -4 backend unavailable.
OCCT_BRIDGE_API int occt_bridge_import_step_xcaf_json(
    const char* step_path, char** out_json, char* err_buf, size_t err_buf_len);

// Export AP242 to a UTF-8 path. The JSON object has assembly_name, a nonempty
// parts array (name, size_mm, translation_mm, color_rgba), and a pmi_items
// array (name, kind, value_text). Colors are linear RGBA in [0,1].
// PMI applies to the first box. Dimension (edge length) and geometric
// tolerance (flatness) values are decimal millimetres encoded as strings.
// Datum values are unique identifiers.
// Status: -1 invalid path/specification, -3 export failure, -4 unavailable.
OCCT_BRIDGE_API int occt_bridge_export_step_xcaf_json(
    const char* step_path, const char* spec_json, char* err_buf, size_t err_buf_len);

OCCT_BRIDGE_API void occt_bridge_free_string(char* ptr);

#ifdef __cplusplus
} // extern "C"
#endif
