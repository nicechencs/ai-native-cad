#pragma once

#include <stdint.h>

#ifdef _WIN32
#  ifdef OCCT_BRIDGE_EXPORTS
#    define OCCT_BRIDGE_API __declspec(dllexport)
#  else
#    define OCCT_BRIDGE_API __declspec(dllimport)
#  endif
#else
#  define OCCT_BRIDGE_API __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define OCCT_OK 0
#define OCCT_ERR_BUFFER_TOO_SMALL (-10)
#define AICAD_KERNEL_ABI_VERSION 1u

typedef struct OcctBridgeBBox {
    double min[3];
    double max[3];
} OcctBridgeBBox;

typedef struct OcctBridgeMetrics {
    int32_t watertight;
    double volume_mm3;
    OcctBridgeBBox bbox;
    int32_t face_count;
    int32_t edge_count;
} OcctBridgeMetrics;

typedef struct OcctImportOptions {
    int32_t heal_geometry;
    int32_t unify_same_domain;
    double sewing_tol_mm;
    double linear_deflection_mm;
    double angular_deflection_rad;
} OcctImportOptions;

typedef struct OcctImportMetadata {
    char unit_detected[16];
    int32_t is_watertight_solid;
    int32_t open_shell_count;
    int32_t component_count;
} OcctImportMetadata;

typedef struct OcctTessellationCounts {
    int32_t face_count;
    uint32_t total_vertices;
    uint32_t total_indices;
} OcctTessellationCounts;

typedef struct OcctFaceDescriptor {
    int32_t face_index;
    int32_t surface_kind;
    double normal_or_axis[3];
    double origin_or_center[3];
    double radius_mm;
    double area_mm2;
    uint32_t tri_vertex_offset;
    uint32_t tri_vertex_count;
    uint32_t tri_index_offset;
    uint32_t tri_index_count;
} OcctFaceDescriptor;

typedef struct OcctComponentDescriptor {
    char component_id[128];
    char part_name[128];
    double transform_col_major[16];
    uint64_t shape_handle;
} OcctComponentDescriptor;

typedef struct OcctBridgeLastError {
    int32_t status_code;
    char operation[64];
    char message[512];
} OcctBridgeLastError;

// Fallible operations clear the calling thread's error on entry and set it on
// failure. Cleanup and capability queries leave it alone. Strings are NUL-terminated;
// operation is the function name without "occt_bridge_". Status calls keep their
// error codes; handle calls use -1 for invalid inputs or handles, -3 for operation
// failures, and -4 when unavailable.
// Returns 1 and consumes the error, or 0 and zeroes *out if there is no error.
// Passing NULL returns 0 without consuming the error.
OCCT_BRIDGE_API int32_t occt_bridge_take_last_error(OcctBridgeLastError* out);
OCCT_BRIDGE_API void occt_bridge_clear_last_error(void);

OCCT_BRIDGE_API const char* occt_bridge_version();
OCCT_BRIDGE_API uint32_t aicad_kernel_abi_version(void);

OCCT_BRIDGE_API uint64_t occt_bridge_make_box(
    double dx,
    double dy,
    double dz,
    double ox,
    double oy,
    double oz);

OCCT_BRIDGE_API uint64_t occt_bridge_make_cylinder(
    double diameter,
    double height,
    double ox,
    double oy,
    double oz);

// Exact right circular cylinder whose base circle is centred at `origin` and
// whose axis points along `axis` (any finite nonzero direction; normalized
// internally). The solid spans origin .. origin + height * axis/|axis|.
// Returns a caller-owned handle for a validated closed solid, or 0 (-1 for
// invalid arguments, -3 for an OCCT failure, -4 when OCCT is unavailable).
OCCT_BRIDGE_API uint64_t occt_bridge_make_cylinder_axis(
    double diameter,
    double height,
    const double origin[3],
    const double axis[3]);

OCCT_BRIDGE_API uint64_t occt_bridge_boolean_cut(
    uint64_t base_handle,
    uint64_t tool_handle);

OCCT_BRIDGE_API uint64_t occt_bridge_boolean_union(
    uint64_t base_handle,
    uint64_t tool_handle);

OCCT_BRIDGE_API uint64_t occt_bridge_boolean_intersect(
    uint64_t base_handle,
    uint64_t tool_handle);

// Inspect candidate zero-based tessellation face indices on a single closed solid.
// Writes the exact hole radius in mm or zero for each unsupported face.
OCCT_BRIDGE_API int32_t occt_bridge_read_through_holes(
    uint64_t handle, int32_t count, const int32_t* face_indices, double* out_radii);

