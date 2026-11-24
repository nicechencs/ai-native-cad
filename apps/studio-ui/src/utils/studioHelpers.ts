import type {
  EditHandle,
  GeometryMetrics,
  LspCompletionItem,
  LspHoverInfo,
  LspQueryResponse,
  SceneMeshPayload,
  TopoEdge,
  Vec3,
} from "../types/scene.ts";

const KCL_BUILTIN_COMPLETIONS: readonly LspCompletionItem[] = [
  {
    label: "shell",
    kind: "operator",
    detail: "(thickness: Length, open_faces?: Face[]) -> Solid",
    insert_text: "shell(thickness = 2.5mm, open_faces = [top_face])",
  },
  {
    label: "sweep",
    kind: "operator",
    detail: "(profile: Sketch, path: Edge[]) -> Solid",
    insert_text: "sweep(profile = section_sketch, path = spine_path)",
  },
  {
    label: "loft",
    kind: "operator",
    detail: "(sections: Sketch[]) -> Solid",
    insert_text: "loft(sections = [base_sketch, top_sketch])",
  },
  {
    label: "mirror",
    kind: "operator",
    detail: "(plane: XY_PLANE | XZ_PLANE | YZ_PLANE) -> Solid",
    insert_text: "mirror(plane = YZ_PLANE)",
  },
  {
    label: "hole::tapped",
    kind: "namespace",
    detail: "(spec: String, depth: Length) -> Solid",
    insert_text: 'hole::tapped(spec = "M6x1.0-6H", depth = 12.0mm)',
  },
  {
    label: "hole::counterbore",
    kind: "namespace",
    detail: "(d_through: Length, d_cbore: Length, depth_cbore: Length) -> Solid",
    insert_text:
      "hole::counterbore(d_through = 6.6mm, d_cbore = 11.0mm, depth_cbore = 6.2mm)",
  },
  {
    label: "hole::simple",
    kind: "namespace",
    detail: "(d: Length, depth: Length) -> Solid",
    insert_text: "hole::simple(d = 6.6mm, depth = 15.0mm)",
  },
  {
    label: "fillet",
    kind: "operator",
    detail: "(r: Length) -> Solid",
    insert_text: "fillet(r = 2.0mm)",
  },
  {
    label: "chamfer",
    kind: "operator",
    detail: "(d: Length) -> Solid",
    insert_text: "chamfer(d = 1.5mm)",
  },
  {
    label: "boolean::subtract",
    kind: "namespace",
    detail: "(tools: Solid[], preserve_exact_interfaces?: bool) -> Solid",
    insert_text:
      "boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)",
  },
  {
    label: "boolean::union",
    kind: "namespace",
    detail: "(items: Solid[]) -> Solid",
    insert_text: "boolean::union(items = [part_a, part_b])",
  },
  {
    label: "mate::coaxial",
    kind: "namespace",
    detail: "(a: Component, b: Component, axis?: Axis) -> Constraint",
    insert_text: "mate::coaxial(a = housing, b = cover, axis = Z_AXIS)",
  },
  {
    label: "mate::coincident",
    kind: "namespace",
    detail: "(a: Face, b: Face, offset?: Length) -> Constraint",
    insert_text: "mate::coincident(a = housing_top, b = cover_bottom, offset = 0mm)",
  },
  {
    label: "assembly",
    kind: "keyword",
    detail: "assembly Name() -> Assembly { ... }",
    insert_text: "assembly MainAssembly() -> Assembly {\n    return []\n}",
  },
  {
    label: "component",
    kind: "keyword",
    detail: "component(part = PartCall, origin = [x, y, z], rotation = [rx, ry, rz])",
    insert_text:
      "component(part = BasePart(), origin = [0mm, 0mm, 0mm], rotation = [0deg, 0deg, 0deg])",
  },
  {
    label: "ellipse",
    kind: "operator",
    detail: "(rx: Length, ry: Length, center?: Length[2], segments?: u32) -> SketchEntity",
    insert_text: "ellipse(rx = 30.0mm, ry = 18.0mm, center = [0mm, 0mm])",
  },
  {
    label: "profile::ellipse",
    kind: "namespace",
    detail: "(rx: Length, ry: Length, center?: Length[2], segments?: u32, plane?: Plane) -> Sketch2D",
    insert_text: "profile::ellipse(rx = 30.0mm, ry = 18.0mm, center = [0mm, 0mm], plane = XY_PLANE)",
  },
  {
    label: "spline",
    kind: "operator",
    detail: "(points: Length[2][], closed?: bool, segments_per_span?: u32) -> SketchEntity",
    insert_text:
      "spline(points = [[24mm, 0mm], [0mm, 16mm], [-24mm, 0mm], [0mm, -16mm]], closed = true)",
  },
  {
    label: "profile::spline",
    kind: "namespace",
    detail: "(points: Length[2][], closed?: bool, segments_per_span?: u32, plane?: Plane) -> Sketch2D",
    insert_text:
      "profile::spline(points = [[24mm, 0mm], [0mm, 16mm], [-24mm, 0mm], [0mm, -16mm]], closed = true, plane = XY_PLANE)",
  },
  {
    label: "path::helix",
    kind: "namespace",
    detail:
      "(radius: Length, pitch: Length, turns?: f64, origin?: Length[3], segments_per_turn?: u32) -> Length[3][]",
    insert_text:
      "path::helix(radius = 20.0mm, pitch = 12.0mm, turns = 2.0, origin = [0mm, 0mm, 0mm])",
  },
  {
    label: "path::spline3d",
    kind: "namespace",
    detail:
      "(points: Length[3][], closed?: bool, segments_per_span?: u32) -> Length[3][]",
    insert_text:
      "path::spline3d(points = [[0mm, 0mm, 0mm], [20mm, 15mm, 18mm], [45mm, 0mm, 36mm]], segments_per_span = 12)",
  },
];

const BUILTIN_HOVER_DOCS: Readonly<Record<string, LspHoverInfo>> = {
  shell: {
    symbol: "shell",
    dimension_type: "(thickness: Length, open_faces?: Face[]) -> Solid",
    summary: "等壁厚抽壳算子：生成闭合流形内外壁薄壳，自动校验壁厚合法性。",
  },
  sweep: {
    symbol: "sweep",
    dimension_type: "(profile: Sketch, path: Edge[]) -> Solid",
    summary: "路径扫掠算子：沿 3D 折线或圆弧路径扫掠二维闭合草图生成管道/流道。",
  },
  loft: {
    symbol: "loft",
    dimension_type: "(sections: Sketch[]) -> Solid",
    summary: "多截面放样算子：在多个有序草图截面间生成平滑过渡实体。",
  },
  mirror: {
    symbol: "mirror",
    dimension_type: "(plane: PlaneConstant) -> Solid",
    summary: "对称镜像算子：关于 XY_PLANE / XZ_PLANE / YZ_PLANE 镜像并合并特征。",
  },
  "hole::tapped": {
    symbol: "hole::tapped",
    dimension_type: "(spec: String, depth: Length) -> Solid",
    summary: "ISO 公制螺纹底孔算子（M2–M20 自动匹配底孔径并附加 PMI 螺纹语义）。",
  },
  "mate::coaxial": {
    symbol: "mate::coaxial",
    dimension_type: "(a: Component, b: Component, axis?: Axis) -> Mate",
    summary: "装配体同轴配合约束：对齐两个子组件的旋转轴线。",
  },
  "mate::coincident": {
    symbol: "mate::coincident",
    dimension_type: "(a: Face, b: Face, offset?: Length) -> Mate",
    summary: "装配体贴合/重合配合约束：锁定子组件装配基准面。",
  },
  assembly: {
    symbol: "assembly",
    dimension_type: "Assembly Declaration",
    summary: "声明多子组件装配体，支持 BOM 拆件、爆炸视图与 STEP AP242 装配树导出。",
  },
  ellipse: {
    symbol: "ellipse",
    dimension_type: "(rx: Length, ry: Length, center?: Length[2], segments?: u32) -> SketchEntity",
    summary: "草图椭圆轮廓原语：通过正长短半轴 rx / ry 生成逆时针闭合椭圆边界。",
  },
  "profile::ellipse": {
    symbol: "profile::ellipse",
    dimension_type: "(rx: Length, ry: Length, center?: Length[2], segments?: u32, plane?: Plane) -> Sketch2D",
    summary: "快捷二维椭圆截面原语：直接返回可传入 extrude / loft / sweep 的闭合 Sketch2D 截面。",
  },
  spline: {
    symbol: "spline",
    dimension_type: "(points: Length[2][], closed?: bool, segments_per_span?: u32) -> SketchEntity",
    summary: "草图样条曲线原语：基于控制点环生成平滑 Catmull-Rom 三次插值闭合曲线。",
  },
  "profile::spline": {
    symbol: "profile::spline",
    dimension_type: "(points: Length[2][], closed?: bool, segments_per_span?: u32, plane?: Plane) -> Sketch2D",
    summary: "快捷二维闭合样条截面原语：直接返回可传入 extrude / loft 的平滑样条 Sketch2D 截面。",
  },
  "path::helix": {
    symbol: "path::helix",
    dimension_type:
      "(radius: Length, pitch: Length, turns?: f64, origin?: Length[3], segments_per_turn?: u32) -> Length[3][]",
    summary: "三维圆柱螺旋路径原语：生成可传入 sweep(profile, path) 的空间螺旋扫掠中心线点列。",
  },
  "path::spline3d": {
    symbol: "path::spline3d",
    dimension_type:
      "(points: Length[3][], closed?: bool, segments_per_span?: u32) -> Length[3][]",
    summary: "三维 Catmull-Rom 空间样条路径原语：通过 3D 控制点序列插值生成平滑空间扫掠轨迹。",
  },
};

