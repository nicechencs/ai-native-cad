import type { WorkspaceAsset } from "../../../desktop/src/ipcBridge.ts";
import type { EvaluatedDemoModel } from "../data/demoWorkbenchState.ts";
import type {
  AssemblyComponentInfo,
  FaceMesh,
  SceneMeshPayload,
  SourceSpan,
} from "../types/scene.ts";

const STEP_NAME = /^[A-Za-z0-9][A-Za-z0-9_-]{0,95}\.(?:step|stp)$/i;
const SPAN: SourceSpan = { start_line: 1, start_col: 1, end_line: 1, end_col: 1 };

export interface OcctFaceRange {
  /** Inclusive triangle index of the first triangle in this B-rep face. */
  readonly first: number;
  /** Inclusive triangle index of the last triangle in this B-rep face. */
  readonly last: number;
}

/** One tessellated solid or shell returned by a STEP reader. Positions and normals are flat XYZ. */
export interface OcctPreviewMesh {
  readonly name?: string;
  readonly positions: ArrayLike<number>;
  readonly normals: ArrayLike<number>;
  readonly indices: ArrayLike<number>;
  readonly faceTriangles?: readonly OcctFaceRange[];
}

export type PlainStepImport =
  | { readonly kind: "plain"; readonly partName: string; readonly assetPath: string }
  | { readonly kind: "unsupported" }
  | { readonly kind: "none" };

const files = new Map<string, ArrayBuffer>();

export function rememberBrowserStepFile(name: string, bytes: ArrayBuffer): string {
  if (!STEP_NAME.test(name)) throw new Error("仅 STEP 或 STP 文件可以留在浏览器预览");
  if (bytes.byteLength === 0) throw new Error("STEP 文件是空的");
  const assetPath = `assets/${name}`;
  files.set(assetPath, bytes);
  return assetPath;
}

export function browserStepFile(assetPath: string): ArrayBuffer | undefined {
  return files.get(assetPath);
}

export function clearBrowserStepFiles(): void {
  files.clear();
}

export function localStepAsset(name: string, sizeBytes: number): WorkspaceAsset {
  const assetPath = `assets/${name}`;
  if (!STEP_NAME.test(name) || !browserStepFile(assetPath)) throw new Error("没有可插入的浏览器 STEP 文件");
  const expression = `import::step(path = "${assetPath}")`;
  return {
    name,
    path: assetPath,
    sizeBytes,
    format: "step",
    expression,
    source: `part ImportedAsset() -> Solid {\n    return ${expression}\n}\n`,
  };
}

function stripComments(source: string): string {
  let out = "";
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i];
    if (char === '"') {
      const start = i;
      i += 1;
      while (i < source.length && source[i] !== '"') {
        if (source[i] === "\\") i += 1;
        i += 1;
      }
      out += source.slice(start, i + 1);
      continue;
    }
    if (char === "/" && source[i + 1] === "/") {
      while (i < source.length && source[i] !== "\n") i += 1;
      i -= 1;
      continue;
    }
    out += char;
  }
  return out.replace(/\r\n/g, "\n").trim();
}

/** A plain import can be tessellated as-is. Modeling after the import must keep using the workbench service. */
export function classifyStepImport(source: string): PlainStepImport {
  const body = stripComments(source);
  if (!/\bimport\s*::\s*step\s*\(/.test(body)) return { kind: "none" };
  const match = /^part\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\)\s*->\s*Solid\s*\{\s*return\s+import\s*::\s*step\s*\(\s*path\s*=\s*"([^"]+)"\s*\)\s*;?\s*\}$/.exec(body);
  if (!match) return { kind: "unsupported" };
  const assetPath = match[2];
  if (!/^assets\/[A-Za-z0-9][A-Za-z0-9_-]{0,95}\.(?:step|stp)$/i.test(assetPath)) return { kind: "unsupported" };
  return { kind: "plain", partName: match[1], assetPath };
}

export function browserStepBlockedMessage(source: string): string | null {
  const parsed = classifyStepImport(source);
  if (parsed.kind === "none") return null;
  if (parsed.kind === "unsupported") {
    return "工作台服务未连接。浏览器不会把原始 STEP 显示成已经做过孔编辑或其他建模的结果。";
  }
  if (!browserStepFile(parsed.assetPath)) {
    return "工作台服务未连接，而且这个页面里没有该 STEP 文件。请在资产页重新选择文件。";
  }
  return null;
}

/** Reads a plain STEP import in a worker. Edited source returns null so the caller keeps the service error. */
export async function previewBrowserStep(source: string, signal: AbortSignal): Promise<EvaluatedDemoModel | null> {
  const parsed = classifyStepImport(source);
  if (parsed.kind !== "plain" || !browserStepFile(parsed.assetPath)) return null;
  const bytes = browserStepFile(parsed.assetPath);
  if (!bytes) return null;
  const { readStepOffThread } = await import("./stepPreviewWorkerClient.ts");
  const meshes = await readStepOffThread(bytes.slice(0), signal);
  return sceneFromOcctMeshes(parsed.partName, meshes);
}

function finiteArray(values: ArrayLike<number>, label: string): Float64Array {
  const copy = new Float64Array(values.length);
  for (let i = 0; i < values.length; i += 1) {
    const value = values[i];
    if (!Number.isFinite(value)) throw new Error(`${label}含有无效数字`);
    copy[i] = value;
  }
  return copy;
}