// Resize one full cylindrical through-hole in a closed solid. Face indices are
// zero-based, as in tessellation. On success the caller owns out_handle and
// out_source_indices maps each new face index to one original face index.
// Returns -1 for bad input, -2 for unsupported geometry, -3 if the edit would
// change other topology or fail validation, -4 if native OCCT is unavailable.
OCCT_BRIDGE_API int32_t occt_bridge_offset_through_hole(
    uint64_t handle, int32_t face_index, double delta_mm,
    int32_t capacity, int32_t* out_source_indices, uint64_t* out_handle);

// Move or fill the same through-hole with exact B-Rep geometry. Move reads
// three offset components in mm and requires a perpendicular offset (-5
// otherwise). The mapping capacity equals the original face count for move
// and that count minus one for delete. Other statuses match offset.
OCCT_BRIDGE_API int32_t occt_bridge_move_through_hole(
    uint64_t handle, int32_t face_index, const double* offset_mm,
    int32_t capacity, int32_t* out_source_indices, uint64_t* out_handle);
OCCT_BRIDGE_API int32_t occt_bridge_delete_through_hole(
    uint64_t handle, int32_t face_index, int32_t capacity,
    int32_t* out_source_indices, uint64_t* out_handle);

// Return 1 only when the exact interference implementation is available.
OCCT_BRIDGE_API int32_t occt_bridge_has_exact_interference(void);

// Exact closed B-Rep solids only. Return 0 if clear, 1 for common volume
// > 1e-4 mm^3, or 2 for distance < min_clearance_mm - 1e-4 mm (when clearance
// is positive). Write volume (1) or distance (2) to out_measurement on violation.
// Negative status: -1 invalid argument/handle, -2 invalid solid topology,
// -3 OCCT operation failed, -4 exact backend unavailable. No output otherwise.
OCCT_BRIDGE_API int32_t occt_bridge_check_interference(
    uint64_t a_handle,
    uint64_t b_handle,
    double min_clearance_mm,
    double* out_measurement);

// Compare two closed B-Rep solids. An empty cut has zero volume; no mesh fallback.
// Return 0 and fill the three outputs, or -1 for invalid arguments/handles,
// -2 for invalid solid topology, -3 for an OCCT failure, -4 without OCCT,
// or -5 if an operand contains more than one solid.
typedef struct OcctBridgeSymmetricDifference {
    double candidate_only_mm3;
    double reference_only_mm3;
    double reference_volume_mm3;
} OcctBridgeSymmetricDifference;

OCCT_BRIDGE_API int32_t occt_bridge_measure_symmetric_difference(
    uint64_t candidate_handle,
    uint64_t reference_handle,
    OcctBridgeSymmetricDifference* out);

// Dress-ups return a new caller-owned handle for a validated closed solid, or
// 0 on any failure; they never return the input handle unchanged. The *_all
// variants first merge faces split by Booleans, then dress every sharp
// manifold edge (seams, degenerate and tangent edges are skipped).
OCCT_BRIDGE_API uint64_t occt_bridge_fillet_all(
    uint64_t handle,
    double radius);

OCCT_BRIDGE_API uint64_t occt_bridge_chamfer_all(
    uint64_t handle,
    double distance);

// Dress exactly the listed edges. Indices are zero-based TopExp edge-map
// indices of this handle, matching occt_bridge_extract_topology. Each index
// must be distinct and name a sharp edge shared by two faces.
OCCT_BRIDGE_API uint64_t occt_bridge_fillet_edges(
    uint64_t handle,
    double radius,
    const int32_t* edge_indices,
    int32_t edge_count);

OCCT_BRIDGE_API uint64_t occt_bridge_chamfer_edges(
    uint64_t handle,
    double distance,
    const int32_t* edge_indices,
    int32_t edge_count);

// Hollow a closed solid inward, opening the face whose outward normal is
// closest to the given direction (cosine at least 0.9). Returns 0 on failure.
OCCT_BRIDGE_API uint64_t occt_bridge_shell(
    uint64_t handle,
    double thickness,
    double open_nx,
    double open_ny,
    double open_nz);

OCCT_BRIDGE_API uint64_t occt_bridge_extrude_polygon(
    int plane_code,
    const double* xy_pairs,
    int point_count,
    double height,
    int symmetric);

