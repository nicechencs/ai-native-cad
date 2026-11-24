//! Shared, content-addressed geometry evaluation for the DSL and asset inspection.

use crate::asset_resolver::{AssetResolveError, AssetResolver, asset_sha256};
use cad_hir::{Diagnostic, Severity};
use cad_kernel_occt::{
    BBox3, BRepEdge, BRepFace, CheckReport, ComponentMeta, CsgNode, CurveType, GeometryKernel,
    KernelError, KernelPayload, OcctImportFormat, OcctImportOptions, SolidShape, SurfaceType,
    default_brep_kernel,
};
use cad_mesh_io::{
    AnalyticSurface, LengthUnit, Mesh, MeshError, cluster_analytic_faces,
    load_3mf_document_tolerant, load_gltf_with_source_unit, load_obj, load_stl,
};
use cad_syntax::Span;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportFormat {
    Step,
    Brep,
    Iges,
    Stl,
    ThreeMf,
    Obj,
    Gltf,
}

impl ImportFormat {
    pub fn from_path(path: &str) -> Option<Self> {
        match Path::new(path)
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
            .as_str()
        {
            "step" | "stp" => Some(Self::Step),
            "brep" => Some(Self::Brep),
            "iges" | "igs" => Some(Self::Iges),
            "stl" => Some(Self::Stl),
            "3mf" => Some(Self::ThreeMf),
            "obj" => Some(Self::Obj),
            "gltf" | "glb" => Some(Self::Gltf),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Step => "STEP",
            Self::Brep => "BREP",
            Self::Iges => "IGES",
            Self::Stl => "STL",
            Self::ThreeMf => "3MF",
            Self::Obj => "OBJ",
            Self::Gltf => "GLTF",
        }
    }

    pub(crate) fn is_mesh(self) -> bool {
        matches!(self, Self::Stl | Self::ThreeMf | Self::Obj | Self::Gltf)
    }
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Uses the asset extension if unspecified.
    pub format: Option<ImportFormat>,
    pub sha256: Option<String>,
    pub source_unit: Option<LengthUnit>,
    pub deflection_mm: f64,
    pub component: Option<String>,
    pub cluster_analytic_faces: bool,
    pub allow_open_shell: bool,
    /// Opt in to repair, sewing and same-domain unification for an exact import.
    pub heal: bool,
    /// In millimeters, used when `heal` is true; defaults to 0.001 mm.
    pub sewing_tol_mm: Option<f64>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            format: None,
            sha256: None,
            source_unit: None,
            deflection_mm: 0.05,
            component: None,
            cluster_analytic_faces: true,
            allow_open_shell: false,
            heal: false,
            sewing_tol_mm: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvaluatedImport {
    pub solid: SolidShape,
    pub format: ImportFormat,
    pub sha256: String,
    pub unit_detected: String,
    pub check_report: CheckReport,
    pub component_count: usize,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    hash: String,
    format: ImportFormat,
    allow_open_shell: bool,
    options: CanonicalOptions,
}

#[derive(PartialEq, Eq)]
enum CanonicalOptions {
    Mesh {
        unit: Option<LengthUnit>,
        cluster: bool,
    },
    Exact {
        cache_namespace: Option<&'static str>,
        deflection_bits: u64,
        component: Option<String>,
        healing_tol_bits: Option<u64>,
    },
}

struct ImportedGeometry {
    solid: SolidShape,
    unit: String,
    component_count: usize,
    open_issue: Option<String>,
    exact_open_shell_count: Option<usize>,
}

// LRU by entry count and approximate Rust-side footprint. Native work and deep
// geometry clones stay outside the lock; failures and source locations aren't cached.
const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
struct ImportCache {
    entries: VecDeque<(CacheKey, Arc<ImportedGeometry>, usize)>,
    bytes: usize,
}
static CACHE: Mutex<ImportCache> = Mutex::new(ImportCache {
    entries: VecDeque::new(),
    bytes: 0,
});

impl ImportCache {
    fn get(&mut self, key: &CacheKey) -> Option<Arc<ImportedGeometry>> {
        let index = self.entries.iter().position(|(k, _, _)| k == key)?;
        let entry = self.entries.remove(index)?;
        let geometry = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(geometry)
    }

