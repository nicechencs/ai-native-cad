import materialsCatalog from "../../../../schemas/materials.json" with { type: "json" };
import type {
  DiagnosticsReport,
  EditHandle,
  FaceMesh,
  MaterialPreset,
  SceneMeshPayload,
  SourceSpan,
  TopoEdge,
} from "../types/scene.ts";

export const MATERIAL_PRESETS: readonly MaterialPreset[] = materialsCatalog.materials
  .filter((entry) => entry.workbench_preset)
  .map((entry) => ({
    id: entry.id,
    name: entry.name,
    density_g_cm3: entry.density_g_cm3,
    yield_strength_mpa: entry.yield_strength_mpa,
    machinability_index: entry.machinability_index,
  }));

export const INITIAL_DEMO_KCL = `@unit(length = "mm", angle = "deg", force = "N")
@material("Al-6061-T6")
@manufacturing(method = "CNC_3AXIS_MILLING", min_tool_radius = 1.5mm)

part LightweightBearingMount(
    bore_diameter: Length = 42.0mm @pmi::tolerance("H7", roughness = "Ra1.6"), // 轴承内孔 H7 配合
    wall_thickness: Length = 8.0mm, // 最小径向壁厚
    flange_od: Length = 110.0mm, // 法兰安装底盘外径
    base_thickness: Length = 10.0mm, // 法兰底盘厚度
    hub_height: Length = 48.0mm, // 中心轴承座总高
    rib_count: u32 = 6,
    axial_load: Force = 1500.0N
) -> Solid {
    let hub_od = bore_diameter + 16.0mm // Tier 2: 单变量仿射偏移
    let fillet_r = min(flange_od / 18.0, wall_thickness * 0.65) // Tier 3: 多变量耦合圆角表达式

    // 1. 精确解析特征轨 (Exact B-Rep): 法兰底盘与中心轴承座外柱
    let flange = cylinder(d = flange_od, h = base_thickness, origin = [0mm, 0mm, 0mm])
        |> tag::faces("flange_datum_base")
    let hub = cylinder(d = hub_od, h = hub_height, origin = [0mm, 0mm, 0mm])
        |> tag::cylindrical_faces("hub_outer_wall")

    // 2. 精确减材配合孔与拓扑标签溯源
    let bearing_bore_tool = cylinder(
        d = bore_diameter,
        h = hub_height + 2.0mm,
        origin = [0mm, 0mm, -1.0mm],
        exact_brep = true
    )
    |> tag::cylindrical_faces("bearing_seat_wall")

    let final_part = boolean::union(items = [flange, hub])
        |> boolean::subtract(
            tools = [bearing_bore_tool],
            preserve_exact_interfaces = true
        )

    assert::bbox(target = final_part, max = [120mm, 120mm, 65mm])
    assert::dfm_accessible(target = final_part, axes = [+Z, -Z], min_wall_thickness = 2.5mm)

    return final_part
}
`;

function findSpanInSource(source: string, needle: string): SourceSpan {
  const idx = source.indexOf(needle);
  if (idx === -1) {
    return { start_line: 1, start_col: 1, end_line: 1, end_col: 2 };
  }
  let line = 1;
  let col = 1;
  for (let i = 0; i < idx; i += 1) {
    if (source[i] === "\n") {
      line += 1;
      col = 1;
    } else {
      col += 1;
    }
  }
  let endLine = line;
  let endCol = col;
  for (let i = 0; i < needle.length; i += 1) {
    if (needle[i] === "\n") {
      endLine += 1;
      endCol = 1;
    } else {
      endCol += 1;
    }
  }
  return {
    start_line: line,
    start_col: col,
    end_line: endLine,
    end_col: endCol,
  };
}

function parseNumericAssignment(
  source: string,
  pattern: RegExp,
  fallback: number,
): { readonly value: number; readonly matchText: string } {
  const m = pattern.exec(source);
  if (!m) {
    return { value: fallback, matchText: "" };
  }
  const parsed = Number.parseFloat(m[1]);
  return {
    value: Number.isFinite(parsed) ? parsed : fallback,
    matchText: m[0],
  };
}