// Metrics of a stored shape. `watertight` is 1 only when every solid shell is
// closed and manifold, BRepCheck_Analyzer reports the shape valid and the
// enclosed volume is positive.
OCCT_BRIDGE_API int32_t occt_bridge_check(
    uint64_t handle,
    OcctBridgeMetrics* out_metrics);

// The centroid is defined for a closed solid with positive volume.
OCCT_BRIDGE_API int32_t occt_bridge_volume_centroid(uint64_t handle, double out_mm[3]);

OCCT_BRIDGE_API int32_t occt_bridge_export_step(
    uint64_t handle,
    const char* path_utf8);

// Same STEP export with a UTF-8 product name for the root shape.
OCCT_BRIDGE_API int32_t occt_bridge_export_step_named(
    uint64_t handle, const char* path_utf8, const char* name_utf8);

// Export `count` >= 1 stored shapes as one AP242 STEP assembly. Each handle
// becomes a component product named names_utf8[i] (non-empty UTF-8) placed
// under a root assembly product named assembly_name_utf8. Shapes keep their
// stored world placement; exact geometry is written by STEPCAFControl_Writer.
// Status: 0 on success, -1 invalid arguments, -2 invalid handle, -3 transfer
// failure, -4 write failure or OCCT unavailable.
OCCT_BRIDGE_API int32_t occt_bridge_export_step_assembly(
    int32_t count, const uint64_t* handles, const char* const* names_utf8,
    const char* assembly_name_utf8, const char* path_utf8);

OCCT_BRIDGE_API void occt_bridge_free(uint64_t handle);

// Formats: 0 = extension-based detection, 1 = STEP, 2 = IGES, 3 = BREP.
OCCT_BRIDGE_API uint64_t occt_bridge_import_file(
    const char* path_utf8, int32_t format, const OcctImportOptions* opts,
    OcctImportMetadata* out_meta, char* err_buf, int32_t err_buf_len);

// Null output or zero capacity queries the count without allocating any handles.
// Each returned shape_handle is local to the part and must be freed independently.
OCCT_BRIDGE_API int32_t occt_bridge_get_components(
    uint64_t root_handle, OcctComponentDescriptor* out_items, int32_t max_items);

// For each occurrence, return the first component index with exactly the same
// local OCCT shape, including location and orientation. Order and count match
// get_components. No handles are allocated; this is not geometric matching.
OCCT_BRIDGE_API int32_t occt_bridge_get_component_prototypes(
    uint64_t root_handle, int32_t* out_indices, int32_t capacity);

// Fill uses the latest successful mesh query on the calling thread, for that
// same handle. Concurrent threads get independent snapshots.
OCCT_BRIDGE_API int32_t occt_bridge_mesh_and_query_counts(
    uint64_t handle, const OcctImportOptions* opts, OcctTessellationCounts* out_counts);

// Position/normal lengths are 3 * max_vertices. Indices are local to each face.
// Insufficient capacity returns OCCT_ERR_BUFFER_TOO_SMALL without writing output.
OCCT_BRIDGE_API int32_t occt_bridge_extract_tessellation(
    uint64_t handle, int32_t max_faces, uint32_t max_vertices, uint32_t max_indices,
    OcctFaceDescriptor* out_faces, double* out_positions_mm, double* out_normals,
    uint32_t* out_indices);

typedef struct OcctTopologyUse {
    uint32_t face_index;
    int32_t is_seam;
} OcctTopologyUse;

typedef struct OcctTopologyEdge {
    int32_t curve_kind; // 0=line, 1=circle, 2=ellipse, 3=B-spline, 4=other
    int32_t is_degenerate;
    uint32_t use_offset;
    uint32_t use_count;
} OcctTopologyEdge;

typedef struct OcctTopologyResult {
    const int32_t* face_surface_kinds; // Same codes as OcctFaceDescriptor.
    const OcctTopologyEdge* edges;
    const OcctTopologyUse* uses;
    uint32_t face_count, edge_count, use_count;
    void* storage;
} OcctTopologyResult;

// Exact B-Rep topology, no tessellation. Face and edge IDs are zero-based
// TopExp map indices for this shape. Each edge lists distinct, sorted faces.
// Use fresh/freed output storage. Free the result once, independently of the
// shape handle. Status: -1 invalid argument/handle, -3 extraction failed,
// -4 exact backend unavailable.
OCCT_BRIDGE_API int32_t occt_bridge_extract_topology(uint64_t handle, OcctTopologyResult* out);
OCCT_BRIDGE_API void occt_bridge_free_topology(OcctTopologyResult* result);