export function applyKclCompletion(source: string, line: number, col: number, insertion: string) {
  let lineStart = 0;
  for (let i = 1; i < line; i += 1) {
    const newline = source.indexOf("\n", lineStart);
    if (newline < 0) return { source, caret: source.length };
    lineStart = newline + 1;
  }
  const lineEnd = source.indexOf("\n", lineStart);
  const offset = Math.min(lineEnd < 0 ? source.length : lineEnd, lineStart + Math.max(0, col - 1));
  const prefix = /[A-Za-z_][\w:]*$/.exec(source.slice(lineStart, offset))?.[0] ?? "";
  const suffix = prefix || /[A-Za-z_]/.test(source[offset] ?? "")
    ? /^[\w:]*/.exec(source.slice(offset))?.[0] ?? "" : "";
  const start = offset - prefix.length;
  const end = offset + suffix.length;
  const text = source[end] === "(" ? insertion.split("(")[0] : insertion.replace(/\n/g, source.includes("\r\n") ? "\r\n" : "\n");
  return { source: source.slice(0, start) + text + source.slice(end), caret: start + text.length };
}

function extractWordAtPosition(
  source: string,
  line: number,
  col: number,
): string {
  const lines = source.split(/\r?\n/);
  const text = lines[Math.max(0, line - 1)] ?? "";
  const idx = Math.max(0, Math.min(text.length, col - 1));

  let start = idx;
  while (start > 0 && /[a-zA-Z0-9_:]/.test(text[start - 1])) {
    start -= 1;
  }
  let end = idx;
  while (end < text.length && /[a-zA-Z0-9_:]/.test(text[end])) {
    end += 1;
  }
  return text.slice(start, end);
}

function inferExprDimension(expr: string): string {
  if (/\d\s*(?:mm|m|in)\b/.test(expr)) {
    return "Length";
  }
  if (/\d\s*(?:deg|rad)\b/.test(expr)) {
    return "Angle";
  }
  if (/\d\s*(?:kN|N)\b/.test(expr)) {
    return "Force";
  }
  if (/\d\s*MPa\b/.test(expr)) {
    return "Pressure";
  }
  if (/^(?:box|cylinder|extrude|shell|sweep|loft|mirror|boolean::)/.test(expr.trim())) {
    return "Solid";
  }
  return "Inferred";
}

export function analyzeKclLspFallback(
  source: string,
  line = 1,
  col = 1,
): LspQueryResponse {
  const symbolMap = new Map<string, LspHoverInfo>();
  const variableCompletions: LspCompletionItem[] = [];
  const lines = source.split(/\r?\n/);

  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i];
    const lineNum = i + 1;

    const paramMatch = /^\s*([a-zA-Z_]\w*)\s*:\s*([A-Za-z_]\w*)\s*=\s*(.+)$/.exec(
      raw,
    );
    if (paramMatch) {
      const name = paramMatch[1];
      const ty = paramMatch[2];
      const rhs = paramMatch[3].replace(/\/\/.*$/, "").trim();
      symbolMap.set(name, {
        symbol: name,
        dimension_type: ty,
        summary: `参数声明：${name}: ${ty} = ${rhs}`,
        span: {
          start_line: lineNum,
          start_col: 1,
          end_line: lineNum,
          end_col: raw.length + 1,
        },
      });
      variableCompletions.push({
        label: name,
        kind: "variable",
        detail: `${ty} (line ${lineNum})`,
        insert_text: name,
      });
      continue;
    }

    const letMatch = /^\s*(?:let|const)\s+([a-zA-Z_]\w*)\s*=\s*(.+)$/.exec(raw);
    if (letMatch) {
      const name = letMatch[1];
      const rhs = letMatch[2].replace(/\/\/.*$/, "").trim();
      const dimType = inferExprDimension(rhs);
      symbolMap.set(name, {
        symbol: name,
        dimension_type: dimType,
        summary: `绑定变量：${name} (${dimType}) = ${rhs}`,
        span: {
          start_line: lineNum,
          start_col: 1,
          end_line: lineNum,
          end_col: raw.length + 1,
        },
      });
      variableCompletions.push({
        label: name,
        kind: "variable",
        detail: `${dimType} (line ${lineNum})`,
        insert_text: name,
      });
    }
  }

  const word = extractWordAtPosition(source, line, col);
  const hover =
    (word ? symbolMap.get(word) ?? BUILTIN_HOVER_DOCS[word] ?? null : null);

  const prefix = word.toLowerCase();
  const allCompletions = [...variableCompletions, ...KCL_BUILTIN_COMPLETIONS];
  const completions = prefix
    ? allCompletions.filter(
        (c) =>
          c.label.toLowerCase().includes(prefix) ||
          c.detail.toLowerCase().includes(prefix),
      )
    : allCompletions;

  return {
    backend: "builtin-analyzer",
    completions: completions.slice(0, 28),
    hover,
  };
}

export type QuickFeatureKind =
  | "through_hole"
  | "counterbore_hole"
  | "fillet_edges"
  | "chamfer_edges";

const UNPREFIXED_OCCT_ID = /^(occt_edge_|occt_face_|occt-edge-|occt-face-)/;

/**
 * Map a selected face/edge id onto a solid name.
 *
 * Multi-body meshes use `{solid}_occt_edge_*`. A single terminal solid keeps the
 * kernel's unprefixed `occt_edge_*` (see graph_eval when terminal_ids.len()==1).
 * Unprefixed ids may map only when there is exactly one returned solid.
 * They must not pick the first of several bodies, and a leftover
 * `hole_fillet_occt_*` id must not map onto a shorter `hole`.
 */