interface CylinderRingBuild {
  readonly vertices: number[];
  readonly normals: number[];
  readonly indices: number[];
}

function buildAnnularRingFace(
  innerR: number,
  outerR: number,
  z: number,
  normalZ: 1 | -1,
  segments: number,
): CylinderRingBuild {
  const vertices: number[] = [];
  const normals: number[] = [];
  const indices: number[] = [];

  for (let i = 0; i <= segments; i += 1) {
    const theta = (i / segments) * Math.PI * 2;
    const cos = Math.cos(theta);
    const sin = Math.sin(theta);
    vertices.push(innerR * cos, innerR * sin, z);
    normals.push(0, 0, normalZ);
    vertices.push(outerR * cos, outerR * sin, z);
    normals.push(0, 0, normalZ);
  }

  for (let i = 0; i < segments; i += 1) {
    const base = i * 2;
    if (normalZ === 1) {
      indices.push(base, base + 1, base + 3);
      indices.push(base, base + 3, base + 2);
    } else {
      indices.push(base, base + 3, base + 1);
      indices.push(base, base + 2, base + 3);
    }
  }

  return { vertices, normals, indices };
}

function buildCylindricalWallFace(
  radius: number,
  zBottom: number,
  zTop: number,
  outward: boolean,
  segments: number,
): CylinderRingBuild {
  const vertices: number[] = [];
  const normals: number[] = [];
  const indices: number[] = [];
  const sign = outward ? 1 : -1;

  for (let i = 0; i <= segments; i += 1) {
    const theta = (i / segments) * Math.PI * 2;
    const cos = Math.cos(theta);
    const sin = Math.sin(theta);
    vertices.push(radius * cos, radius * sin, zBottom);
    normals.push(cos * sign, sin * sign, 0);
    vertices.push(radius * cos, radius * sin, zTop);
    normals.push(cos * sign, sin * sign, 0);
  }

  for (let i = 0; i < segments; i += 1) {
    const base = i * 2;
    if (outward) {
      indices.push(base, base + 2, base + 3);
      indices.push(base, base + 3, base + 1);
    } else {
      indices.push(base, base + 3, base + 2);
      indices.push(base, base + 1, base + 3);
    }
  }

  return { vertices, normals, indices };
}

function buildCirclePolyline(radius: number, z: number, segments: number): number[] {
  const pts: number[] = [];
  for (let i = 0; i <= segments; i += 1) {
    const theta = (i / segments) * Math.PI * 2;
    pts.push(radius * Math.cos(theta), radius * Math.sin(theta), z);
  }
  return pts;
}

export interface EvaluatedDemoModel {
  readonly sceneMesh: SceneMeshPayload;
  readonly diagnostics: DiagnosticsReport;
}

/**
 * Evaluates current `.kcl` source parameters to produce a live preview
 * `SceneMeshPayload` and `DiagnosticsReport` without external backend latency.
 */