    fn insert(&mut self, key: CacheKey, geometry: &Arc<ImportedGeometry>, bytes: usize) {
        if bytes > CACHE_BYTES || self.get(&key).is_some() {
            return;
        }
        while self.entries.len() >= CACHE_ENTRIES || self.bytes + bytes > CACHE_BYTES {
            if let Some((_, _, old_bytes)) = self.entries.pop_front() {
                self.bytes -= old_bytes;
            }
        }
        self.bytes += bytes;
        self.entries.push_back((key, Arc::clone(geometry), bytes));
    }
}

fn cache_weight(solid: &SolidShape) -> usize {
    let faces = solid
        .faces
        .iter()
        .map(|f| {
            size_of::<BRepFace>()
                + 8 * (f.vertices.capacity() + f.normals.capacity())
                + 4 * f.indices.capacity()
                + 24 * (f.outer_loop.capacity()
                    + f.top_loop.as_ref().map_or(0, Vec::capacity)
                    + f.inner_loops.iter().map(Vec::capacity).sum::<usize>())
        })
        .sum::<usize>();
    let edges = solid
        .edges
        .iter()
        .map(|e| size_of::<BRepEdge>() + 8 * e.polyline.capacity())
        .sum::<usize>();
    // Geometry envelopes also carry triangles independently of the render faces.
    1024 + 3 * faces + edges
}

pub(crate) fn import_diagnostic(code: &str, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: Severity::Error,
        message: message.into(),
        span,
    }
}

pub(crate) fn operation_diagnostic(err: KernelError, span: Span) -> Diagnostic {
    let (code, message) = match err {
        KernelError::BackendUnavailable => ("E504_UNSUPPORTED_IMPORT_FORMAT", "operation requires an OCCT-enabled build".into()),
        KernelError::OpenShell => ("E505_IMPORT_NON_MANIFOLD_OR_OPEN", "open or non-manifold geometry is preview-only and cannot participate in a solid operation".into()),
        KernelError::UnsupportedMixedBoolean => ("E507_UNSUPPORTED_MIXED_BOOLEAN", "unsupported boolean input; use STEP/BREP for exact operations or keep mesh assets as assembly references".into()),
        KernelError::OperationFailed(message) => ("E207_INVALID_FEATURE", message),
    };
    import_diagnostic(code, message, span)
}

/// Resolve and verify asset bytes, then import in millimeters using the same path
/// as DSL evaluation. The returned solid is independently owned by this call.
pub fn evaluate_import(
    resolver: &dyn AssetResolver,
    path: &str,
    options: &ImportOptions,
    span: Span,
) -> Result<EvaluatedImport, Diagnostic> {
    let kernel = default_brep_kernel();
    evaluate_import_with_kernel(resolver, path, options, span, kernel.as_ref())
}

/// Resolve and import an asset using the caller's kernel for exact geometry and checking.
pub fn evaluate_import_with_kernel(
    resolver: &dyn AssetResolver,
    path: &str,
    options: &ImportOptions,
    span: Span,
    kernel: &dyn GeometryKernel,
) -> Result<EvaluatedImport, Diagnostic> {
    evaluate_import_for_family(resolver, path, options, None, span, kernel)
}