export function solidNameFromSelectionId(
  selectionId: string,
  candidates: readonly string[],
  returnedSolids: readonly string[] = candidates,
): string | null {
  const id = selectionId.trim();
  if (!id) return null;
  if (UNPREFIXED_OCCT_ID.test(id)) {
    const terminals = returnedSolids.length > 0 ? returnedSolids : [];
    const unique = [...new Set(terminals)];
    return unique.length === 1 ? unique[0] : null;
  }
  if (candidates.length === 0) return null;
  if (candidates.includes(id)) return id;
  let best: string | null = null;
  for (const name of candidates) {
    if (id === name || id.startsWith(`${name}_`) || id.startsWith(`${name}-`)) {
      if (!best || name.length > best.length) best = name;
    }
  }
  // Prefixed OCCT ids must use the ident immediately before `_occt_`.
  if (best && /(?:_occt_|-occt-)/.test(id)) {
    if (id !== best && !id.startsWith(`${best}_occt_`) && !id.startsWith(`${best}-occt-`)) {
      return null;
    }
  }
  return best;
}

function collectConsumedIrSolidIdents(source: string): Set<string> {
  const consumed = new Set<string>();
  const blockRe = /(?:^|[\n\r])\s*let\s+[A-Za-z_]\w*\s*=\s*[A-Za-z_]\w*\s*\{([^}]*)\}/g;
  let match: RegExpExecArray | null = blockRe.exec(source);
  while (match) {
    const body = match[1] ?? "";
    const singleRefRe = /\b(?:solid|target)\s*=\s*([A-Za-z_]\w*)/g;
    let refMatch: RegExpExecArray | null = singleRefRe.exec(body);
    while (refMatch) {
      consumed.add(refMatch[1]);
      refMatch = singleRefRe.exec(body);
    }
    const toolsMatch = /\btools\s*=\s*\[([^\]]*)\]/.exec(body);
    if (toolsMatch) {
      for (const part of toolsMatch[1].split(",")) {
        const ident = part.trim();
        if (/^[A-Za-z_]\w*$/.test(ident)) consumed.add(ident);
      }
    }
    match = blockRe.exec(source);
  }
  return consumed;
}

function collectReturnedSolidIdents(source: string): string[] {
  let idents: string[] = [];
  for (const line of source.split(/\r?\n/)) {
    const info = returnArrayIdents(line);
    if (info) {
      idents = info.idents;
      continue;
    }
    const bare = /^\s*return\s+([A-Za-z_]\w*)\s*(?:\/\/.*)?$/.exec(line);
    if (bare) idents = [bare[1]];
  }
  if (idents.length <= 1) return idents;
  // Mirror crates/cad-evaluator `terminal_finished_output_ids`: a single
  // terminal solid (e.g. `return [block, hole]` where `hole` consumes `block`)
  // keeps unprefixed `occt_edge_*` / `occt_face_*` ids in the scene mesh.
  const consumed = collectConsumedIrSolidIdents(source);
  const terminals = idents.filter((ident) => !consumed.has(ident));
  return terminals.length > 0 ? terminals : idents;
}

function mapIrSelectionToSolid(selectionId: string, source: string): string | null {
  return solidNameFromSelectionId(
    selectionId,
    collectSolidCandidates(source),
    collectReturnedSolidIdents(source),
  );
}

/** True when the buffer is a `.kcl 2.0` / `@ir_schema` feature document, not CAD-DSL `part`. */
export function isIrSchemaKcl(source: string): boolean {
  return /(?:^|[\n\r])\s*@ir_schema\s*\(/.test(source);
}

function letBoundIdents(source: string): string[] {
  const idents: string[] = [];
  const re = /(?:^|[\n\r])\s*let\s+([A-Za-z_]\w*)\s*=/g;
  let match: RegExpExecArray | null = re.exec(source);
  while (match) {
    idents.push(match[1]);
    match = re.exec(source);
  }
  return idents;
}

function kclLetIdentExists(source: string, ident: string): boolean {
  return new RegExp(String.raw`(?:^|[\n\r])\s*let\s+${ident}\s*=`).test(source);
}

function allocateKclIdent(source: string, base: string): string {
  if (!kclLetIdentExists(source, base)) return base;
  for (let i = 2; i < 1000; i += 1) {
    const candidate = `${base}_${i}`;
    if (!kclLetIdentExists(source, candidate)) return candidate;
  }
  return `${base}_${Date.now()}`;
}

function rewriteReturnArrayIdent(source: string, fromIdent: string, toIdent: string): string {
  if (fromIdent === toIdent) return source;
  const newline = source.includes("\r\n") ? "\r\n" : "\n";
  const lines = source.split(/\r?\n/);
  const next = lines.map((line) => {
    const parsed = returnArrayIdents(line);
    if (!parsed || !parsed.idents.includes(fromIdent)) return line;
    const match = /^(\s*return\s*\[)([^\]]+)(\].*)$/.exec(line);
    if (!match) return line;
    const rewritten = match[2]
      .split(",")
      .map((part) => {
        const ident = part.trim();
        if (ident !== fromIdent) return part;
        const leading = part.match(/^\s*/)?.[0] ?? "";
        const trailing = part.match(/\s*$/)?.[0] ?? "";
        return `${leading}${toIdent}${trailing}`;
      })
      .join(",");
    return `${match[1]}${rewritten}${match[3]}`;
  });
  return next.join(newline);
}

/** Rewrite both `return [..., fromIdent, ...]` and a single-ident `return fromIdent`. */
function rewriteReturnIdent(source: string, fromIdent: string, toIdent: string): string {
  if (fromIdent === toIdent) return source;
  const afterArray = rewriteReturnArrayIdent(source, fromIdent, toIdent);
  const newline = afterArray.includes("\r\n") ? "\r\n" : "\n";
  return afterArray
    .split(/\r?\n/)
    .map((line) => {
      const match = /^(\s*return\s+)([A-Za-z_]\w*)(\s*(?:\/\/.*)?)?$/.exec(line);
      if (!match || match[2] !== fromIdent) return line;
      // Feature-KCL requires `return [ident]`; a bare `return ident` is E002.
      return `${match[1]}[${toIdent}]${match[3] ?? ""}`;
    })
    .join(newline);
}

function formatKclScalar(value: number): string {
  if (!Number.isFinite(value)) return "0";
  if (Number.isInteger(value)) return String(value);
  return String(Math.round(value * 10_000) / 10_000);
}

/** Top-face center of a `let <ident> = box { ... }` (following upstream `solid = ...` links), or null. */
function boxTopCenterOrigin(source: string, ident: string): string | null {
  let current: string | null = ident;
  const visited = new Set<string>();
  for (let depth = 0; depth < 16 && current; depth += 1) {
    if (visited.has(current)) return null;
    visited.add(current);
    const boxBlock = new RegExp(
      String.raw`(?:^|[\n\r])\s*let\s+${current}\s*=\s*box\s*\{([^}]*)\}`,
    ).exec(source);
    if (boxBlock) {
      const body = boxBlock[1] ?? "";
      const mm = (field: string): number | null => {
        const match = new RegExp(String.raw`\b${field}\s*=\s*(-?\d+(?:\.\d+)?)mm`).exec(body);
        return match ? Number(match[1]) : null;
      };
      const width = mm("width");
      const depthVal = mm("depth");
      const height = mm("height");
      if (width === null || depthVal === null || height === null) return null;
      let originX = 0;
      let originY = 0;
      let originZ = 0;
      const origin = /\borigin\s*=\s*vec3\(\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*\)/.exec(
        body,
      );
      if (origin) {
        originX = Number(origin[1]);
        originY = Number(origin[2]);
        originZ = Number(origin[3]);
      }
      return `vec3(${formatKclScalar(originX + width / 2)}, ${formatKclScalar(originY + depthVal / 2)}, ${formatKclScalar(originZ + height)})`;
    }
    const featureBlock: RegExpExecArray | null = new RegExp(
      String.raw`(?:^|[\n\r])\s*let\s+${current}\s*=\s*[A-Za-z_]\w*\s*\{([^}]*)\}`,
    ).exec(source);
    const parentSolid: string | null = featureBlock?.[1]
      ? /\b(?:solid|target)\s*=\s*([A-Za-z_]\w*)/.exec(featureBlock[1])?.[1] ?? null
      : null;
    current = parentSolid;
  }
  return null;
}