function componentId(name: string | undefined, index: number): string {
  const cleaned = (name ?? "").replace(/[^A-Za-z0-9_-]/g, "_").replace(/^_+|_+$/g, "");
  return cleaned || `step_mesh_${index}`;
}

function faceFromRange(
  positions: Float64Array,
  normals: Float64Array,
  indices: Uint32Array,
  range: OcctFaceRange,
  ids: { readonly faceId: string; readonly componentId: string; readonly partName: string },
): FaceMesh {
  const triangleCount = indices.length / 3;
  if (!Number.isInteger(range.first) || !Number.isInteger(range.last) || range.first < 0 || range.last < range.first || range.last >= triangleCount) {
    throw new Error("STEP 网格的面范围无效");
  }
  const slice = indices.slice(range.first * 3, (range.last + 1) * 3);
  let min = Infinity;
  let max = -1;
  for (const index of slice) {
    if (!Number.isInteger(index) || index < 0 || index >= positions.length / 3) throw new Error("STEP 网格的三角面索引无效");
    min = Math.min(min, index);
    max = Math.max(max, index);
  }
  const used = new Map<number, number>();
  const nextPositions: number[] = [];
  const nextNormals: number[] = [];
  const nextIndices: number[] = [];
  for (const index of slice) {
    let local = used.get(index);
    if (local === undefined) {
      local = used.size;
      used.set(index, local);
      const point = index * 3;
      nextPositions.push(positions[point], positions[point + 1], positions[point + 2]);
      nextNormals.push(normals[point], normals[point + 1], normals[point + 2]);
    }
    nextIndices.push(local);
  }
  return {
    face_id: ids.faceId,
    surface_type: "mesh",
    vertices: nextPositions,
    normals: nextNormals,
    indices: nextIndices,
    provenance_tags: ["import", "browser-step"],
    component_id: ids.componentId,
    part_name: ids.partName,
    ast_span: SPAN,
  };
}

/** Converts a reader's meshes into a preview scene. It does not claim the solid is watertight or preserve modeling edits. */
export function sceneFromOcctMeshes(partName: string, meshes: readonly OcctPreviewMesh[]): EvaluatedDemoModel {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(partName)) throw new Error("零件名无效");
  const faces: FaceMesh[] = [];
  const components: AssemblyComponentInfo[] = [];
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  meshes.forEach((mesh, meshIndex) => {
    const positions = finiteArray(mesh.positions, "顶点");
    const normals = finiteArray(mesh.normals, "法线");
    if (positions.length === 0 || positions.length % 3 !== 0 || normals.length !== positions.length) {
      throw new Error("STEP 网格的顶点或法线长度无效");
    }
    const rawIndices = finiteArray(mesh.indices, "索引");
    if (rawIndices.length === 0 || rawIndices.length % 3 !== 0) throw new Error("STEP 网格没有完整三角面");
    const indices = new Uint32Array(rawIndices.length);
    for (let i = 0; i < rawIndices.length; i += 1) {
      const index = rawIndices[i];
      if (!Number.isInteger(index) || index < 0 || index >= positions.length / 3) throw new Error("STEP 网格的三角面索引无效");
      indices[i] = index;
    }
    const id = componentId(mesh.name, meshIndex);
    const ranges = mesh.faceTriangles?.length
      ? mesh.faceTriangles
      : [{ first: 0, last: indices.length / 3 - 1 }];
    const before = faces.length;
    const partMin = [Infinity, Infinity, Infinity];
    const partMax = [-Infinity, -Infinity, -Infinity];
    ranges.forEach((range, faceIndex) => {
      const face = faceFromRange(positions, normals, indices, range, {
        faceId: `${id}_face_${faceIndex}`,
        componentId: id,
        partName,
      });
      for (let i = 0; i < face.vertices.length; i += 3) {
        for (let axis = 0; axis < 3; axis += 1) {
          const value = face.vertices[i + axis];
          partMin[axis] = Math.min(partMin[axis], value);
          partMax[axis] = Math.max(partMax[axis], value);
          min[axis] = Math.min(min[axis], value);
          max[axis] = Math.max(max[axis], value);
        }
      }
      faces.push(face);
    });
    components.push({
      component_id: id,
      part_name: partName,
      instance_name: mesh.name || id,
      face_count: faces.length - before,
      bbox_mm: { min: [partMin[0], partMin[1], partMin[2]], max: [partMax[0], partMax[1], partMax[2]] },
    });
  });
  if (faces.length === 0) throw new Error("STEP 文件没有可显示的网格");
  const sceneMesh: SceneMeshPayload = {
    format_version: "0.1",
    part_name: partName,
    kind: meshes.length > 1 ? "assembly" : "part",
    units: "mm",
    ...(meshes.length > 1 ? { components } : {}),
    faces,
    edges: [],
    edit_handles: [],
  };
  return {
    sceneMesh,
    diagnostics: {
      status: "ok",
      file: "import_preview.kcl",
      part_name: partName,
      kind: sceneMesh.kind,
      diagnostics: [{
        code: "I_BROWSER_STEP",
        severity: "info",
        message: "这是浏览器读取的原始 STEP 网格，单位按毫米显示，没有检查水密性，也不包含之后的建模修改。",
        span: SPAN,
      }],
      metrics: {
        watertight: false,
        volume_mm3: 0,
        bbox_mm: { min: [min[0], min[1], min[2]], max: [max[0], max[1], max[2]] },
        face_count: faces.length,
        edge_count: 0,
        kernel_backend: "occt-wasm",
      },
    },
  };
}