/// DSL operator family (`true` = mesh, `false` = exact) restricts formats without
/// letting filename validation take precedence over the resolver's sandbox.
pub(crate) fn evaluate_import_for_family(
    resolver: &dyn AssetResolver,
    path: &str,
    options: &ImportOptions,
    mesh_family: Option<bool>,
    span: Span,
    kernel: &dyn GeometryKernel,
) -> Result<EvaluatedImport, Diagnostic> {
    let asset_error = |e: AssetResolveError| import_diagnostic(e.code(), e.to_string(), span);
    let bytes = resolver.resolve(path).map_err(asset_error)?;
    let sha256 = asset_sha256(&bytes);
    if let Some(expected) = &options.sha256
        && !expected.eq_ignore_ascii_case(&sha256)
    {
        return Err(asset_error(AssetResolveError::HashMismatch {
            expected: expected.clone(),
            actual: sha256,
        }));
    }
    let format = options
        .format
        .or_else(|| ImportFormat::from_path(path))
        .filter(|f| mesh_family.is_none_or(|mesh| mesh == f.is_mesh()))
        .ok_or_else(|| {
            import_diagnostic(
                "E504_UNSUPPORTED_IMPORT_FORMAT",
                "expected a supported import extension (STEP, IGES, BREP, STL, 3MF, OBJ, glTF or GLB) for this operator",
                span,
            )
        })?;

    let canonical = if format.is_mesh() {
        if options.heal || options.sewing_tol_mm.is_some() {
            return Err(import_diagnostic(
                "E504_UNSUPPORTED_IMPORT_FORMAT",
                "heal and sewing_tol are only supported for exact imports",
                span,
            ));
        }
        let unit = match format {
            ImportFormat::Stl | ImportFormat::Obj => Some(options.source_unit.unwrap_or_default()),
            ImportFormat::Gltf => Some(options.source_unit.unwrap_or(LengthUnit::Meter)),
            _ => options.source_unit,
        };
        CanonicalOptions::Mesh {
            unit,
            cluster: options.cluster_analytic_faces,
        }
    } else {
        if options.source_unit.is_some() {
            return Err(import_diagnostic(
                "E504_UNSUPPORTED_IMPORT_FORMAT",
                "source_unit is only supported for mesh imports",
                span,
            ));
        }
        if !options.deflection_mm.is_finite() || options.deflection_mm <= 0.0 {
            return Err(import_diagnostic(
                "E207_INVALID_FEATURE",
                "import deflection must be a finite positive length",
                span,
            ));
        }
        if options.sewing_tol_mm.is_some_and(|tol| !tol.is_finite() || tol <= 0.0) {
            return Err(import_diagnostic(
                "E207_INVALID_FEATURE",
                "import sewing_tol must be a finite positive length",
                span,
            ));
        }
        CanonicalOptions::Exact {
            cache_namespace: kernel.exact_import_cache_namespace(),
            deflection_bits: options.deflection_mm.to_bits(),
            component: options.component.clone(),
            healing_tol_bits: options.heal.then(|| {
                options
                    .sewing_tol_mm
                    .unwrap_or(OcctImportOptions::default().sewing_tol_mm)
                    .to_bits()
            }),
        }
    };
    let key = CacheKey {
        hash: sha256.clone(),
        format,
        allow_open_shell: options.allow_open_shell,
        options: canonical,
    };
    // Mesh decoding is kernel-independent; exact results are only shareable
    // when the kernel explicitly opts in. Reports are always recomputed below.
    let cacheable = !matches!(
        &key.options,
        CanonicalOptions::Exact {
            cache_namespace: None,
            ..
        }
    );
    let cached = if cacheable {
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
    } else {
        None
    };
    let cache_missed = cached.is_none();
    let geometry = if let Some(geometry) = cached {
        geometry
    } else {
        let parse_result = match &key.options {
            CanonicalOptions::Mesh { unit, cluster } => make_mesh_geometry(
                &bytes,
                format,
                *unit,
                *cluster,
                options.allow_open_shell,
                span,
            ),
            CanonicalOptions::Exact {
                deflection_bits,
                component,
                healing_tol_bits,
                ..
            } => {
                let mut native = OcctImportOptions {
                    linear_deflection_mm: f64::from_bits(*deflection_bits),
                    ..Default::default()
                };
                if let Some(bits) = healing_tol_bits {
                    native.heal_geometry = 1;
                    native.unify_same_domain = 1;
                    native.sewing_tol_mm = f64::from_bits(*bits);
                }
                make_exact_geometry(
                    kernel,
                    &bytes,
                    format,
                    &native,
                    component.as_deref(),
                    options.allow_open_shell,
                    span,
                )
            },
        };
        Arc::new(parse_result?)
    };
    let mut solid = geometry.solid.clone();
    solid.part_name = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported_asset")
        .to_owned();
    for face in &mut solid.faces {
        face.ast_span = span;
    }
    for edge in &mut solid.edges {
        edge.ast_span = span;
    }
    let mut report = kernel.check_solid(&solid);
    let exact_check_issue = geometry
        .exact_open_shell_count
        .filter(|_| !report.watertight)
        .map(exact_open_issue);
    let mut diagnostics = Vec::new();
    if let Some(issue) = geometry.open_issue.as_ref().or(exact_check_issue.as_ref()) {
        let mut diagnostic =
            import_diagnostic("E505_IMPORT_NON_MANIFOLD_OR_OPEN", issue.clone(), span);
        if !options.allow_open_shell {
            return Err(diagnostic);
        }
        if format.is_mesh() {
            report.watertight = false;
            report.issues.push(issue.clone());
        }
        diagnostic.severity = Severity::Warning;
        diagnostics.push(diagnostic);
    }
    if cacheable && cache_missed {
        let bytes = cache_weight(&geometry.solid);
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, &geometry, bytes);
    }
    Ok(EvaluatedImport {
        solid,
        format,
        sha256,
        unit_detected: geometry.unit.clone(),
        check_report: report,
        component_count: geometry.component_count,
        diagnostics,
    })
}