function updateIrHoleBinding(
  source: string,
  bindingPrefix: string,
  targetVar: string,
  rewriteBody: (body: string) => string | null,
): string | null {
  const re = new RegExp(
    String.raw`let\s+(${bindingPrefix}(?:_\d+)?)\s*=\s*hole\s*\{([^}]*)\}(\s*->\s*Solid)?`,
    "g",
  );
  const matches = Array.from(source.matchAll(re)).filter((entry) => {
    const binding = entry[1] ?? "";
    const body = entry[2] ?? "";
    const solid = /\bsolid\s*=\s*([A-Za-z_]\w*)/.exec(body)?.[1];
    return binding === targetVar || solid === targetVar;
  });
  const match =
    matches.find((entry) => entry[1] === targetVar) ??
    matches[0];
  if (!match || match.index === undefined) return null;
  const nextBody = rewriteBody(match[2] ?? "");
  if (nextBody === null) return null;
  const tail = match[3] ?? "";
  return (
    source.slice(0, match.index) +
    `let ${match[1]} = hole {${nextBody}}${tail}` +
    source.slice(match.index + match[0].length)
  );
}

function replaceIrHoleDiameter(body: string, diameterText: string): string | null {
  if (!/\bdiameter\s*=\s*\d+(?:\.\d+)?mm/.test(body)) return null;
  return body.replace(/\bdiameter\s*=\s*\d+(?:\.\d+)?mm/, `diameter = ${diameterText}`);
}

function replaceIrCounterboreProfile(
  body: string,
  diameterText: string,
  depthText: string,
): string | null {
  const profile = /profile\s*=\s*counterbore\(\s*diameter\s*=\s*\d+(?:\.\d+)?mm\s*,\s*depth\s*=\s*\d+(?:\.\d+)?mm\s*\)/;
  if (!profile.test(body)) return null;
  return body.replace(
    profile,
    `profile = counterbore(diameter = ${diameterText}, depth = ${depthText})`,
  );
}

function irHoleBlockLines(
  indent: string,
  binding: string,
  solidName: string,
  source: string,
  extraFields: readonly string[],
): string[] {
  const origin = boxTopCenterOrigin(source, solidName);
  const fields = [
    `${indent}    solid = ${solidName},`,
    ...(origin ? [`${indent}    origin = ${origin},`] : []),
    ...extraFields.map((field) => `${indent}    ${field}`),
  ];
  return [`${indent}let ${binding} = hole {`, ...fields, `${indent}} -> Solid`];
}

function updateIrDressupField(
  source: string,
  operator: "fillet" | "chamfer",
  solidName: string,
  field: "radius" | "distance",
  valueText: string,
): string | null {
  const re = new RegExp(
    String.raw`let\s+([A-Za-z_]\w*)\s*=\s*${operator}\s*\{([^}]*)\}(\s*->\s*Solid)?`,
    "g",
  );
  const matches = Array.from(source.matchAll(re)).filter((entry) => {
    const binding = entry[1] ?? "";
    const body = entry[2] ?? "";
    const solid = /\bsolid\s*=\s*([A-Za-z_]\w*)/.exec(body)?.[1];
    const hasField = new RegExp(String.raw`\b${field}\s*=\s*\d+(?:\.\d+)?mm`).test(body);
    // Prefer matching the selected node's own ident (hole_fillet_occt_edge_* → hole_fillet)
    // so a second apply updates radius instead of nesting hole_fillet_fillet.
    return hasField && (binding === solidName || solid === solidName);
  });
  const match =
    matches.find((entry) => entry[1] === solidName) ??
    matches.find((entry) => {
      const body = entry[2] ?? "";
      const solid = /\bsolid\s*=\s*([A-Za-z_]\w*)/.exec(body)?.[1];
      return solid === solidName;
    });
  if (!match || match.index === undefined) return null;
  const nextBody = (match[2] ?? "").replace(
    new RegExp(String.raw`(\b${field}\s*=\s*)(\d+(?:\.\d+)?)mm`),
    `$1${valueText}`,
  );
  const tail = match[3] ?? "";
  return (
    source.slice(0, match.index) +
    `let ${match[1]} = ${operator} {${nextBody}}${tail}` +
    source.slice(match.index + match[0].length)
  );
}

function stripIllegalPipeDressup(source: string, targetVar: string): string {
  const re = new RegExp(
    String.raw`(^|[\n\r])([ \t]*let\s+${targetVar}\s*=\s*${targetVar}\s*\|>\s*(?:fillet|chamfer)\([^)]*\)[ \t]*)(?=\r?\n|$)`,
    "g",
  );
  return source.replace(re, "$1").replace(/\n{3,}/g, "\n\n");
}

function collectSolidCandidates(source: string): string[] {
  const candidates: string[] = [];
  for (const ident of letBoundIdents(source)) {
    if (!candidates.includes(ident)) candidates.push(ident);
  }
  for (const line of source.split(/\r?\n/)) {
    const info = returnArrayIdents(line);
    if (!info) continue;
    for (const ident of info.idents) {
      if (!candidates.includes(ident)) candidates.push(ident);
    }
  }
  return candidates;
}

function returnArrayIdents(line: string): { indent: string; idents: string[] } | null {
  const m = /^(\s*)return\s*\[([^\]]+)\]\s*(?:\/\/.*)?$/.exec(line);
  if (!m) return null;
  const idents = m[2]
    .split(",")
    .map((part) => part.trim())
    .filter((part) => /^[A-Za-z_]\w*$/.test(part));
  if (idents.length === 0) return null;
  return { indent: m[1] || "    ", idents };
}

function insertQuickFeatureBeforeReturn(
  kclSource: string,
  buildStatements: (indent: string, targetVar: string) => readonly string[],
  selectionId?: string,
): string {
  const newline = kclSource.includes("\r\n") ? "\r\n" : "\n";
  const lines = kclSource.split(/\r?\n/);

  // Prefer the last `return <ident>` inside a `part` block.
  let targetLineIdx = -1;
  let targetIndent = "    ";
  let targetVar = "";

  for (let i = 0; i < lines.length; i += 1) {
    const m = /^(\s*)return\s+([A-Za-z_]\w*)\s*(?:\/\/.*)?$/.exec(lines[i]);
    if (m) {
      targetLineIdx = i;
      targetIndent = m[1] || "    ";
      targetVar = m[2];
    }
  }

  if (targetLineIdx >= 0 && targetVar) {
    const inserted = buildStatements(targetIndent, targetVar);
    const updatedLines = [
      ...lines.slice(0, targetLineIdx),
      ...inserted,
      ...lines.slice(targetLineIdx),
    ];
    return updatedLines.join(newline);
  }

  // Multi-body `@ir_schema` documents return `[hole, block2, ...]`.
  // Previously this path was skipped, so GUI 圆角 on the C6 fixture was a silent no-op.
  let arrayLineIdx = -1;
  let arrayInfo: { indent: string; idents: string[] } | null = null;
  for (let i = 0; i < lines.length; i += 1) {
    const info = returnArrayIdents(lines[i]);
    if (info) {
      arrayLineIdx = i;
      arrayInfo = info;
    }
  }
  if (arrayLineIdx >= 0 && arrayInfo) {
    const target = mapIrSelectionToSolid(selectionId ?? "", kclSource);
    if (!target) return kclSource;
    const inserted = buildStatements(arrayInfo.indent, target);
    const updatedLines = [
      ...lines.slice(0, arrayLineIdx),
      ...inserted,
      ...lines.slice(arrayLineIdx),
    ];
    return updatedLines.join(newline);
  }

  // Fallback: `return <expr>` where `<expr>` is not a simple identifier (and not an assembly array `[`).
  const exprReturnMatch = /^(\s*)return\s+(?!\[)([\s\S]+?)(?=\r?\n\s*\})/m.exec(kclSource);
  if (exprReturnMatch && exprReturnMatch.index !== undefined) {
    const indent = exprReturnMatch[1] || "    ";
    const exprText = exprReturnMatch[2].trim();
    const fallbackVar = "final_part";
    const inserted = buildStatements(indent, fallbackVar);
    const replacement = [
      `${indent}let ${fallbackVar} = ${exprText}`,
      ...inserted,
      `${indent}return ${fallbackVar}`,
    ].join(newline);
    return (
      kclSource.slice(0, exprReturnMatch.index) +
      replacement +
      kclSource.slice(exprReturnMatch.index + exprReturnMatch[0].length)
    );
  }

  return kclSource;
}

