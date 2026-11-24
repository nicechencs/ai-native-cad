use crate::csg::SolidShape;
use crate::step_geometry::{self, Body, Circle, Point, Shell, Surface, VertexKey};
use std::collections::HashMap;
use std::fmt::Write;

/// Marker embedded in `FILE_DESCRIPTION` for analytic preview STEP exports so they
/// can never masquerade as authoritative OCCT B-Rep STEP AP242 deliverables.
pub const NON_AUTHORITATIVE_STEP_MARKER: &str = "NON-AUTHORITATIVE ANALYTIC PREVIEW";

/// Structured diagnostic code reported when authoritative OCCT STEP AP242 export is requested
/// on a non-OCCT solid or a build without `occt-ffi`.
pub const AUTHORITATIVE_KERNEL_REQUIRED_CODE: &str = "AuthoritativeKernelRequired";

/// Provenance authority of a STEP AP242 export payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepExportAuthority {
    /// Exported directly from a native OCCT B-Rep solid handle (`occt-ffi`).
    AuthoritativeOcctBRep,
    /// Synthesized from the analytic preview kernel; not authoritative for manufacturing delivery.
    NonAuthoritativeAnalyticPreview,
}

/// STEP AP242 export output tagged with its kernel authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepExportOutput {
    pub content: String,
    pub authority: StepExportAuthority,
}

/// Exports native B-Rep via OCCT's STEP writer, or analytic preview geometry as STEP AP242.
pub fn export_step_ap242(shape: &SolidShape) -> Result<String, String> {
    try_export_step_ap242(shape).map_err(|error| error.to_string())
}

/// Exports STEP AP242 strictly requiring an authoritative native OCCT B-Rep payload.
///
/// Analytic preview solids and builds without `occt-ffi` are rejected with an
/// [`AUTHORITATIVE_KERNEL_REQUIRED_CODE`] diagnostic so approximate geometry can never
/// masquerade as authoritative STEP AP242.
pub fn try_export_authoritative_step_ap242(
    shape: &SolidShape,
) -> Result<StepExportOutput, crate::KernelOperationError> {
    #[cfg(feature = "occt-ffi")]
    if let crate::KernelPayload::Occt(handle) = &shape.payload {
        let content = handle.export_step(&shape.part_name)?;
        return Ok(StepExportOutput {
            content,
            authority: StepExportAuthority::AuthoritativeOcctBRep,
        });
    }
    #[cfg(not(feature = "occt-ffi"))]
    let _ = shape;
    Err(crate::KernelError::OperationFailed(format!(
        "{AUTHORITATIVE_KERNEL_REQUIRED_CODE}: authoritative OCCT B-Rep kernel is required for precision STEP AP242 export; analytic preview cannot masquerade as authoritative STEP"
    ))
    .into())
}

/// Exports STEP AP242 and returns explicit [`StepExportAuthority`] provenance alongside the content.
pub fn try_export_step_ap242_with_authority(
    shape: &SolidShape,
) -> Result<StepExportOutput, crate::KernelOperationError> {
    #[cfg(feature = "occt-ffi")]
    if let crate::KernelPayload::Occt(handle) = &shape.payload {
        let content = handle.export_step(&shape.part_name)?;
        return Ok(StepExportOutput {
            content,
            authority: StepExportAuthority::AuthoritativeOcctBRep,
        });
    }
    let content = export_analytic_step(shape)
        .map_err(crate::KernelError::OperationFailed)?;
    Ok(StepExportOutput {
        content,
        authority: StepExportAuthority::NonAuthoritativeAnalyticPreview,
    })
}

/// Exports STEP AP242 with the native diagnostic preserved on failure.
pub fn try_export_step_ap242(shape: &SolidShape) -> Result<String, crate::KernelOperationError> {
    try_export_step_ap242_with_authority(shape).map(|output| output.content)
}