fn make_exact_geometry(
    kernel: &dyn GeometryKernel,
    bytes: &[u8],
    format: ImportFormat,
    options: &OcctImportOptions,
    component: Option<&str>,
    allow_open_shell: bool,
    span: Span,
) -> Result<ImportedGeometry, Diagnostic> {
    let format = match format {
        ImportFormat::Step => OcctImportFormat::Step,
        ImportFormat::Brep => OcctImportFormat::Brep,
        ImportFormat::Iges => OcctImportFormat::Iges,
        _ => unreachable!("mesh formats are decoded by the mesh importer"),
    };
    let (solid, metadata) = kernel.import_exact(
        bytes,
        format,
        options,
        component,
        allow_open_shell,
        Span::default(),
    )
    .map_err(|err| match err {
        KernelError::OperationFailed(message) => {
            import_diagnostic("E504_UNSUPPORTED_IMPORT_FORMAT", message, span)
        }
        other => operation_diagnostic(other, span),
    })?;
    Ok(ImportedGeometry {
        solid,
        unit: metadata.unit_detected,
        component_count: metadata.component_count,
        open_issue: (!metadata.is_watertight_solid)
            .then(|| exact_open_issue(metadata.open_shell_count)),
        exact_open_shell_count: Some(metadata.open_shell_count),
    })
}

fn exact_open_issue(count: usize) -> String {
    format!("imported geometry is not a watertight solid ({count} open shell(s))")
}

fn mesh_diagnostic(err: MeshError, span: Span) -> Diagnostic {
    let is_topology = matches!(
        &err,
        MeshError::NonManifoldEdge { .. }
            | MeshError::NonManifoldVertex { .. }
            | MeshError::DuplicateTriangle { .. }
    );
    import_diagnostic(
        if is_topology {
            "E505_IMPORT_NON_MANIFOLD_OR_OPEN"
        } else {
            "E504_UNSUPPORTED_IMPORT_FORMAT"
        },
        err.to_string(),
        span,
    )
}