export function applyFaceQuickFeatureToKcl(
  kclSource: string,
  faceId: string,
  featureKind: QuickFeatureKind,
  paramValueMm?: number,
): { nextSource: string; summary: string } {
  const faceLabel = faceId && faceId.trim() ? faceId.trim() : "当前实体";

  if (featureKind === "fillet_edges") {
    const rVal =
      typeof paramValueMm === "number" && Number.isFinite(paramValueMm) && paramValueMm > 0
        ? paramValueMm
        : 2.0;
    const rText = `${rVal.toFixed(1)}mm`;
    if (isIrSchemaKcl(kclSource)) {
      const targetVar = mapIrSelectionToSolid(faceId, kclSource) ?? "";
      if (!targetVar) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      const stripped = stripIllegalPipeDressup(kclSource, targetVar);
      const updated = updateIrDressupField(stripped, "fillet", targetVar, "radius", rText);
      if (updated) {
        return {
          nextSource: updated,
          summary: `已在 ${faceLabel} 更新圆角特征 fillet(radius = ${rText})`,
        };
      }
      let freshName = "";
      const inserted = insertQuickFeatureBeforeReturn(
        stripped,
        (indent, solidName) => {
          freshName = allocateKclIdent(stripped, `${solidName}_fillet`);
          return [
            `${indent}let ${freshName} = fillet {`,
            `${indent}    solid = ${solidName},`,
            `${indent}    radius = ${rText},`,
            `${indent}} -> Solid`,
          ];
        },
        faceId,
      );
      if (inserted === stripped || !freshName) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      return {
        nextSource: rewriteReturnIdent(inserted, targetVar, freshName),
        summary: `已在 ${faceLabel} 添加圆角特征 fillet(radius = ${rText})`,
      };
    }
    const existingFilletRe = /(\|>\s*fillet\(\s*(?:r\s*=\s*)?)(\d+(?:\.\d+)?)mm(\s*\))/;
    if (existingFilletRe.test(kclSource)) {
      const nextSource = kclSource.replace(existingFilletRe, `$1${rText}$3`);
      return {
        nextSource,
        summary: `已在 ${faceLabel} 更新圆角特征 fillet(r = ${rText})`,
      };
    }
    const nextSource = insertQuickFeatureBeforeReturn(
      kclSource,
      (indent, targetVar) => [
        `${indent}let ${targetVar} = ${targetVar} |> fillet(r = ${rText})`,
      ],
      faceId,
    );
    return {
      nextSource,
      summary: `已在 ${faceLabel} 添加圆角特征 fillet(r = ${rText})`,
    };
  }

  if (featureKind === "chamfer_edges") {
    const dVal =
      typeof paramValueMm === "number" && Number.isFinite(paramValueMm) && paramValueMm > 0
        ? paramValueMm
        : 1.5;
    const dText = `${dVal.toFixed(1)}mm`;
    if (isIrSchemaKcl(kclSource)) {
      const targetVar = mapIrSelectionToSolid(faceId, kclSource) ?? "";
      if (!targetVar) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      const stripped = stripIllegalPipeDressup(kclSource, targetVar);
      const updated = updateIrDressupField(stripped, "chamfer", targetVar, "distance", dText);
      if (updated) {
        return {
          nextSource: updated,
          summary: `已在 ${faceLabel} 更新倒角特征 chamfer(distance = ${dText})`,
        };
      }
      let freshName = "";
      const inserted = insertQuickFeatureBeforeReturn(
        stripped,
        (indent, solidName) => {
          freshName = allocateKclIdent(stripped, `${solidName}_chamfer`);
          return [
            `${indent}let ${freshName} = chamfer {`,
            `${indent}    solid = ${solidName},`,
            `${indent}    distance = ${dText},`,
            `${indent}} -> Solid`,
          ];
        },
        faceId,
      );
      if (inserted === stripped || !freshName) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      return {
        nextSource: rewriteReturnIdent(inserted, targetVar, freshName),
        summary: `已在 ${faceLabel} 添加倒角特征 chamfer(distance = ${dText})`,
      };
    }
    const existingChamferRe = /(\|>\s*chamfer\(\s*(?:d\s*=\s*)?)(\d+(?:\.\d+)?)mm(\s*\))/;
    if (existingChamferRe.test(kclSource)) {
      const nextSource = kclSource.replace(existingChamferRe, `$1${dText}$3`);
      return {
        nextSource,
        summary: `已在 ${faceLabel} 更新倒角特征 chamfer(d = ${dText})`,
      };
    }
    const nextSource = insertQuickFeatureBeforeReturn(
      kclSource,
      (indent, targetVar) => [
        `${indent}let ${targetVar} = ${targetVar} |> chamfer(d = ${dText})`,
      ],
      faceId,
    );
    return {
      nextSource,
      summary: `已在 ${faceLabel} 添加倒角特征 chamfer(d = ${dText})`,
    };
  }

  if (featureKind === "through_hole") {
    const dVal =
      typeof paramValueMm === "number" && Number.isFinite(paramValueMm) && paramValueMm > 0
        ? paramValueMm
        : 6.6;
    const dText = `${dVal.toFixed(1)}mm`;
    if (isIrSchemaKcl(kclSource)) {
      const targetVar = mapIrSelectionToSolid(faceId, kclSource) ?? "";
      if (!targetVar) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      const updated = updateIrHoleBinding(kclSource, "quick_through_hole", targetVar, (body) =>
        replaceIrHoleDiameter(body, dText),
      );
      if (updated) {
        return {
          nextSource: updated,
          summary: `已在 ${faceLabel} 更新定位通孔 hole(diameter = ${dText})`,
        };
      }
      let freshName = "";
      const inserted = insertQuickFeatureBeforeReturn(
        kclSource,
        (indent, solidName) => {
          freshName = allocateKclIdent(kclSource, "quick_through_hole");
          return irHoleBlockLines(indent, freshName, solidName, kclSource, [`diameter = ${dText},`]);
        },
        faceId,
      );
      if (inserted === kclSource || !freshName) {
        return { nextSource: kclSource, summary: `无法应用该特征` };
      }
      return {
        nextSource: rewriteReturnIdent(inserted, targetVar, freshName),
        summary: `已在 ${faceLabel} 添加定位通孔 hole(diameter = ${dText})`,
      };
    }
    const existingHoleRe =
      /(let\s+quick_through_hole\s*=\s*hole::simple\(\s*d\s*=\s*)(\d+(?:\.\d+)?)mm/;
    if (existingHoleRe.test(kclSource)) {
      const nextSource = kclSource.replace(existingHoleRe, `$1${dText}`);
      return {
        nextSource,
        summary: `已在 ${faceLabel} 更新定位通孔 hole::simple(d = ${dText}, depth = 15.0mm)`,
      };
    }
    const nextSource = insertQuickFeatureBeforeReturn(
      kclSource,
      (indent, targetVar) => [
        `${indent}let quick_through_hole = hole::simple(d = ${dText}, depth = 15.0mm)`,
        `${indent}let ${targetVar} = ${targetVar} |> boolean::subtract(tools = [quick_through_hole], preserve_exact_interfaces = true)`,
      ],
      faceId,
    );
    return {
      nextSource,
      summary: `已在 ${faceLabel} 添加定位通孔 hole::simple(d = ${dText}, depth = 15.0mm)`,
    };
  }

  // counterbore_hole
  const dThrough =
    typeof paramValueMm === "number" && Number.isFinite(paramValueMm) && paramValueMm > 0
      ? paramValueMm
      : 6.6;
  const dCbore =
    Math.abs(dThrough - 6.6) < 0.05
      ? 11.0
      : Math.max(dThrough + 2.4, Number((dThrough * 1.65).toFixed(1)));
  const depthCbore =
    Math.abs(dThrough - 6.6) < 0.05 ? 5.5 : Number((dThrough * 0.85).toFixed(1));
  const dThroughText = `${dThrough.toFixed(1)}mm`;
  const dCboreText = `${dCbore.toFixed(1)}mm`;
  const depthCboreText = `${depthCbore.toFixed(1)}mm`;
  if (isIrSchemaKcl(kclSource)) {
    const targetVar = mapIrSelectionToSolid(faceId, kclSource) ?? "";
    if (!targetVar) {
      return { nextSource: kclSource, summary: `无法应用该特征` };
    }
    const updated = updateIrHoleBinding(kclSource, "quick_cbore_hole", targetVar, (body) => {
      const withDiameter = replaceIrHoleDiameter(body, dThroughText);
      if (withDiameter === null) return null;
      return replaceIrCounterboreProfile(withDiameter, dCboreText, depthCboreText);
    });
    if (updated) {
      return {
        nextSource: updated,
        summary: `已在 ${faceLabel} 更新沉头孔 hole(diameter = ${dThroughText}, profile = counterbore(diameter = ${dCboreText}, depth = ${depthCboreText}))`,
      };
    }
    let freshName = "";
    const inserted = insertQuickFeatureBeforeReturn(
      kclSource,
      (indent, solidName) => {
        freshName = allocateKclIdent(kclSource, "quick_cbore_hole");
        return irHoleBlockLines(indent, freshName, solidName, kclSource, [
          `diameter = ${dThroughText},`,
          `profile = counterbore(diameter = ${dCboreText}, depth = ${depthCboreText}),`,
        ]);
      },
      faceId,
    );
    if (inserted === kclSource || !freshName) {
      return { nextSource: kclSource, summary: `无法应用该特征` };
    }
    return {
      nextSource: rewriteReturnIdent(inserted, targetVar, freshName),
      summary: `已在 ${faceLabel} 添加沉头孔 hole(diameter = ${dThroughText}, profile = counterbore(diameter = ${dCboreText}, depth = ${depthCboreText}))`,
    };
  }
  const existingCboreRe =
    /let\s+quick_cbore_hole\s*=\s*hole::counterbore\([^)]*\)/;
  if (existingCboreRe.test(kclSource)) {
    const nextSource = kclSource.replace(
      existingCboreRe,
      `let quick_cbore_hole = hole::counterbore(d_through = ${dThroughText}, d_cbore = ${dCboreText}, depth_cbore = ${depthCboreText})`,
    );
    return {
      nextSource,
      summary: `已在 ${faceLabel} 更新沉头孔 hole::counterbore(d_through = ${dThroughText}, d_cbore = ${dCboreText}, depth_cbore = ${depthCboreText})`,
    };
  }
  const nextSource = insertQuickFeatureBeforeReturn(
    kclSource,
    (indent, targetVar) => [
      `${indent}let quick_cbore_hole = hole::counterbore(d_through = ${dThroughText}, d_cbore = ${dCboreText}, depth_cbore = ${depthCboreText})`,
      `${indent}let ${targetVar} = ${targetVar} |> boolean::subtract(tools = [quick_cbore_hole], preserve_exact_interfaces = true)`,
    ],
    faceId,
  );
  return {
    nextSource,
    summary: `已在 ${faceLabel} 添加沉头孔 hole::counterbore(d_through = ${dThroughText}, d_cbore = ${dCboreText}, depth_cbore = ${depthCboreText})`,
  };
}

