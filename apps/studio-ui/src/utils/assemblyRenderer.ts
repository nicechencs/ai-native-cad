import type { AssemblyComponentInfo, FaceMesh, SceneMeshPayload, Vec3 } from "../types/scene";
import type { VisualStyle } from "../core/viewport/displayState";

/** CenterViewport's orthographic camera, in overlay canvas coordinates. */
export interface AssemblyCamera {
  readonly yaw: number;
  readonly pitch: number;
  readonly zoom: number;
  readonly cx: number;
  readonly cy: number;
  readonly panX?: number;
  readonly panY?: number;
  readonly centerZ: number;
  readonly width: number;
  readonly height: number;
}

export interface AssemblyDisplayOptions {
  readonly visualStyle: VisualStyle;
  readonly semanticHighlights: boolean;
}

export interface AssemblyRenderStats {
  readonly drawCalls: number;
  readonly instances: number;
  readonly triangles: number;
  readonly uniqueTriangles: number;
}

type Unsupported = { readonly status: "unsupported"; readonly reason: string };
export type AssemblyRenderResult = Unsupported | { readonly status: "ready"; readonly stats: AssemblyRenderStats };

export interface AssemblyRenderer {
  /** Replace GPU geometry. Call when the scene changes, not on camera moves. */
  updateScene(scene: SceneMeshPayload): AssemblyRenderResult;
  updateCamera(camera: AssemblyCamera): void;
  updateDisplayOptions?(options: AssemblyDisplayOptions): void;
  render(): AssemblyRenderResult;
  dispose(): void;
}

type Placement = { readonly rotation: readonly [Vec3, Vec3, Vec3]; readonly origin: Vec3 };
type Mesh = { readonly vertices: Float64Array; readonly indices: Uint32Array<ArrayBuffer> };
type Batch = Mesh & { readonly placements: Placement[] };
type PreparedScene = { readonly batches: Batch[]; readonly origin: Vec3; readonly extent: Vec3 };
type GpuBatch = {
  readonly vao: WebGLVertexArrayObject;
  readonly buffers: WebGLBuffer[];
  readonly indexCount: number;
  readonly instanceCount: number;
};
type GpuScene = Pick<PreparedScene, "origin" | "extent"> & {
  readonly batches: GpuBatch[];
  readonly stats: AssemblyRenderStats;
};

const VERTEX_SIZE = 7; // position, normal, precision-bore style
const POSITION_TOLERANCE_MM = 1e-7;
const NORMAL_TOLERANCE = 1e-8;
const IDENTITY: Placement["rotation"] = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];

function fromMetadata(info: AssemblyComponentInfo | undefined): Placement | null {
  if (!info?.origin_mm || !info.rotation_deg) return null;
  if (!info.origin_mm.every(Number.isFinite) || !info.rotation_deg.every(Number.isFinite)) return null;
  const [x, y, z] = info.rotation_deg.map((angle) => angle * (Math.PI / 180));
  const cx = Math.cos(x), sx = Math.sin(x);
  const cy = Math.cos(y), sy = Math.sin(y);
  const cz = Math.cos(z), sz = Math.sin(z);
  // Column vectors of Rz * Ry * Rx, matching the kernel's Euler order.
  return {
    rotation: [
      [cz * cy, sz * cy, -sy],
      [cz * sy * sx - sz * cx, sz * sy * sx + cz * cx, cy * sx],
      [cz * sy * cx + sz * sx, sz * sy * cx - cz * sx, cy * cx],
    ],
    origin: info.origin_mm,
  };
}

function matchesGeometry(a: Mesh, b: Mesh): boolean {
  if (a.vertices.length !== b.vertices.length || a.indices.length !== b.indices.length) return false;
  if (!a.indices.every((value, i) => value === b.indices[i])) return false;
  for (let i = 0; i < a.vertices.length; i += VERTEX_SIZE) {
    if (a.vertices[i + 6] !== b.vertices[i + 6]) return false;
    for (let axis = 0; axis < 3; axis++) {
      if (Math.abs(a.vertices[i + axis] - b.vertices[i + axis]) > POSITION_TOLERANCE_MM ||
          Math.abs(a.vertices[i + 3 + axis] - b.vertices[i + 3 + axis]) > NORMAL_TOLERANCE) return false;
    }
  }
  return true;
}