fn make_mesh_geometry(
    bytes: &[u8],
    format: ImportFormat,
    unit: Option<LengthUnit>,
    cluster: bool,
    allow_open_shell: bool,
    span: Span,
) -> Result<ImportedGeometry, Diagnostic> {
    let (mesh, unit, build_items) = match format {
        ImportFormat::Stl => {
            let unit = unit.unwrap_or_default();
            load_stl(bytes, unit).map(|mesh| (mesh, unit, Vec::new()))
        }
        ImportFormat::Obj => {
            let unit = unit.unwrap_or_default();
            load_obj(bytes, unit).map(|mesh| (mesh, unit, Vec::new()))
        }
        ImportFormat::Gltf => {
            let unit = unit.unwrap_or(LengthUnit::Meter);
            load_gltf_with_source_unit(bytes, Some(unit)).map(|mut mesh| {
                mesh.weld_vertices(1e-6);
                (mesh, unit, Vec::new())
            })
        }
        ImportFormat::ThreeMf => load_3mf_document_tolerant(bytes, unit)
            .map(|doc| (doc.mesh, doc.detected_unit, doc.build_items)),
        _ => unreachable!("exact formats are decoded by the OCCT importer"),
    }
    .map_err(|err| mesh_diagnostic(err, span))?;
    let open_issue = match mesh.validate_closed_manifold() {
        Ok(()) => None,
        Err(err) if allow_open_shell => {
            mesh.validate_geometry()
                .map_err(|err| mesh_diagnostic(err, span))?;
            Some(err.to_string())
        }
        Err(err) => return Err(mesh_diagnostic(err, span)),
    };
    let mut solid = SolidShape::empty("imported_mesh");
    solid.exact_brep = false;
    solid.payload = KernelPayload::TriMesh;
    let triangles: Vec<_> = mesh
        .triangles
        .iter()
        .map(|tri| tri.map(|v| mesh.vertices[v as usize]))
        .collect();
    let mut bbox = BBox3::empty();
    for &p in triangles.iter().flatten() {
        bbox.expand_point(p);
    }
    let component_count = build_items.len();
    let mut component_for_triangle = vec![None; triangles.len()];
    for (index, item) in build_items.iter().enumerate() {
        let [min, max] = item.bounds_mm;
        let bounds = BBox3 { min, max };
        let local_triangles = &triangles[item.triangle_range.clone()];
        let volume = if open_issue.is_none() {
            local_triangles
                .iter()
                .map(|tri| {
                    let [a, b, c] = tri.map(|p| super::delta(p, min));
                    super::dot(a, cross(b, c))
                })
                .sum::<f64>()
                .abs()
                / 6.0
        } else {
            0.0
        };
        component_for_triangle[item.triangle_range.clone()].fill(Some(index));
        solid.components.push(ComponentMeta {
            component_id: format!("build_{}", item.occurrence_id),
            part_name: item
                .name
                .clone()
                .unwrap_or_else(|| format!("object_{}", item.object_id)),
            // The 3MF loader has already applied the (potentially nonrigid)
            // affine build transform; these meshes are in world coordinates.
            origin_mm: [0.0; 3],
            rotation_deg: [0.0; 3],
            volume_mm3: volume,
            bbox_mm: bounds,
        });
    }
    let components = &solid.components;
    let component_id =
        |index: usize| component_for_triangle[index].map(|i| components[i].component_id.clone());
    let mut assigned = vec![usize::MAX; triangles.len()];
    if cluster {
        for patch in cluster_analytic_faces(&mesh).map_err(|err| mesh_diagnostic(err, span))? {
            let id = solid.faces.len();
            for &index in &patch.triangle_indices {
                assigned[index as usize] = id;
            }
            let first = patch.triangle_indices[0] as usize;
            let mut face = mesh_face(
                id,
                &triangles,
                patch.triangle_indices.into_iter().map(|i| i as usize),
                Some(patch.surface),
                patch.exact_interface,
            );
            face.component_id = component_id(first);
            solid.faces.push(face);
        }
    }
    let mut add_remaining = |range: std::ops::Range<usize>| {
        let id = solid.faces.len();
        let start = range.start;
        let mut remaining = assigned[range]
            .iter_mut()
            .enumerate()
            .filter_map(|(i, patch)| {
                (*patch == usize::MAX).then(|| {
                    *patch = id;
                    start + i
                })
            })
            .peekable();
        if let Some(&first) = remaining.peek() {
            let mut face = mesh_face(id, &triangles, remaining, None, false);
            face.component_id = component_id(first);
            solid.faces.push(face);
        }
    };
    if build_items.is_empty() {
        add_remaining(0..triangles.len());
    } else {
        for item in &build_items {
            add_remaining(item.triangle_range.clone());
        }
    }
    solid.edges = mesh_edges(&mesh, &assigned, |patch| {
        solid.faces[patch].component_id.clone()
    });
    solid.csg = CsgNode::Mesh {
        bbox,
        triangles,
        closed: open_issue.is_none(),
    };
    Ok(ImportedGeometry {
        solid,
        unit: unit.symbol().into(),
        component_count,
        open_issue,
        exact_open_shell_count: None,
    })
}