function extractLengthMm(prompt: string, pattern: RegExp, fallback: number): number {
  const m = pattern.exec(prompt);
  if (!m) {
    return fallback;
  }
  const val = Number.parseFloat(m[1]);
  return Number.isFinite(val) && val > 1 ? val : fallback;
}

export function generateFallbackKclFromPrompt(
  prompt: string,
  material = "Al-6061-T6",
  manufacturingMethod = "CNC_3AXIS_MILLING",
): string {
  const lower = prompt.toLowerCase();

  if (/椭圆|ellipse|elliptic|oval/.test(lower)) {
    const rx = extractLengthMm(
      prompt,
      /(?:长半轴|长轴半径|rx|major)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      40.0,
    );
    const ry = extractLengthMm(
      prompt,
      /(?:短半轴|短轴半径|ry|minor)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      24.0,
    );
    const height = extractLengthMm(
      prompt,
      /(?:厚|高|拉伸|thickness|height|extrude)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      10.0,
    );
    const defaultBore = Math.max(6.0, Math.min(16.0, Math.min(rx, ry) * 0.6));
    const rawBore = extractLengthMm(
      prompt,
      /(?:内孔|孔径|bore)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      defaultBore,
    );
    const bore = Math.min(rawBore, Math.max(4.0, Math.min(rx, ry) * 1.2));

    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part EllipticalFlangePlate(
    major_rx: Length = ${rx.toFixed(1)}mm,
    minor_ry: Length = ${ry.toFixed(1)}mm,
    plate_h: Length = ${height.toFixed(1)}mm,
    bore_d: Length = ${bore.toFixed(1)}mm @pmi::tolerance("H7")
) -> Solid {
    let base = profile::ellipse(rx = major_rx, ry = minor_ry, center = [0mm, 0mm], plane = XY_PLANE)
        |> extrude(h = plate_h)
    let bore_tool = cylinder(d = bore_d, h = plate_h + 4.0mm, origin = [0mm, 0mm, -2.0mm], exact_brep = true)
        |> tag::cylindrical_faces("bearing_seat_wall")
    let final_part = base
        |> boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)
    return final_part
}
`;
  }

  if (/螺旋|盘管|弹簧|helix|helical|spiral/.test(lower)) {
    const coilRadius = extractLengthMm(
      prompt,
      /(?:螺旋半径|中径半径|半径|radius)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      20.0,
    );
    const pitch = extractLengthMm(
      prompt,
      /(?:螺距|节距|pitch)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      14.0,
    );
    const rawTubeR = extractLengthMm(
      prompt,
      /(?:截面半径|管径|线径|tube)[^\d]{0,8}(\d+(?:\.\d+)?)/i,
      3.0,
    );
    const tubeRadius = Math.max(1.5, Math.min(rawTubeR, pitch * 0.35, coilRadius * 0.4));
    const turns = 1.5;
    const steps = 18;
    const pts: string[] = [];
    for (let i = 0; i <= steps; i += 1) {
      const t = i / steps;
      const theta = t * turns * Math.PI * 2;
      const x = (coilRadius * Math.cos(theta)).toFixed(2);
      const y = (coilRadius * Math.sin(theta)).toFixed(2);
      const z = (t * turns * pitch).toFixed(2);
      pts.push(`        [${x}mm, ${y}mm, ${z}mm]`);
    }

    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

// Spatial helical sweep path: path::helix(radius = ${coilRadius.toFixed(1)}mm, pitch = ${pitch.toFixed(1)}mm, turns = ${turns.toFixed(1)})
part HelicalCoilPipe(
    coil_radius: Length = ${coilRadius.toFixed(1)}mm,
    coil_pitch: Length = ${pitch.toFixed(1)}mm,
    tube_radius: Length = ${tubeRadius.toFixed(1)}mm
) -> Solid {
    let section = profile::ellipse(rx = tube_radius, ry = tube_radius, center = [0mm, 0mm], plane = XY_PLANE)
    let helix_path = [
${pts.join(",\n")}
    ]
    let final_part = section
        |> sweep(path = helix_path)
    return final_part
}
`;
  }

  if (
    /装配|assembly|组件|端盖.*底座|轴承座.*盖|mate/.test(lower)
  ) {
    const boreD = extractLengthMm(prompt, /(?:内孔|孔径|bore)[^\d]{0,8}(\d+(?:\.\d+)?)/i, 30.0);
    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part HousingBase(
    bore_d: Length = ${boreD.toFixed(1)}mm @pmi::tolerance("H7"),
    flange_od: Length = 88.0mm,
    base_h: Length = 10.0mm,
    hub_h: Length = 34.0mm
) -> Solid {
    let flange = cylinder(d = flange_od, h = base_h, origin = [0mm, 0mm, 0mm])
        |> tag::faces("housing_base")
    let hub = cylinder(d = bore_d + 20.0mm, h = hub_h, origin = [0mm, 0mm, 0mm])
    let bore_tool = cylinder(d = bore_d, h = hub_h + 4.0mm, origin = [0mm, 0mm, -2.0mm], exact_brep = true)
        |> tag::cylindrical_faces("bearing_seat_wall")
    let housing = boolean::union(items = [flange, hub])
        |> boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)
    return housing
}

