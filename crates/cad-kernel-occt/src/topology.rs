//! Exact face and edge incidence from a native OCCT handle.

use crate::{KernelError, OcctShapeGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    BSpline,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveKind {
    Line,
    Circle,
    Ellipse,
    BSpline,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceUse {
    pub face_index: u32,
    pub is_seam: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub curve_kind: CurveKind,
    pub is_degenerate: bool,
    pub face_uses: Vec<FaceUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTopology {
    pub faces: Vec<SurfaceKind>,
    pub edges: Vec<Edge>,
}

#[repr(C)]
struct RawUse {
    face_index: u32,
    is_seam: i32,
}

#[repr(C)]
struct RawEdge {
    curve_kind: i32,
    is_degenerate: i32,
    use_offset: u32,
    use_count: u32,
}

#[derive(Default)]
#[repr(C)]
struct RawResult {
    face_surface_kinds: *const i32,
    edges: *const RawEdge,
    uses: *const RawUse,
    face_count: u32,
    edge_count: u32,
    use_count: u32,
    storage: *mut std::ffi::c_void,
}

unsafe extern "C" {
    fn occt_bridge_extract_topology(handle: u64, out: *mut RawResult) -> i32;
    fn occt_bridge_free_topology(result: *mut RawResult);
}

impl Drop for RawResult {
    fn drop(&mut self) {
        unsafe { occt_bridge_free_topology(self) };
    }
}

fn invalid() -> KernelError {
    KernelError::OperationFailed("OCCT: invalid exact topology output".into())
}

// Safety: for a nonempty slice the native buffer must have `count` initialized
// elements and remain live for the duration of the borrow of `owner`.
unsafe fn native_slice<T>(
    _owner: &RawResult,
    data: *const T,
    count: u32,
) -> Result<&[T], KernelError> {
    let count = count as usize;
    if count == 0 {
        return Ok(&[]);
    }
    if data.is_null() || !data.is_aligned() || count > isize::MAX as usize / size_of::<T>() {
        return Err(invalid());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, count) })
}

fn flag(value: i32) -> Result<bool, KernelError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}

/// Reads the shape's original B-Rep, without consulting its preview mesh.
/// Vector indices are shape-local IDs; face indices correspond to the native
/// mesh descriptors (`occt_face_<index>`) for the same shape.
pub fn extract(handle: &OcctShapeGuard) -> Result<ExactTopology, KernelError> {
    extract_from_handle(handle.raw_handle())
}

pub fn extract_from_handle(raw_handle: u64) -> Result<ExactTopology, KernelError> {
    let mut raw = RawResult::default();
    match unsafe { occt_bridge_extract_topology(raw_handle, &mut raw) } {
        0 => (),
        -4 => return Err(KernelError::BackendUnavailable),
        status => {
            return Err(KernelError::OperationFailed(format!(
                "OCCT: exact topology extraction failed ({status})"
            )));
        }
    }
    if raw.storage.is_null() {
        return Err(invalid());
    }
    // The buffers are owned by `raw` until Drop. No borrowed data is returned.
    let faces = unsafe { native_slice(&raw, raw.face_surface_kinds, raw.face_count) }?;
    let edges = unsafe { native_slice(&raw, raw.edges, raw.edge_count) }?;
    let uses = unsafe { native_slice(&raw, raw.uses, raw.use_count) }?;
    let faces = faces
        .iter()
        .map(|kind| {
            Ok(match kind {
                0 => SurfaceKind::Plane,
                1 => SurfaceKind::Cylinder,
                2 => SurfaceKind::Cone,
                3 => SurfaceKind::Sphere,
                4 => SurfaceKind::Torus,
                5 => SurfaceKind::BSpline,
                6 => SurfaceKind::Other,
                _ => return Err(invalid()),
            })
        })
        .collect::<Result<_, _>>()?;
    let mut next = 0usize;
    let mut owned_edges = Vec::with_capacity(edges.len());
    for edge in edges {
        let end = next
            .checked_add(edge.use_count as usize)
            .ok_or_else(invalid)?;
        if edge.use_offset as usize != next {
            return Err(invalid());
        }
        let face_uses = uses.get(next..end).ok_or_else(invalid)?;
        if face_uses
            .windows(2)
            .any(|w| w[0].face_index >= w[1].face_index)
        {
            return Err(invalid());
        }
        let face_uses = face_uses
            .iter()
            .map(|face_use| {
                if face_use.face_index >= raw.face_count {
                    return Err(invalid());
                }
                Ok(FaceUse {
                    face_index: face_use.face_index,
                    is_seam: flag(face_use.is_seam)?,
                })
            })
            .collect::<Result<_, _>>()?;
        owned_edges.push(Edge {
            curve_kind: match edge.curve_kind {
                0 => CurveKind::Line,
                1 => CurveKind::Circle,
                2 => CurveKind::Ellipse,
                3 => CurveKind::BSpline,
                4 => CurveKind::Other,
                _ => return Err(invalid()),
            },
            is_degenerate: flag(edge.is_degenerate)?,
            face_uses,
        });
        next = end;
    }
    if next != uses.len() {
        return Err(invalid());
    }
    Ok(ExactTopology {
        faces,
        edges: owned_edges,
    })
}