function prepareScene(scene: SceneMeshPayload): PreparedScene {
  const components = new Map<string, AssemblyComponentInfo | null>();
  for (const component of scene.components ?? []) {
    // Ambiguous IDs cannot safely establish a part identity.
    components.set(component.component_id, components.has(component.component_id) ? null : component);
  }
  const groups = new Map<string | undefined, FaceMesh[]>();
  for (const face of scene.faces) {
    if (face.indices.length === 0) continue;
    const group = groups.get(face.component_id);
    if (group) group.push(face);
    else groups.set(face.component_id, [face]);
  }
  if (groups.size === 0) throw new Error("No indexed faces to render");

  const batches: Batch[] = [];
  const candidates = new Map<string, Batch[]>();
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  for (const [componentId, faces] of groups) {
    const info = componentId === undefined ? undefined : components.get(componentId) ?? undefined;
    // Placement is in mm. Missing placement or ambiguous identity uses its own world-space mesh.
    const placed = scene.units === "mm" ? fromMetadata(info) : null;
    const first = faces[0].vertices;
    const placement: Placement = placed ?? { rotation: IDENTITY, origin: [first[0], first[1], first[2]] };
    const { rotation, origin } = placement;
    const partName = placed && info?.part_name && faces.every((face) => !face.part_name || face.part_name === info.part_name)
      ? info.part_name : null;

    let vertexCount = 0;
    let indexCount = 0;
    for (const face of faces) {
      if (face.vertices.length === 0 || face.vertices.length % 3 !== 0 ||
          face.normals.length !== face.vertices.length || face.indices.length % 3 !== 0) {
        throw new Error("Invalid assembly face mesh");
      }
      vertexCount += face.vertices.length / 3;
      indexCount += face.indices.length;
    }
    if (vertexCount > 0xffffffff || indexCount > 0x7fffffff) throw new Error("Assembly mesh exceeds WebGL2 limits");
    const vertices = new Float64Array(vertexCount * VERTEX_SIZE);
    const indices = new Uint32Array(indexCount);
    let vertexBase = 0;
    let indexBase = 0;
    let topologyHash = 2166136261;
    for (const face of faces) {
      const { vertices: points, normals } = face;
      const style = Number(face.provenance_tags.includes("bearing_seat_wall"));
      let offset = vertexBase * VERTEX_SIZE;
      for (let i = 0; i < points.length; i += 3, offset += VERTEX_SIZE) {
        const x = points[i], y = points[i + 1], z = points[i + 2];
        const nx = normals[i], ny = normals[i + 1], nz = normals[i + 2];
        min[0] = Math.min(min[0], x); max[0] = Math.max(max[0], x);
        min[1] = Math.min(min[1], y); max[1] = Math.max(max[1], y);
        min[2] = Math.min(min[2], z); max[2] = Math.max(max[2], z);
        const dx = x - origin[0], dy = y - origin[1], dz = z - origin[2];
        // World -> local: transpose the rigid rotation; normals do not translate.
        for (let axis = 0; axis < 3; axis++) {
          const [rx, ry, rz] = rotation[axis];
          vertices[offset + axis] = rx * dx + ry * dy + rz * dz;
          vertices[offset + 3 + axis] = rx * nx + ry * ny + rz * nz;
        }
        vertices[offset + 6] = style;
      }
      for (const index of face.indices) {
        if (!Number.isInteger(index) || index < 0 || index >= points.length / 3) {
          throw new Error("Invalid assembly triangle index");
        }
        const combined = index + vertexBase;
        indices[indexBase++] = combined;
        topologyHash = Math.imul(topologyHash ^ combined, 16777619);
      }
      vertexBase += points.length / 3;
    }
    if (!vertices.every(Number.isFinite)) throw new Error("Invalid assembly vertex or normal");

    // The key only narrows candidates: no hash, part name or bounds alone can alias geometry.
    // Compare against the actual representative, not transitively against other near-matches.
    const key = partName === null ? null : JSON.stringify([partName, vertexCount, indexCount, topologyHash]);
    const mesh = { vertices, indices };
    const bucket = key === null ? undefined : candidates.get(key);
    const existing = bucket?.find((candidate) => matchesGeometry(candidate, mesh));
    if (existing) {
      existing.placements.push(placement);
    } else {
      const batch: Batch = { ...mesh, placements: [placement] };
      batches.push(batch);
      if (bucket) bucket.push(batch);
      else if (key !== null) candidates.set(key, [batch]);
    }
  }
  const origin: Vec3 = [min[0] / 2 + max[0] / 2, min[1] / 2 + max[1] / 2, min[2] / 2 + max[2] / 2];
  return { batches, origin, extent: [max[0] - origin[0], max[1] - origin[1], max[2] - origin[2]] };
}