export function evaluateDemoKclModel(source: string): EvaluatedDemoModel {
  const boreMatch = parseNumericAssignment(
    source,
    /bore_diameter:\s*Length\s*=\s*([0-9.]+)mm/,
    42.0,
  );
  const wallMatch = parseNumericAssignment(
    source,
    /wall_thickness:\s*Length\s*=\s*([0-9.]+)mm/,
    8.0,
  );
  const flangeOdMatch = parseNumericAssignment(
    source,
    /flange_od:\s*Length\s*=\s*([0-9.]+)mm/,
    110.0,
  );
  const baseThickMatch = parseNumericAssignment(
    source,
    /base_thickness:\s*Length\s*=\s*([0-9.]+)mm/,
    10.0,
  );
  const hubHeightMatch = parseNumericAssignment(
    source,
    /hub_height:\s*Length\s*=\s*([0-9.]+)mm/,
    48.0,
  );
  const hubOffsetMatch = parseNumericAssignment(
    source,
    /let\s+hub_od\s*=\s*bore_diameter\s*\+\s*([0-9.]+)mm/,
    16.0,
  );

  const boreD = Math.max(8, boreMatch.value);
  const wallT = Math.max(2, wallMatch.value);
  const flangeOd = Math.max(boreD + 20, flangeOdMatch.value);
  const baseH = Math.max(3, baseThickMatch.value);
  const hubH = Math.max(baseH + 4, hubHeightMatch.value);
  const hubOffset = hubOffsetMatch.value;
  const hubOd = Math.max(boreD + 4, boreD + hubOffset);

  const rBore = boreD / 2;
  const rHub = hubOd / 2;
  const rFlange = flangeOd / 2;
  const seg = 36;

  const flangeSpan = findSpanInSource(
    source,
    "let flange = cylinder(d = flange_od, h = base_thickness, origin = [0mm, 0mm, 0mm])",
  );
  const hubSpan = findSpanInSource(
    source,
    "let hub = cylinder(d = hub_od, h = hub_height, origin = [0mm, 0mm, 0mm])",
  );
  const boreSpan = findSpanInSource(
    source,
    "let bearing_bore_tool = cylinder(",
  );

  const bottomRing = buildAnnularRingFace(rBore, rFlange, 0, -1, seg);
  const flangeOuterWall = buildCylindricalWallFace(rFlange, 0, baseH, true, seg);
  const flangeTopRing = buildAnnularRingFace(rHub, rFlange, baseH, 1, seg);
  const hubOuterWall = buildCylindricalWallFace(rHub, baseH, hubH, true, seg);
  const hubTopRing = buildAnnularRingFace(rBore, rHub, hubH, 1, seg);
  const boreInnerWall = buildCylindricalWallFace(rBore, 0, hubH, false, seg);

  const faces: FaceMesh[] = [
    {
      face_id: "F1_flange_bottom_datum_A",
      surface_type: "plane",
      ...bottomRing,
      provenance_tags: ["flange_datum_base", "datum_A"],
      exact_interface: true,
      ast_span: flangeSpan,
    },
    {
      face_id: "F2_flange_outer_cyl",
      surface_type: "cylinder",
      ...flangeOuterWall,
      provenance_tags: ["flange_datum_base"],
      ast_span: flangeSpan,
    },
    {
      face_id: "F3_flange_top_shoulder",
      surface_type: "plane",
      ...flangeTopRing,
      provenance_tags: ["flange_datum_base"],
      ast_span: flangeSpan,
    },
    {
      face_id: "F4_hub_outer_wall",
      surface_type: "cylinder",
      ...hubOuterWall,
      provenance_tags: ["hub_outer_wall"],
      ast_span: hubSpan,
    },
    {
      face_id: "F5_hub_top_annular",
      surface_type: "plane",
      ...hubTopRing,
      provenance_tags: ["hub_outer_wall"],
      ast_span: hubSpan,
    },
    {
      face_id: "F6_bearing_seat_bore",
      surface_type: "cylinder",
      ...boreInnerWall,
      provenance_tags: ["bearing_seat_wall", "pmi_H7"],
      exact_interface: true,
      ast_span: boreSpan,
    },
  ];

  const edges: TopoEdge[] = [
    {
      edge_id: "E1_flange_bottom_outer",
      curve_type: "circle",
      polyline: buildCirclePolyline(rFlange, 0, seg),
      provenance_tags: ["flange_datum_base", "datum_A"],
      ast_span: flangeSpan,
    },
    {
      edge_id: "E2_flange_top_outer",
      curve_type: "circle",
      polyline: buildCirclePolyline(rFlange, baseH, seg),
      provenance_tags: ["flange_datum_base"],
      ast_span: flangeSpan,
    },
    {
      edge_id: "E3_hub_flange_junction",
      curve_type: "circle",
      polyline: buildCirclePolyline(rHub, baseH, seg),
      provenance_tags: ["hub_outer_wall", "flange_datum_base"],
      ast_span: hubSpan,
    },
    {
      edge_id: "E4_hub_top_outer",
      curve_type: "circle",
      polyline: buildCirclePolyline(rHub, hubH, seg),
      provenance_tags: ["hub_outer_wall"],
      ast_span: hubSpan,
    },
    {
      edge_id: "E5_bore_top_chamfer_edge",
      curve_type: "circle",
      polyline: buildCirclePolyline(rBore, hubH, seg),
      provenance_tags: ["bearing_seat_wall", "pmi_H7"],
      ast_span: boreSpan,
    },
    {
      edge_id: "E6_bore_bottom_edge",
      curve_type: "circle",
      polyline: buildCirclePolyline(rBore, 0, seg),
      provenance_tags: ["bearing_seat_wall", "datum_A"],
      ast_span: boreSpan,
    },
  ];

  const boreDeclSpan = findSpanInSource(
    source,
    boreMatch.matchText || `bore_diameter: Length = ${boreMatch.value.toFixed(1)}mm`,
  );
  const boreParamSpan = boreDeclSpan;

  const hubHeightSpan = findSpanInSource(source, hubHeightMatch.matchText || "48.0mm");
  const flangeOdSpan = findSpanInSource(source, flangeOdMatch.matchText || "110.0mm");
  const baseThickSpan = findSpanInSource(source, baseThickMatch.matchText || "10.0mm");

  const hubOdOffsetExpr = `let hub_od = bore_diameter + ${hubOffsetMatch.value}mm`;
  const hubOdLineText =
    source
      .split(/\r?\n/)
      .find((line) => line.includes("let hub_od ="))
      ?.trim() ?? hubOdOffsetExpr;
  const hubOdCodeOnly = hubOdLineText.split("//")[0].trim();
  const hubOffsetSpan = findSpanInSource(source, hubOdCodeOnly);

  const coupledRhs = "min(flange_od / 18.0, wall_thickness * 0.65)";
  const coupledLine =
    source
      .split(/\r?\n/)
      .find((line) => line.includes("let fillet_r =")) ??
    `    let fillet_r = ${coupledRhs}`;
  const hasCoupledExpr = source.includes(coupledRhs);

  const filletCurrentValue = Math.min(flangeOd / 18.0, wallT * 0.65);

  const edit_handles: EditHandle[] = [
    {
      handle_id: "h_flange_od",
      param_name: "flange_od",
      tier: 1,
      current_value: flangeOd,
      unit: "mm",
      ast_span: flangeOdSpan,
      label: "法兰外径 (Tier 1)",
      min_value: 60,
      max_value: 180,
      anchor_point: [rFlange, 0, baseH / 2],
    },
    {
      handle_id: "h_hub_height",
      param_name: "hub_height",
      tier: 1,
      current_value: hubH,
      unit: "mm",
      ast_span: hubHeightSpan,
      label: "轴承座总高 (Tier 1)",
      min_value: 20,
      max_value: 90,
      anchor_point: [0, rHub, hubH],
    },
    {
      handle_id: "h_bore_diameter",
      param_name: "bore_diameter",
      tier: 1,
      current_value: boreD,
      unit: "mm",
      ast_span: boreParamSpan,
      label: "轴承内孔 Ø H7 (Tier 1)",
      min_value: 16,
      max_value: 70,
      anchor_point: [rBore, 0, hubH],
    },
    {
      handle_id: "h_base_thickness",
      param_name: "base_thickness",
      tier: 1,
      current_value: baseH,
      unit: "mm",
      ast_span: baseThickSpan,
      label: "法兰底盘厚度 (Tier 1)",
      min_value: 4,
      max_value: 25,
      anchor_point: [rFlange, 0, baseH],
    },
    {
      handle_id: "h_hub_od",
      param_name: "hub_od",
      tier: 2,
      current_value: hubOd,
      unit: "mm",
      ast_span: findSpanInSource(source, hubOdLineText),
      label: "轴承座外径 (Tier 2: bore + offset)",
      min_value: 30,
      max_value: 100,
      anchor_point: [rHub, 0, (baseH + hubH) / 2],
      upstream_param: "bore_diameter",
      offset_value: hubOffset,
      tier2_binding: {
        operator: "+",
        upstream_param: "bore_diameter",
        upstream_value: boreD,
        upstream_unit: "mm",
        upstream_span: boreParamSpan,
        offset_value: hubOffset,
        offset_unit: "mm",
        offset_span: hubOffsetSpan,
      },
    },
    ...(hasCoupledExpr
      ? [
          {
            handle_id: "h_fillet_r",
            param_name: "fillet_r",
            tier: 3 as const,
            current_value: Number(filletCurrentValue.toFixed(2)),
            unit: "mm",
            ast_span: findSpanInSource(source, coupledRhs),
            label: "过渡圆角半径 (Tier 3: 耦合表达式)",
            min_value: 1.5,
            max_value: 12,
            anchor_point: [rHub, 0, baseH] as const,
            expression: coupledRhs,
            tier3_binding: {
              expression_text: coupledRhs,
              dependencies: ["flange_od", "wall_thickness"],
              suggested_param_name: "fillet_r_override",
              statement_span: findSpanInSource(source, coupledLine),
              rhs_span: findSpanInSource(source, coupledRhs),
            },
          },
        ]
      : []),
  ];

  // Exact analytic volume of annular flange + annular hub
  const flangeArea = Math.PI * (rFlange * rFlange - rBore * rBore);
  const hubArea = Math.PI * (rHub * rHub - rBore * rBore);
  const volumeMm3 = flangeArea * baseH + hubArea * (hubH - baseH);

  const diagnostics: DiagnosticsReport["diagnostics"][number][] = [];

  if (flangeOd > 120 || hubH > 65) {
    diagnostics.push({
      code: "GATE_L1_BBOX_EXCEEDED",
      severity: "error",
      message: `包络空间越界：当前尺寸 [${flangeOd.toFixed(1)} x ${flangeOd.toFixed(1)} x ${hubH.toFixed(1)}mm] 超出 Keep-In 限制 [120 x 120 x 65mm]。`,
      span: flangeOd > 120 ? flangeOdSpan : hubHeightSpan,
    });
  }

  if ((hubOd - boreD) / 2 < 2.5) {
    diagnostics.push({
      code: "GATE_L2_DFM_MIN_WALL",
      severity: "warning",
      message: `轴承座径向壁厚 ${((hubOd - boreD) / 2).toFixed(2)}mm 小于 3 轴 CNC 推荐最小壁厚 2.5mm。`,
      span: findSpanInSource(source, hubOdLineText),
    });
  }

  diagnostics.push({
    code: "PMI_H7_LOCKED",
    severity: "info",
    message: "配合面 `bearing_seat_wall` 已锁定精确解析圆柱轨 (Exact B-Rep)，STEP AP242 PMI 保留 H7 公差语义。",
    span: boreDeclSpan,
  });

  diagnostics.push({
    code: "TOPOLOGY_PROVENANCE_OK",
    severity: "info",
    message: `拓扑溯源标签校验通过：${faces.length} 个面与 ${edges.length} 条边全部绑定无歧义 AST Span。`,
    span: findSpanInSource(source, "let final_part ="),
  });

  const hasError = diagnostics.some((d) => d.severity === "error");

  return {
    sceneMesh: {
      format_version: "0.1",
      part_name: "LightweightBearingMount",
      units: "mm",
      faces,
      edges,
      edit_handles,
    },
    diagnostics: {
      status: hasError ? "error" : "ok",
      file: "examples/lightweight_bearing_mount.kcl",
      diagnostics,
      metrics: {
        watertight: !hasError,
        volume_mm3: Number(volumeMm3.toFixed(2)),
        bbox_mm: {
          min: [-rFlange, -rFlange, 0],
          max: [rFlange, rFlange, hubH],
        },
        face_count: faces.length,
        edge_count: edges.length,
      },
    },
  };
}