part EndCap(
    cap_od: Length = 76.0mm,
    cap_h: Length = 8.0mm,
    pilot_d: Length = ${boreD.toFixed(1)}mm
) -> Solid {
    let plate = cylinder(d = cap_od, h = cap_h, origin = [0mm, 0mm, 0mm])
    let center_hole = cylinder(d = pilot_d - 6.0mm, h = cap_h + 2.0mm, origin = [0mm, 0mm, -1.0mm], exact_brep = true)
    let cap = boolean::subtract(tools = [center_hole], preserve_exact_interfaces = true)
    return boolean::union(items = [plate]) |> boolean::subtract(tools = [center_hole], preserve_exact_interfaces = true)
}

assembly BearingHousingAssembly() -> Assembly {
    let c_housing = component(part = HousingBase(), origin = [0mm, 0mm, 0mm], rotation = [0deg, 0deg, 0deg])
    let c_cap = component(part = EndCap(), origin = [0mm, 0mm, 34.0mm], rotation = [0deg, 0deg, 0deg])
    mate::coaxial(a = c_housing, b = c_cap, axis = Z_AXIS)
    mate::coincident(a = c_housing, b = c_cap, offset = 0mm)
    return [c_housing, c_cap]
}
`;
  }

  if (/抽壳|盒|shell|控制盒|enclosure|box|螺纹/.test(lower)) {
    const length = extractLengthMm(prompt, /(?:长|length)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 96.0);
    const width = extractLengthMm(prompt, /(?:宽|width)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 68.0);
    const height = extractLengthMm(prompt, /(?:高|height)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 36.0);
    const wall = extractLengthMm(prompt, /(?:壁厚|wall|shell)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 2.5);
    const threadMatch = /(M\d+(?:x[0-9.]+)?)/i.exec(prompt);
    const threadSpec = threadMatch ? `${threadMatch[1].toUpperCase()}-6H` : "M4x0.7-6H";

    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part ShelledControlBox(
    box_len: Length = ${length.toFixed(1)}mm,
    box_width: Length = ${width.toFixed(1)}mm,
    box_height: Length = ${height.toFixed(1)}mm,
    wall_t: Length = ${wall.toFixed(1)}mm
) -> Solid {
    let body = box(size = [box_len, box_width, box_height], origin = [-box_len / 2.0, -box_width / 2.0, 0mm])
        |> shell(thickness = wall_t, open_faces = [])
    let ear = box(size = [14.0mm, 24.0mm, 6.0mm], origin = [box_len / 2.0, -12.0mm, 0mm])
        |> mirror(plane = YZ_PLANE)
    let tapped = hole::tapped(spec = "${threadSpec}", depth = 8.0mm)
    let final_part = boolean::union(items = [body, ear])
        |> boolean::subtract(tools = [tapped], preserve_exact_interfaces = true)
    assert::dfm_accessible(target = final_part, axes = [+Z, -Z], min_wall_thickness = 2.0mm)
    return final_part
}
`;
  }

  if (/扫掠|放样|管|流道|歧管|sweep|loft|manifold|pipe/.test(lower)) {
    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part SweptManifoldPipe(
    pipe_od: Length = 26.0mm,
    flange_d: Length = 54.0mm,
    rise_h: Length = 42.0mm
) -> Solid {
    let base_flange = cylinder(d = flange_d, h = 8.0mm, origin = [0mm, 0mm, 0mm])
    let riser = cylinder(d = pipe_od, h = rise_h, origin = [0mm, 0mm, 8.0mm])
    let arm = box(size = [46.0mm, pipe_od, pipe_od], origin = [0mm, -pipe_od / 2.0, rise_h - pipe_od / 2.0])
    let bore = cylinder(d = pipe_od - 8.0mm, h = rise_h + 12.0mm, origin = [0mm, 0mm, -2.0mm], exact_brep = true)
        |> tag::cylindrical_faces("fluid_channel_wall")
    let final_part = boolean::union(items = [base_flange, riser, arm])
        |> boolean::subtract(tools = [bore], preserve_exact_interfaces = true)
        |> fillet(r = 1.5mm)
    return final_part
}
`;
  }

  if (/法兰|flange|轴承|bearing|圆盘/.test(lower)) {
    const od = extractLengthMm(prompt, /(?:外径|od|直径)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 100.0);
    const bore = extractLengthMm(prompt, /(?:内孔|孔径|bore)[^\d]{0,6}(\d+(?:\.\d+)?)/i, 36.0);
    return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part PrecisionFlangeMount(
    flange_od: Length = ${od.toFixed(1)}mm,
    bore_d: Length = ${bore.toFixed(1)}mm @pmi::tolerance("H7"),
    flange_h: Length = 10.0mm,
    hub_h: Length = 36.0mm
) -> Solid {
    let disk = cylinder(d = flange_od, h = flange_h, origin = [0mm, 0mm, 0mm])
    let hub = cylinder(d = bore_d + 18.0mm, h = hub_h, origin = [0mm, 0mm, 0mm])
    let bore_tool = cylinder(d = bore_d, h = hub_h + 2.0mm, origin = [0mm, 0mm, -1.0mm], exact_brep = true)
        |> tag::cylindrical_faces("bearing_seat_wall")
    let bolts = hole::counterbore(d_through = 6.6mm, d_cbore = 11.0mm, depth_cbore = 5.5mm)
        |> array::polar(axis = Z_AXIS, radius = (flange_od / 2.0) - 10.0mm, count = 4, start_angle = 45.0deg)
    let final_part = boolean::union(items = [disk, hub])
        |> boolean::subtract(tools = [bore_tool, bolts], preserve_exact_interfaces = true)
    return final_part
}
`;
  }

  // Default: L-shaped sensor mounting bracket
  return `@unit(length = "mm", angle = "deg", force = "N")
@material("${material}")
@manufacturing(method = "${manufacturingMethod}", min_tool_radius = 1.5mm)

part SensorMountingBracket(
    base_len: Length = 80.0mm,
    base_w: Length = 46.0mm,
    wall_t: Length = 8.0mm,
    upright_h: Length = 48.0mm
) -> Solid {
    let base_plate = box(size = [base_len, base_w, wall_t], origin = [0mm, 0mm, 0mm])
    let vertical_wall = box(size = [wall_t, base_w, upright_h], origin = [0mm, 0mm, 0mm])
    let sensor_hole = cylinder(d = 12.0mm, h = wall_t + 4.0mm, origin = [base_len - 20.0mm, base_w / 2.0, -2.0mm], exact_brep = true)
        |> tag::cylindrical_faces("sensor_bore_wall")
    let final_part = boolean::union(items = [base_plate, vertical_wall])
        |> boolean::subtract(tools = [sensor_hole], preserve_exact_interfaces = true)
        |> chamfer(d = 1.2mm)
    return final_part
}
`;
}