const VERTEX_SHADER = `#version 300 es
layout(location = 0) in vec3 aPosition;
layout(location = 1) in vec3 aNormal;
layout(location = 2) in float aStyle;
layout(location = 3) in mat4 aPlacement;
uniform mat4 uView;
out vec3 vNormal;
flat out float vStyle;
void main() {
  gl_Position = uView * aPlacement * vec4(aPosition, 1.0);
  vNormal = mat3(aPlacement) * aNormal;
  vStyle = aStyle;
}`;

const FRAGMENT_SHADER = `#version 300 es
precision highp float;
in vec3 vNormal;
flat in float vStyle;
uniform vec4 uDisplayParams;
out vec4 outColor;
void main() {
  float shade = min(1.0, 0.32 + 0.68 * abs(dot(vNormal, vec3(0.42, -0.52, 0.74))));
  float opacity = uDisplayParams.x;
  float hlrMode = uDisplayParams.y;
  float semanticOn = uDisplayParams.z;
  float base = 92.0 + shade * 95.0;
  vec3 gray = base * vec3(1.0, 1.07, 1.18);
  vec3 bore = vec3(68.0, 135.0 + shade * 70.0, 180.0 + shade * 60.0);
  vec3 shadedRgb = mix(gray, bore, vStyle * semanticOn) / 255.0;
  float hlrTone = (230.0 + shade * 22.0) / 255.0;
  vec3 hlrRgb = vec3(hlrTone, min(1.0, hlrTone + 0.008), min(1.0, hlrTone + 0.02));
  outColor = vec4(mix(shadedRgb, hlrRgb, hlrMode), opacity);
}`;

function linkProgram(gl: WebGL2RenderingContext): WebGLProgram {
  const program = gl.createProgram();
  if (!program) throw new Error("Could not allocate assembly shader program");
  const shaders: WebGLShader[] = [];
  try {
    for (const [type, source] of [[gl.VERTEX_SHADER, VERTEX_SHADER], [gl.FRAGMENT_SHADER, FRAGMENT_SHADER]] as const) {
      const shader = gl.createShader(type);
      if (!shader) throw new Error("Could not allocate assembly shader");
      shaders.push(shader);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        throw new Error(gl.getShaderInfoLog(shader) || "Could not compile assembly shader");
      }
      gl.attachShader(program, shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error(gl.getProgramInfoLog(program) || "Could not link assembly shaders");
    }
    for (const shader of shaders) gl.detachShader(program, shader);
    return program;
  } catch (error) {
    gl.deleteProgram(program);
    throw error;
  } finally {
    for (const shader of shaders) gl.deleteShader(shader);
  }
}

function deleteBatch(gl: WebGL2RenderingContext, batch: GpuBatch): void {
  gl.deleteVertexArray(batch.vao);
  for (const buffer of batch.buffers) gl.deleteBuffer(buffer);
}