fn mesh_face(
    id: usize,
    triangles: &[[[f64; 3]; 3]],
    indices: impl Iterator<Item = usize>,
    surface: Option<AnalyticSurface>,
    exact_interface: bool,
) -> BRepFace {
    let (surface_type, center_hint, normal_hint, radius_hint) = match surface {
        Some(AnalyticSurface::Plane { normal, .. }) => (SurfaceType::Plane, [0.0; 3], normal, None),
        Some(AnalyticSurface::Cylinder {
            origin,
            axis,
            radius,
        }) => (SurfaceType::Cylinder, origin, axis, Some(radius)),
        None => (SurfaceType::Mesh, [0.0; 3], [0.0; 3], None),
    };
    let mut face = BRepFace {
        face_id: format!("mesh_face_{id}"),
        surface_type,
        center_hint,
        normal_hint,
        radius_hint,
        area_mm2: 0.0,
        vertices: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
        outer_loop: Vec::new(),
        top_loop: None,
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface,
        component_id: None,
        ast_span: Span::default(),
    };
    let mut center_sum = [0.0; 3];
    for i in indices {
        let [a, b, c] = triangles[i];
        let u = super::delta(b, a);
        let v = super::delta(c, a);
        let cross = cross(u, v);
        let length = super::dot(cross, cross).sqrt();
        let area = 0.5 * length;
        face.area_mm2 += area;
        for p in [a, b, c] {
            face.indices.push((face.vertices.len() / 3) as u32);
            face.vertices.extend(p);
            face.normals.extend(cross.map(|v| v / length));
            for axis in 0..3 {
                center_sum[axis] += p[axis] * (area / 3.0);
            }
        }
    }
    if surface_type != SurfaceType::Cylinder && face.area_mm2 > 0.0 {
        face.center_hint = center_sum.map(|v| v / face.area_mm2);
    }
    face
}

fn cross(u: [f64; 3], v: [f64; 3]) -> [f64; 3] {
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn mesh_edges(
    mesh: &Mesh,
    assigned: &[usize],
    component_id: impl Fn(usize) -> Option<String>,
) -> Vec<BRepEdge> {
    // Expose patch seams and open boundaries, not every tessellation diagonal.
    let mut incidents = Vec::with_capacity(mesh.triangles.len() * 3);
    for (tri, &patch) in mesh.triangles.iter().zip(assigned) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            incidents.push(([a.min(b), a.max(b)], patch, a < b));
        }
    }
    incidents.sort_unstable_by_key(|e| e.0);
    incidents
        .chunk_by(|a, b| a.0 == b.0)
        .filter(|group| !matches!(group, [a, b] if a.1 == b.1 && a.2 != b.2))
        .enumerate()
        .map(|(id, group)| BRepEdge {
            edge_id: format!("mesh_edge_{id}"),
            curve_type: CurveType::Line,
            polyline: group[0]
                .0
                .into_iter()
                .flat_map(|v| mesh.vertices[v as usize])
                .collect(),
            provenance_tags: Vec::new(),
            component_id: component_id(group[0].1),
            ast_span: Span::default(),
        })
        .collect()
}