export function handleRange(handle: EditHandle): readonly [number, number] {
  const value = handle.current_value;
  return [
    handle.min_value ?? (value > 0 && handle.unit === "mm" ? value * 0.25 : value - Math.max(10, Math.abs(value))),
    handle.max_value ?? value + Math.max(10, Math.abs(value) * 1.5),
  ];
}

export function clipToSection(
  points: readonly Vec3[],
  axis: 0 | 1 | 2,
  limit: number,
  closed = true,
  flip = false,
): readonly Vec3[] {
  const clipped: Vec3[] = [];
  const sign = flip ? -1 : 1;
  let previous = (closed ? points.at(-1) : points[0])!;
  for (const next of points) {
    const a = (previous[axis] - limit) * sign;
    const b = (next[axis] - limit) * sign;
    if ((a < 0 && b > 0) || (a > 0 && b < 0)) {
      const t = a / (a - b);
      clipped.push([
        previous[0] + t * (next[0] - previous[0]),
        previous[1] + t * (next[1] - previous[1]),
        previous[2] + t * (next[2] - previous[2]),
      ]);
    }
    if (b <= 0) clipped.push(next);
    previous = next;
  }
  return clipped;
}

export function normalizePolyline(polyline: TopoEdge["polyline"]): readonly Vec3[] {
  if (polyline.length === 0) {
    return [];
  }
  if (Array.isArray(polyline[0])) {
    return polyline as readonly Vec3[];
  }
  const flat = polyline as readonly number[];
  const pts: Vec3[] = [];
  for (let i = 0; i + 2 < flat.length; i += 3) {
    pts.push([flat[i], flat[i + 1], flat[i + 2]]);
  }
  return pts;
}

export function generateFallbackDxfFromMesh(sceneMesh: SceneMeshPayload): string {
  const lines: string[] = [
    "0",
    "SECTION",
    "2",
    "HEADER",
    "9",
    "$INSUNITS",
    "70",
    "4",
    "0",
    "ENDSEC",
    "0",
    "SECTION",
    "2",
    "TABLES",
    "0",
    "TABLE",
    "2",
    "LAYER",
    "0",
    "LAYER",
    "2",
    "OUTLINE",
    "70",
    "0",
    "62",
    "7",
    "0",
    "LAYER",
    "2",
    "HOLES",
    "70",
    "0",
    "62",
    "5",
    "0",
    "ENDTAB",
    "0",
    "ENDSEC",
    "0",
    "SECTION",
    "2",
    "ENTITIES",
  ];

  for (const edge of sceneMesh.edges) {
    const pts = normalizePolyline(edge.polyline);
    const layer = edge.provenance_tags.some((t) => /hole|bore/i.test(t))
      ? "HOLES"
      : "OUTLINE";
    for (let i = 0; i + 1 < pts.length; i += 1) {
      const a = pts[i];
      const b = pts[i + 1];
      lines.push(
        "0",
        "LINE",
        "8",
        layer,
        "10",
        a[0].toFixed(4),
        "20",
        a[1].toFixed(4),
        "30",
        a[2].toFixed(4),
        "11",
        b[0].toFixed(4),
        "21",
        b[1].toFixed(4),
        "31",
        b[2].toFixed(4),
      );
    }
  }

  lines.push("0", "ENDSEC", "0", "EOF");
  return `${lines.join("\n")}\n`;
}

export function generateFallbackDrawingSvg(
  sceneMesh: SceneMeshPayload,
  metrics: GeometryMetrics,
  views: Readonly<Record<string, string | undefined>>,
): string {
  const min = metrics.bbox_mm.min;
  const max = metrics.bbox_mm.max;
  const sx = (max[0] - min[0]).toFixed(1);
  const sy = (max[1] - min[1]).toFixed(1);
  const sz = (max[2] - min[2]).toFixed(1);

  const polylines = sceneMesh.edges.map((edge) => normalizePolyline(edge.polyline));
  const embedView = (
    svgText: string | undefined,
    x: number,
    y: number,
    title: string,
    project: (point: Vec3) => readonly [number, number],
  ): string => {
    const root = /<svg\b([^>]*)>([\s\S]*)<\/svg>/i.exec(svgText ?? "");
    const viewBox = root?.[1].match(/\bviewBox=["']([\d.eE+\- ,]+)["']/i)?.[1];
    let inner = `<text x="208" y="120" fill="#64748b" font-size="13" text-anchor="middle">暂无视图</text>`;
    let bounds = viewBox ?? "0 0 416 230";
    if (root) {
      const width = root[1].match(/\bwidth=["']([\d.]+)(?:px)?["']/i)?.[1];
      const height = root[1].match(/\bheight=["']([\d.]+)(?:px)?["']/i)?.[1];
      if (!viewBox && width && height) bounds = `0 0 ${width} ${height}`;
      inner = root[2];
    } else if (polylines.some((points) => points.length > 1)) {
      const projected = polylines.map((points) => points.map(project));
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const points of projected) {
        for (const [px, py] of points) {
          minX = Math.min(minX, px);
          maxX = Math.max(maxX, px);
          minY = Math.min(minY, py);
          maxY = Math.max(maxY, py);
        }
      }
      const padding = Math.max(1, Math.max(maxX - minX, maxY - minY) * 0.07);
      bounds = `${minX - padding} ${minY - padding} ${maxX - minX + 2 * padding} ${maxY - minY + 2 * padding}`;
      inner = projected.map((points) =>
        `<polyline points="${points.map(([px, py]) => `${px},${py}`).join(" ")}" fill="none" stroke="#cbd5e1" vector-effect="non-scaling-stroke"/>`,
      ).join("");
    }
    return `<g transform="translate(${x}, ${y})">
      <rect x="0" y="0" width="420" height="260" fill="#0d131d" stroke="#334155" stroke-width="1"/>
      <text x="12" y="20" fill="#94a3b8" font-size="12" font-family="monospace">${title}</text>
      <svg x="2" y="26" width="416" height="232" viewBox="${bounds}" overflow="hidden">${inner}</svg>
    </g>`;
  };
  const partName = sceneMesh.part_name.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 920 640" width="920" height="640">
  <rect x="0" y="0" width="920" height="640" fill="#090d14"/>
  <rect x="14" y="14" width="892" height="612" fill="none" stroke="#64748b" stroke-width="2"/>
  ${embedView(views.front, 30, 28, "FRONT VIEW (X–Z)", ([x, , z]) => [x, -z])}
  ${embedView(views.top, 470, 28, "TOP VIEW (X–Y)", ([x, y]) => [x, -y])}
  ${embedView(views.right, 30, 302, "RIGHT VIEW (Y–Z)", ([, y, z]) => [y, -z])}
  ${embedView(views.iso, 470, 302, "ISOMETRIC VIEW", ([x, y, z]) => [(x - y) * Math.sqrt(3) / 2, (x + y) / 2 - z])}
  <g transform="translate(30, 574)">
    <rect x="0" y="0" width="860" height="42" fill="#111827" stroke="#475569" stroke-width="1"/>
    <text x="14" y="17" fill="#e2e8f0" font-size="13" font-family="monospace" font-weight="bold">PART: ${partName} (${sceneMesh.kind ?? "part"})</text>
    <text x="14" y="34" fill="#94a3b8" font-size="11" font-family="monospace">BBOX: ${sx} x ${sy} x ${sz} mm | VOL: ${metrics.volume_mm3.toFixed(1)} mm3 | ISO 2768-mK</text>
    <text x="640" y="26" fill="#38bdf8" font-size="12" font-family="monospace">FACES: ${metrics.face_count} / EDGES: ${metrics.edge_count}</text>
  </g>
</svg>`;
}