fn export_analytic_step(shape: &SolidShape) -> Result<String, String> {
    let part = sanitize_step_str(&shape.part_name);
    let mut step = StepFile {
        out: format!("ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('AI-Native CAD STEP AP242 Export ({NON_AUTHORITATIVE_STEP_MARKER})'),'2;1');\nFILE_NAME('{part}.step','2026-11-18T00:00:00',('AI-Native CAD'),('AI-Native CAD Engine'),'cad-kernel-occt 0.1','cad-cli','');\nFILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF'));\nENDSEC;\nDATA;\n"),
        id: 0,
    };
    let app = step.entity(format_args!(
        "APPLICATION_CONTEXT('managed model based 3d engineering')"
    ));
    step.entity(format_args!("APPLICATION_PROTOCOL_DEFINITION('international standard','ap242_managed_model_based_3d_engineering',2014,#{app})"));
    let product_context = step.entity(format_args!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let definition_context = step.entity(format_args!(
        "PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"
    ));
    let length = step.entity(format_args!(
        "(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))"
    ));
    let angle = step.entity(format_args!(
        "(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))"
    ));
    let solid_angle = step.entity(format_args!(
        "(NAMED_UNIT(*)SOLID_ANGLE_UNIT()SI_UNIT($,.STERADIAN.))"
    ));
    let uncertainty = step.entity(format_args!("UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-04),#{length},'distance_accuracy_value','confusion accuracy')"));
    let context = step.entity(format_args!("(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{length},#{angle},#{solid_angle}))REPRESENTATION_CONTEXT('Context3D','3D'))"));
    let placement = step.axis([0.; 3], [0., 0., 1.], [1., 0., 0.]);
    let root = step.product(&part, product_context, definition_context);

    if shape.components.is_empty() {
        let bodies = step_geometry::prepare_bodies(&shape.faces)?;
        step.solid_representation(&part, root, context, placement, &bodies);
    } else {
        let parent_rep = step.entity(format_args!(
            "SHAPE_REPRESENTATION('{part}',(#{placement}),#{context})"
        ));
        step.link_shape(root, parent_rep);
        for comp in &shape.components {
            let id = sanitize_step_str(&comp.component_id);
            let name = sanitize_step_str(&comp.part_name);
            let bodies =
                step_geometry::prepare_bodies(shape.faces.iter().filter(|face| {
                    face.component_id.as_deref() == Some(comp.component_id.as_str())
                }))
                .map_err(|error| format!("component {id}: {error}"))?;
            let child = step.product(&name, product_context, definition_context);
            let child_rep = step.solid_representation(&name, child, context, placement, &bodies);
            let usage = step.entity(format_args!(
                "NEXT_ASSEMBLY_USAGE_OCCURRENCE('{id}','{name}','',#{root},#{child},$)"
            ));
            let usage_shape = step.entity(format_args!("PRODUCT_DEFINITION_SHAPE('','',#{usage})"));
            // Component faces already carry the evaluated assembly placement.
            let transform = step.entity(format_args!(
                "ITEM_DEFINED_TRANSFORMATION('','',#{placement},#{placement})"
            ));
            let relation = step.entity(format_args!("(REPRESENTATION_RELATIONSHIP('','',#{child_rep},#{parent_rep})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{transform})SHAPE_REPRESENTATION_RELATIONSHIP())"));
            step.entity(format_args!(
                "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{relation},#{usage_shape})"
            ));
        }
    }

    for pmi in &shape.pmi {
        let kind = sanitize_step_str(&pmi.kind);
        let label = sanitize_step_str(&pmi.label);
        let tol = pmi.tolerance_mm.unwrap_or(0.0);
        step.entity(format_args!(
            "DESCRIPTIVE_REPRESENTATION_ITEM('PMI_{kind}','{label} tol={tol:.4}mm')"
        ));
    }
    step.out.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    Ok(step.out)
}

struct StepFile {
    out: String,
    id: usize,
}

impl StepFile {
    fn entity(&mut self, definition: std::fmt::Arguments<'_>) -> usize {
        self.id += 1;
        let _ = writeln!(self.out, "#{}={definition};", self.id);
        self.id
    }

    fn point(&mut self, [x, y, z]: Point) -> usize {
        self.entity(format_args!(
            "CARTESIAN_POINT('',({x:.15E},{y:.15E},{z:.15E}))"
        ))
    }

    fn direction(&mut self, [x, y, z]: Point) -> usize {
        self.entity(format_args!("DIRECTION('',({x:.15E},{y:.15E},{z:.15E}))"))
    }

    fn axis(&mut self, origin: Point, axis: Point, radial: Point) -> usize {
        let origin = self.point(origin);
        let axis = self.direction(axis);
        let radial = self.direction(radial);
        self.entity(format_args!(
            "AXIS2_PLACEMENT_3D('',#{origin},#{axis},#{radial})"
        ))
    }

    fn product(&mut self, name: &str, product_context: usize, definition_context: usize) -> usize {
        let product = self.entity(format_args!(
            "PRODUCT('{name}','{name}','',(#{product_context}))"
        ));
        let formation = self.entity(format_args!(
            "PRODUCT_DEFINITION_FORMATION('1','',#{product})"
        ));
        self.entity(format_args!(
            "PRODUCT_DEFINITION('design','',#{formation},#{definition_context})"
        ))
    }

    fn link_shape(&mut self, product: usize, representation: usize) {
        let definition = self.entity(format_args!("PRODUCT_DEFINITION_SHAPE('','',#{product})"));
        self.entity(format_args!(
            "SHAPE_DEFINITION_REPRESENTATION(#{definition},#{representation})"
        ));
    }

    fn solid_representation(
        &mut self,
        name: &str,
        product: usize,
        context: usize,
        placement: usize,
        bodies: &[Body<'_>],
    ) -> usize {
        let mut items = vec![placement];
        for body in bodies {
            let outer = self.shell(&body.outer, false);
            let solid = if body.voids.is_empty() {
                self.entity(format_args!("MANIFOLD_SOLID_BREP('{name}',#{outer})"))
            } else {
                let voids: Vec<_> = body
                    .voids
                    .iter()
                    .map(|shell| {
                        let shell = self.shell(shell, true);
                        self.entity(format_args!("ORIENTED_CLOSED_SHELL('',*,#{shell},.F.)"))
                    })
                    .collect();
                self.entity(format_args!(
                    "BREP_WITH_VOIDS('{name}',#{outer},({}))",
                    refs(&voids)
                ))
            };
            items.push(solid);
        }
        let rep = self.entity(format_args!(
            "ADVANCED_BREP_SHAPE_REPRESENTATION('{name}',({}),#{context})",
            refs(&items)
        ));
        self.link_shape(product, rep);
        rep
    }

    fn shell(&mut self, shell: &Shell<'_>, reverse: bool) -> usize {
        let mut vertices = HashMap::new();
        let mut edges = HashMap::new();
        let mut faces = Vec::with_capacity(shell.faces.len());
        for face in &shell.faces {
            let mut bounds = Vec::new();
            for (i, ring) in face.loops.iter().enumerate() {
                let mut oriented = Vec::new();
                let mut append = |a, b| {
                    let key = step_geometry::edge_key(a, b);
                    let forward = step_geometry::vertex_key(a) == key.0;
                    let edge = *edges.entry(key).or_insert_with(|| {
                        let (a, b) = if forward { (a, b) } else { (b, a) };
                        self.edge(a, b, shell.arcs.get(&key).copied(), &mut vertices)
                    });
                    oriented.push(self.entity(format_args!(
                        "ORIENTED_EDGE('',*,*,#{edge},{})",
                        logical(forward)
                    )));
                };
                if reverse {
                    for index in (0..ring.len()).rev() {
                        append(ring[(index + 1) % ring.len()], ring[index]);
                    }
                } else {
                    for (a, b) in step_geometry::loop_edges(ring) {
                        append(a, b);
                    }
                }
                let wire = self.entity(format_args!("EDGE_LOOP('',({}))", refs(&oriented)));
                let bound_type = if i == 0 {
                    "FACE_OUTER_BOUND"
                } else {
                    "FACE_BOUND"
                };
                bounds.push(self.entity(format_args!("{bound_type}('',#{wire},.T.)")));
            }
            let label = sanitize_step_str(&face.source.provenance_tags.join(","));
            let (surface, outward) = match face.surface {
                Surface::Plane { origin, normal } => {
                    let reference = if normal[2].abs() < 0.9 {
                        [0., 0., 1.]
                    } else {
                        [1., 0., 0.]
                    };
                    let axis = self.axis(origin, normal, step_geometry::cross(reference, normal));
                    (self.entity(format_args!("PLANE('{label}',#{axis})")), true)
                }
                Surface::Round {
                    origin,
                    axis,
                    radial,
                    radius,
                    semi_angle,
                    outward,
                } => {
                    let axis = self.axis(origin, axis, radial);
                    let surface = if semi_angle == 0. {
                        self.entity(format_args!(
                            "CYLINDRICAL_SURFACE('{label}',#{axis},{radius:.15E})"
                        ))
                    } else {
                        self.entity(format_args!(
                            "CONICAL_SURFACE('{label}',#{axis},{radius:.15E},{semi_angle:.15E})"
                        ))
                    };
                    (surface, outward)
                }
            };
            faces.push(self.entity(format_args!(
                "ADVANCED_FACE('{label}',({}),#{surface},{})",
                refs(&bounds),
                logical(outward != reverse)
            )));
        }
        self.entity(format_args!("CLOSED_SHELL('',({}))", refs(&faces)))
    }

    fn edge(
        &mut self,
        a: Point,
        b: Point,
        arc: Option<Circle>,
        vertices: &mut HashMap<VertexKey, (usize, Point)>,
    ) -> usize {
        let [(start, a), (end, b)] = [a, b].map(|p| {
            *vertices
                .entry(step_geometry::vertex_key(p))
                .or_insert_with(|| {
                    let pt = self.point(p);
                    (self.entity(format_args!("VERTEX_POINT('',#{pt})")), p)
                })
        });
        let curve = if let Some(Circle {
            center,
            axis,
            radius,
        }) = arc
        {
            let ra = step_geometry::sub(a, center);
            let rb = step_geometry::sub(b, center);
            let cross = step_geometry::cross(ra, rb);
            let orientation: f64 = axis.iter().zip(cross).map(|(x, y)| x * y).sum();
            let axis = if orientation < 0. {
                axis.map(|x| -x)
            } else {
                axis
            };
            let placement = self.axis(center, axis, ra);
            self.entity(format_args!("CIRCLE('',#{placement},{radius:.15E})"))
        } else {
            let change = step_geometry::sub(b, a);
            let length = step_geometry::length(change);
            let point = self.point(a);
            let dir = self.direction(change.map(|x| x / length));
            let vector = self.entity(format_args!("VECTOR('',#{dir},{length:.15E})"));
            self.entity(format_args!("LINE('',#{point},#{vector})"))
        };
        self.entity(format_args!("EDGE_CURVE('',#{start},#{end},#{curve},.T.)"))
    }
}

fn refs(ids: &[usize]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn logical(value: bool) -> &'static str {
    if value {
        ".T."
    } else {
        ".F."
    }
}

fn sanitize_step_str(s: &str) -> String {
    s.chars().map(|c| if c == '\'' { '_' } else { c }).collect()
}
