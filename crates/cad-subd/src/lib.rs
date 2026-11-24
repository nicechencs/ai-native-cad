//! SubD control cage binary codec (`blobs/subd/`) and weighted Catmull-Clark evaluator.
//!
//! A control cage is a polygon mesh plus per-edge crease sharpness. The cage is
//! stored as a little-endian `SUBD` blob so large coordinate lists stay out of
//! text source files. Subdivision turns that cage into a finer all-quad cage.

#![forbid(unsafe_code)]

mod cage;
mod catmull_clark;
mod codec;
mod continuity;
mod nurbs_convert;

pub use cage::{CREASE_INFINITE, CREASE_SMOOTH, CageReport, SubDCage, SubDError};
pub use catmull_clark::{SubDMesh, subdivide, subdivide_once, to_triangle_mesh};
pub use codec::{SUBD_MAGIC, SUBD_VERSION, decode_subd_blob, encode_subd_blob, subd_blob_sha256};
pub use continuity::{ContinuityReport, ContinuityTolerances, verify_patch_set_continuity};
pub use nurbs_convert::{
    BicubicPatch, GregoryInterior, NurbsConversionOptions, NurbsPatchSet, PatchEdge, PatchEdgeRef,
    PatchKind, PatchSeam, QuadSurfaceGrid, SurfaceEval, bspline_to_bezier, to_nurbs_patch_set,
    to_nurbs_patch_set_with_options,
};