function uploadBatch(gl: WebGL2RenderingContext, batch: Batch, sceneOrigin: Vec3): GpuBatch {
  // Rebase both positions and translations before converting to f32. Models far from
  // world zero can then still align with the double-precision overlay projection.
  const anchor: Vec3 = [batch.vertices[0], batch.vertices[1], batch.vertices[2]];
  const vertices = new Float32Array(batch.vertices.length);
  for (let i = 0; i < vertices.length; i += VERTEX_SIZE) {
    for (let axis = 0; axis < 3; axis++) vertices[i + axis] = batch.vertices[i + axis] - anchor[axis];
    for (let j = 3; j < VERTEX_SIZE; j++) vertices[i + j] = batch.vertices[i + j];
  }
  const instances = new Float32Array(batch.placements.length * 16);
  batch.placements.forEach(({ rotation: [x, y, z], origin }, i) => {
    instances.set([
      ...x, 0, ...y, 0, ...z, 0,
      (origin[0] - sceneOrigin[0]) + x[0] * anchor[0] + y[0] * anchor[1] + z[0] * anchor[2],
      (origin[1] - sceneOrigin[1]) + x[1] * anchor[0] + y[1] * anchor[1] + z[1] * anchor[2],
      (origin[2] - sceneOrigin[2]) + x[2] * anchor[0] + y[2] * anchor[1] + z[2] * anchor[2],
      1,
    ], i * 16);
  });
  if (!vertices.every(Number.isFinite) || !instances.every(Number.isFinite)) {
    throw new Error("Assembly coordinates exceed WebGL2 precision range");
  }

  const vao = gl.createVertexArray();
  if (!vao) throw new Error("Could not allocate assembly vertex array");
  const gpu: GpuBatch = { vao, buffers: [], indexCount: batch.indices.length, instanceCount: batch.placements.length };
  function upload(target: number, data: Float32Array<ArrayBuffer> | Uint32Array<ArrayBuffer>) {
    const buffer = gl.createBuffer();
    if (!buffer) throw new Error("Could not allocate assembly buffer");
    gpu.buffers.push(buffer);
    gl.bindBuffer(target, buffer);
    gl.bufferData(target, data, gl.STATIC_DRAW);
  }
  try {
    gl.bindVertexArray(vao);
    upload(gl.ARRAY_BUFFER, vertices);
    for (const [index, count, offset] of [[0, 3, 0], [1, 3, 12], [2, 1, 24]]) {
      gl.enableVertexAttribArray(index);
      gl.vertexAttribPointer(index, count, gl.FLOAT, false, VERTEX_SIZE * 4, offset);
    }
    upload(gl.ARRAY_BUFFER, instances);
    for (let column = 0; column < 4; column++) {
      const index = 3 + column;
      gl.enableVertexAttribArray(index);
      gl.vertexAttribPointer(index, 4, gl.FLOAT, false, 64, column * 16);
      gl.vertexAttribDivisor(index, 1);
    }
    upload(gl.ELEMENT_ARRAY_BUFFER, batch.indices);
    if (gl.getError() !== gl.NO_ERROR) throw new Error("Could not upload assembly mesh to WebGL2");
    return gpu;
  } catch (error) {
    deleteBatch(gl, gpu);
    throw error;
  } finally {
    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);
  }
}

function viewMatrix(camera: AssemblyCamera, { origin, extent }: GpuScene): Float32Array | null {
  const { yaw, pitch, zoom, cx, cy, centerZ, width, height } = camera;
  const panX = camera.panX ?? 0;
  const panY = camera.panY ?? 0;
  if (![yaw, pitch, zoom, cx, cy, panX, panY, centerZ, width, height].every(Number.isFinite) ||
      width <= 0 || height <= 0 || zoom <= 0) return null;

  const effCx = cx + panX;
  const effCy = cy + panY;
  const cY = Math.cos(yaw), sY = Math.sin(yaw), cP = Math.cos(pitch), sP = Math.sin(pitch);
  const scaleX = 2 * zoom / width, scaleY = 2 * zoom / height;
  const offsetX = cY * origin[0] - sY * origin[1];
  const offsetY = sY * sP * origin[0] + cY * sP * origin[1] + cP * (origin[2] - centerZ);
  const dx = sY * cP, dy = cY * cP, dz = -sP;
  const depthRange = Math.max(1, 1.01 * (Math.abs(dx) * extent[0] + Math.abs(dy) * extent[1] + Math.abs(dz) * extent[2]));
  // Canvas y is downward; clip-space y is upward. Larger CPU depths are nearer.
  const matrix = new Float32Array([
    cY * scaleX, sY * sP * scaleY, -dx / depthRange, 0,
    -sY * scaleX, cY * sP * scaleY, -dy / depthRange, 0,
    0, cP * scaleY, -dz / depthRange, 0,
    2 * effCx / width - 1 + offsetX * scaleX, 1 - 2 * effCy / height + offsetY * scaleY, 0, 1,
  ]);
  return matrix.every(Number.isFinite) ? matrix : null;
}

const unsupported = (reason: string): Unsupported => ({ status: "unsupported", reason });
const errorReason = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback;