OCCT_BRIDGE_API uint64_t occt_bridge_transform(uint64_t handle, const double mat_col_major[16]);

OCCT_BRIDGE_API uint64_t occt_bridge_revolve_polygon(
    int plane_code, const double* xy_pairs, int point_count, const double axis_origin[3],
    const double axis_dir[3], double angle_deg);

// Sweep a closed simple polygon along an open polyline spine with mitred
// (right-corner) transitions. profile_xy holds profile_count >= 3 XY pairs in
// the profile plane at path[0]: with t = normalize(path[1] - path[0]),
// r = |t.z| < 0.9 ? +Z : +X, u = normalize(r x t) and v = t x u, profile point
// (x, y) maps to path[0] + x * u + y * v. path_xyz holds path_count >= 2 XYZ
// points; consecutive points must be distinct and the path must be open.
// Returns a caller-owned handle for a validated closed solid, or 0.
OCCT_BRIDGE_API uint64_t occt_bridge_sweep_polygon(
    const double* profile_xy, int32_t profile_count,
    const double* path_xyz, int32_t path_count);

// Loft through section_count >= 2 closed sketch polygons. Section i has
// section_point_counts[i] >= 3 consecutive XY pairs in xy_pairs, lying on
// sketch plane plane_codes[i] (0 = XY, 1 = XZ, 2 = YZ) at offsets[i] along
// that plane's normal. ruled != 0 joins consecutive sections with ruled
// surfaces; otherwise OCCT fits smooth surfaces through the sections.
// Returns a caller-owned handle for a validated closed solid, or 0.
OCCT_BRIDGE_API uint64_t occt_bridge_loft_polygons(
    int32_t section_count, const int32_t* section_point_counts,
    const int32_t* plane_codes, const double* offsets,
    const double* xy_pairs, int32_t ruled);

typedef struct OcctHlrPoint {
    double x, y;
} OcctHlrPoint;

typedef struct OcctHlrPolyline {
    uint32_t point_offset, point_count;
    int32_t hidden;
} OcctHlrPolyline;

typedef struct OcctHlrResult {
    const OcctHlrPoint* points;
    const OcctHlrPolyline* polylines;
    uint32_t point_count, polyline_count;
    void* storage;
} OcctHlrResult;

// Orthographic X/Y coordinates in mm, relative to the world origin. `toward_eye`
// and `view_x` define +Z and +X of the view; the bridge derives +Y. Visibility
// is solved on the B-Rep, then curves are sampled at deflection_mm.
// Pass fresh or freed output storage. On success free it once with free_hlr.
// Status: -1 invalid argument/handle, -3 HLR failed, -4 backend unavailable.
OCCT_BRIDGE_API int32_t occt_bridge_compute_hlr(
    uint64_t handle, const double toward_eye[3], const double view_x[3],
    double deflection_mm, OcctHlrResult* out);
OCCT_BRIDGE_API void occt_bridge_free_hlr(OcctHlrResult* result);

// The freeform functions return a caller-owned handle, or zero on failure or
// when the OCCT backend is unavailable.
// Supply at least two sections of at least three vertices each. xyz_points
// contains consecutive XYZ triplets for each section, closed automatically.
OCCT_BRIDGE_API uint64_t occt_bridge_make_bspline_loft(
    int32_t section_count, const int32_t* section_point_counts,
    const double* xyz_points, int32_t make_solid, int32_t ruled);

// Draft planar side faces whose normals are perpendicular to pull_dir.
OCCT_BRIDGE_API uint64_t occt_bridge_draft_faces(
    uint64_t handle, double draft_angle_deg, const double pull_dir[3],
    const double neutral_origin[3], const double neutral_normal[3]);

// Interpolate the radius linearly along each eligible fillet contour. Closed
// contours require equal start and end radii.
OCCT_BRIDGE_API uint64_t occt_bridge_variable_fillet(
    uint64_t handle, double r_start_mm, double r_end_mm);

// Both grid dimensions must be at least two. xyz_grid contains XYZ triplets
// in U-major order (u * nv + v).
// Zero thickness returns a face. Positive thickness sweeps the face along
// the oriented surface normal at the center of the fitted parameter domain.
OCCT_BRIDGE_API uint64_t occt_bridge_quad_grid_to_bspline_surface(
    int32_t nu, int32_t nv, const double* xyz_grid, double thickness_mm);

#ifdef __cplusplus
} // extern "C"
#endif