/** Use a dedicated, transparent canvas. Context loss is terminal for this renderer; switch to 2D. */
export function createAssemblyRenderer(
  canvas: HTMLCanvasElement,
  { onContextLost }: { readonly onContextLost?: () => void } = {},
): Unsupported | { readonly status: "ready"; readonly renderer: AssemblyRenderer } {
  let context: WebGL2RenderingContext | null;
  try {
    context = canvas.getContext("webgl2", { alpha: true, antialias: true, depth: true });
  } catch {
    return unsupported("WebGL2 context creation failed");
  }
  if (!context || context.isContextLost()) return unsupported("WebGL2 is unavailable");
  const gl = context;
  let program: WebGLProgram;
  try {
    program = linkProgram(gl);
  } catch (error) {
    return unsupported(errorReason(error, "Could not initialize assembly renderer"));
  }
  const view = gl.getUniformLocation(program, "uView");
  if (!view) {
    gl.deleteProgram(program);
    return unsupported("Assembly view uniform is unavailable");
  }
  const displayParams = gl.getUniformLocation(program, "uDisplayParams");

  let current: GpuScene | null = null;
  let camera: AssemblyCamera | null = null;
  let displayOptions: AssemblyDisplayOptions = {
    visualStyle: "shaded-edges",
    semanticHighlights: true,
  };
  let disposed = false;
  let lost = false;
  const handleLost = () => {
    lost = true;
    current = null;
    onContextLost?.();
  };
  canvas.addEventListener("webglcontextlost", handleLost);
  gl.clearColor(0, 0, 0, 0);
  gl.enable(gl.DEPTH_TEST);
  gl.depthFunc(gl.LEQUAL);

  function unavailable(): Unsupported | null {
    if (disposed) return unsupported("Assembly renderer was disposed");
    if (lost || gl.isContextLost()) return unsupported("WebGL2 context was lost");
    return null;
  }

  function releaseScene() {
    for (const batch of current?.batches ?? []) deleteBatch(gl, batch);
    current = null;
  }

  return {
    status: "ready",
    renderer: {
      updateScene(scene) {
        const problem = unavailable();
        if (problem) return problem;
        releaseScene();
        const batches: GpuBatch[] = [];
        try {
          const { batches: prepared, origin, extent } = prepareScene(scene);
          let instances = 0, triangles = 0, uniqueTriangles = 0;
          for (const batch of prepared) {
            const gpu = uploadBatch(gl, batch, origin);
            batches.push(gpu);
            instances += gpu.instanceCount;
            uniqueTriangles += gpu.indexCount / 3;
            triangles += (gpu.indexCount / 3) * gpu.instanceCount;
          }
          const stats = { drawCalls: batches.length, instances, triangles, uniqueTriangles };
          current = { batches, stats, origin, extent };
          return { status: "ready", stats };
        } catch (error) {
          for (const batch of batches) deleteBatch(gl, batch);
          gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
          return unsupported(errorReason(error, "Could not prepare assembly scene"));
        }
      },
      updateCamera(nextCamera) {
        camera = nextCamera;
      },
      updateDisplayOptions(nextOptions) {
        displayOptions = nextOptions;
      },
      render() {
        const problem = unavailable();
        if (problem) return problem;
        const width = gl.drawingBufferWidth, height = gl.drawingBufferHeight;
        gl.viewport(0, 0, width, height);
        gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
        if (!current || !camera || width <= 0 || height <= 0) return unsupported("Assembly renderer has no drawable scene or camera");
        const matrix = viewMatrix(camera, current);
        if (!matrix) return unsupported("Invalid assembly camera");
        const isTranslucent = displayOptions.visualStyle === "translucent";
        const isHlr = displayOptions.visualStyle === "hlr";
        if (isTranslucent) {
          gl.enable(gl.BLEND);
          gl.blendFunc?.(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        } else {
          gl.disable?.(gl.BLEND);
        }
        gl.useProgram(program);
        gl.uniformMatrix4fv(view, false, matrix);
        if (displayParams) {
          gl.uniform4f?.(
            displayParams,
            isTranslucent ? 0.32 : 1.0,
            isHlr ? 1.0 : 0.0,
            displayOptions.semanticHighlights ? 1.0 : 0.0,
            0.0,
          );
        }
        for (const { vao, indexCount, instanceCount } of current.batches) {
          gl.bindVertexArray(vao);
          gl.drawElementsInstanced(gl.TRIANGLES, indexCount, gl.UNSIGNED_INT, 0, instanceCount);
        }
        gl.bindVertexArray(null);
        return { status: "ready", stats: current.stats };
      },
      dispose() {
        if (disposed) return;
        disposed = true;
        canvas.removeEventListener("webglcontextlost", handleLost);
        releaseScene();
        gl.useProgram(null);
        gl.deleteProgram(program);
        gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
      },
    },
  };
}
