import type { FaceMesh, SceneMeshPayload, SectionCutState, SelectionTarget, TopoEdge, Vec3 } from "../types/scene.ts";
import {
  resolveEdgeDisplay,
  resolveFaceDisplay,
  type ProjectionMode,
  type ViewportDisplayState,
} from "../core/viewport/displayState.ts";
import { normalizePolyline } from "./studioHelpers.ts";

export interface CadWebglCamera {
  readonly yaw: number;
  readonly pitch: number;
  readonly zoom: number;
  readonly panX: number;
  readonly panY: number;
  readonly cx: number;
  readonly cy: number;
  readonly width: number;
  readonly height: number;
  readonly projection: ProjectionMode;
  readonly center: Vec3;
  readonly modelRadius: number;
}

export interface CadWebglFrameState {
  readonly displayState: ViewportDisplayState;
  readonly sectionCut: SectionCutState;
  readonly cutCoord: number;
  readonly explodeFactor: number;
  readonly selectedTarget: SelectionTarget;
  readonly hoveredTarget: SelectionTarget;
  readonly selectedComponentId?: string | null;
  readonly measuredFaces?: ReadonlySet<FaceMesh>;
}

export interface CadWebglRenderer {
  updateScene(scene: SceneMeshPayload, componentOrder: ReadonlyMap<string, number>): boolean;
  render(camera: CadWebglCamera, state: CadWebglFrameState): boolean;
  dispose(): void;
}

interface FaceDrawRecord {
  readonly face: FaceMesh;
  readonly indexOffset: number;
  readonly indexCount: number;
  readonly componentIndex: number;
}

interface EdgeDrawRecord {
  readonly edge: TopoEdge;
  readonly vertexOffset: number;
  readonly vertexCount: number;
  readonly componentIndex: number;
}

interface UploadedScene {
  readonly faceVao: WebGLVertexArrayObject;
  readonly faceBuffers: WebGLBuffer[];
  readonly faceRecords: readonly FaceDrawRecord[];
  readonly edgeVao: WebGLVertexArrayObject | null;
  readonly edgeBuffers: WebGLBuffer[];
  readonly edgeRecords: readonly EdgeDrawRecord[];
  readonly gridVao: WebGLVertexArrayObject | null;
  readonly gridBuffer: WebGLBuffer | null;
  readonly gridRegularCount: number;
  readonly gridAxisXOffset: number;
  readonly gridAxisYOffset: number;
}

const FACE_VERTEX_SHADER = `#version 300 es
layout(location = 0) in vec3 aPosition;
layout(location = 1) in vec3 aNormal;

uniform mat4 uViewProj;
uniform mat3 uNormalView;
uniform vec3 uExplodeOffset;
uniform vec3 uSceneCenter;

out vec3 vViewNormal;
out vec3 vWorldPos;
out float vDepthNorm;

void main() {
  vec3 worldPos = aPosition + uExplodeOffset;
  vec3 rel = worldPos - uSceneCenter;
  vec4 clipPos = uViewProj * vec4(rel, 1.0);
  gl_Position = clipPos;
  vViewNormal = uNormalView * aNormal;
  vWorldPos = worldPos;
  float ndcZ = clipPos.w != 0.0 ? (clipPos.z / clipPos.w) : clipPos.z;
  vDepthNorm = clamp(ndcZ * 0.5 + 0.5, 0.0, 1.0);
}`;

const FACE_FRAGMENT_SHADER = `#version 300 es
precision highp float;

in vec3 vViewNormal;
in vec3 vWorldPos;
in float vDepthNorm;

uniform vec4 uBaseColor;
uniform vec4 uBackColor;
uniform vec4 uSectionPlane; // xyz = cut normal (pointing into discarded half), w = threshold
uniform vec4 uParams;       // x = sectionEnabled, y = hlrMode, z = specularStrength, w = fogStrength
uniform vec3 uFogColor;

out vec4 outColor;

void main() {
  if (uParams.x > 0.5) {
    if (dot(vWorldPos, uSectionPlane.xyz) > uSectionPlane.w) {
      discard;
    }
  }

  vec3 N = normalize(vViewNormal);
  bool isBack = !gl_FrontFacing;
  if (isBack) {
    N = -N;
  }

  // Camera-space 3-point CAD studio lighting (headlight + fill + rim)
  vec3 V = vec3(0.0, 0.0, 1.0);
  vec3 L1 = normalize(vec3(0.45, 0.62, 0.72));   // Key light (upper-right front)
  vec3 L2 = normalize(vec3(-0.68, -0.32, 0.42)); // Fill light (lower-left cool)
  vec3 L3 = normalize(vec3(0.0, -0.85, 0.35));   // Subtle bottom bounce

  float ndotl1 = dot(N, L1);
  float wrap1 = max(0.0, (ndotl1 + 0.22) / 1.22);
  float diff2 = max(dot(N, L2), 0.0);
  float bounce = max(dot(N, L3), 0.0);

  vec3 H1 = normalize(L1 + V);
  float spec1 = pow(max(dot(N, H1), 0.0), 42.0);
  float rim = pow(1.0 - max(dot(N, V), 0.0), 2.6);

  vec3 litColor;
  if (uParams.y > 0.5) {
    // HLR engineering white-model: clean subtle tonal separation so slopes stay readable
    float tone = isBack
      ? (0.76 + 0.12 * wrap1)
      : (0.86 + 0.12 * wrap1 + 0.02 * diff2);
    litColor = vec3(tone, min(1.0, tone + 0.008), min(1.0, tone + 0.018));
  } else if (isBack) {
    // Distinct warm-dark inner/backface shading so front vs. back/cavity is unmistakable
    float lum = 0.26 + 0.48 * wrap1 + 0.14 * diff2;
    litColor = uBackColor.rgb * lum + vec3(0.75, 0.62, 0.52) * rim * 0.08;
  } else {
    float lum = 0.24 + 0.62 * wrap1 + 0.18 * diff2 + 0.06 * bounce;
    vec3 specular = vec3(0.95, 0.98, 1.0) * spec1 * uParams.z;
    vec3 rimGlow = vec3(0.62, 0.78, 0.96) * rim * 0.20;
    litColor = uBaseColor.rgb * lum + specular + rimGlow;
  }

  // Subtle depth cueing: foreground stays crisp, distant surfaces gently recede
  litColor = mix(litColor, uFogColor, vDepthNorm * uParams.w);
  outColor = vec4(clamp(litColor, 0.0, 1.0), uBaseColor.a);
}`;

const EDGE_VERTEX_SHADER = `#version 300 es
layout(location = 0) in vec3 aPosition;

uniform mat4 uViewProj;
uniform vec3 uExplodeOffset;
uniform vec3 uSceneCenter;
uniform float uDepthBias;

out vec3 vWorldPos;

void main() {
  vec3 worldPos = aPosition + uExplodeOffset;
  vec3 rel = worldPos - uSceneCenter;
  vec4 clipPos = uViewProj * vec4(rel, 1.0);
  clipPos.z -= uDepthBias * abs(clipPos.w);
  gl_Position = clipPos;
  vWorldPos = worldPos;
}`;

const EDGE_FRAGMENT_SHADER = `#version 300 es
precision highp float;

in vec3 vWorldPos;

uniform vec4 uEdgeColor;
uniform vec4 uSectionPlane;
uniform float uSectionEnabled;

out vec4 outColor;

void main() {
  if (uSectionEnabled > 0.5) {
    if (dot(vWorldPos, uSectionPlane.xyz) > uSectionPlane.w) {
      discard;
    }
  }
  outColor = uEdgeColor;
}`;

function compileProgram(
  gl: WebGL2RenderingContext,
  vertexSrc: string,
  fragmentSrc: string,
): WebGLProgram | null {
  const program = gl.createProgram();
  if (!program) return null;
  const shaders: WebGLShader[] = [];
  try {
    for (const [type, src] of [
      [gl.VERTEX_SHADER, vertexSrc],
      [gl.FRAGMENT_SHADER, fragmentSrc],
    ] as const) {
      const shader = gl.createShader(type);
      if (!shader) throw new Error("Failed to allocate shader");
      shaders.push(shader);
      gl.shaderSource(shader, src);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        throw new Error(gl.getShaderInfoLog(shader) || "Shader compilation failed");
      }
      gl.attachShader(program, shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error(gl.getProgramInfoLog(program) || "Shader program link failed");
    }
    for (const shader of shaders) gl.detachShader(program, shader);
    return program;
  } catch {
    gl.deleteProgram(program);
    return null;
  } finally {
    for (const shader of shaders) gl.deleteShader(shader);
  }
}

/**
 * Build a column-major 4x4 projection matrix that matches `CenterViewport.projectPoint()`
 * pixel-for-pixel in both orthographic and perspective modes.
 * Input coordinates are relative to `camera.center`.
 */
export function buildCadProjectionMatrix(camera: CadWebglCamera): {
  readonly viewProj: Float32Array;
  readonly normalView: Float32Array;
} | null {
  const { yaw, pitch, zoom, panX, panY, cx, cy, width, height, projection, modelRadius } = camera;
  if (
    ![yaw, pitch, zoom, panX, panY, cx, cy, width, height, modelRadius].every(Number.isFinite) ||
    width <= 0 ||
    height <= 0 ||
    zoom <= 0
  ) {
    return null;
  }

  const cosY = Math.cos(yaw);
  const sinY = Math.sin(yaw);
  const cosP = Math.cos(pitch);
  const sinP = Math.sin(pitch);

  // Basis rows in camera space:
  // x1 =  cosY * dx - sinY * dy
  // z2 =  sinY*sinP * dx + cosY*sinP * dy + cosP * dz   (screen up)
  // y2 =  sinY*cosP * dx + cosY*cosP * dy - sinP * dz   (toward viewer; larger is nearer)
  const rx0 = cosY, rx1 = -sinY, rx2 = 0;
  const ry0 = sinY * sinP, ry1 = cosY * sinP, ry2 = cosP;
  const rz0 = sinY * cosP, rz1 = cosY * cosP, rz2 = -sinP;

  // Column-major 3x3 normal matrix transforming world normals -> camera space (right, up, toward-eye)
  const normalView = new Float32Array([
    rx0, ry0, rz0,
    rx1, ry1, rz1,
    rx2, ry2, rz2,
  ]);

  const sx = (2 * zoom) / width;
  const sy = (2 * zoom) / height;
  const tx = (2 * (cx + panX)) / width - 1;
  const ty = 1 - (2 * (cy + panY)) / height;
  const safeRadius = Math.max(10, modelRadius);

  if (projection === "perspective") {
    const dist = Math.max(240, safeRadius * 4.2);
    const invDist = 1 / dist;
    // Map y2 in [-R, +R] -> ndcZ in [+0.92, -0.92] monotonically after perspective divide.
    const R = Math.min(dist * 0.85, safeRadius * 2.2);
    const wNear = 1 - R * invDist;
    const wFar = 1 + R * invDist;
    const zNear = -0.92 * wNear;
    const zFar = 0.92 * wFar;
    const za = (zNear - zFar) / (2 * R);
    const zb = (zNear + zFar) / 2;

    // clipX = sx * x1 + tx * (1 - y2/dist)
    // clipY = sy * z2 + ty * (1 - y2/dist)
    // clipZ = za * y2 + zb
    // clipW = -invDist * y2 + 1
    const viewProj = new Float32Array([
      sx * rx0 - tx * invDist * rz0, sy * ry0 - ty * invDist * rz0, za * rz0, -invDist * rz0,
      sx * rx1 - tx * invDist * rz1, sy * ry1 - ty * invDist * rz1, za * rz1, -invDist * rz1,
      sx * rx2 - tx * invDist * rz2, sy * ry2 - ty * invDist * rz2, za * rz2, -invDist * rz2,
      tx, ty, zb, 1,
    ]);
    return viewProj.every(Number.isFinite) ? { viewProj, normalView } : null;
  }

  const depthRange = Math.max(1, safeRadius * 2.5);
  const invDepth = -1 / depthRange;
  const viewProj = new Float32Array([
    sx * rx0, sy * ry0, invDepth * rz0, 0,
    sx * rx1, sy * ry1, invDepth * rz1, 0,
    sx * rx2, sy * ry2, invDepth * rz2, 0,
    tx, ty, 0, 1,
  ]);
  return viewProj.every(Number.isFinite) ? { viewProj, normalView } : null;
}

function parseHexRgb(color: string | undefined): readonly [number, number, number] | null {
  if (!color) return null;
  const trimmed = color.trim();
  const shortHex = /^#([0-9a-f]{3})$/i.exec(trimmed);
  if (shortHex) {
    const h = shortHex[1];
    return [
      parseInt(h[0] + h[0], 16) / 255,
      parseInt(h[1] + h[1], 16) / 255,
      parseInt(h[2] + h[2], 16) / 255,
    ];
  }
  const fullHex = /^#([0-9a-f]{6})$/i.exec(trimmed);
  if (fullHex) {
    const h = fullHex[1];
    return [
      parseInt(h.slice(0, 2), 16) / 255,
      parseInt(h.slice(2, 4), 16) / 255,
      parseInt(h.slice(4, 6), 16) / 255,
    ];
  }
  return null;
}

function componentExplodeOffset(componentIndex: number, explodeFactor: number): Vec3 {
  if (componentIndex <= 0 || explodeFactor <= 0) return [0, 0, 0];
  return [componentIndex * 14 * explodeFactor, 0, componentIndex * 28 * explodeFactor];
}

export function createCadWebglRenderer(
  canvas: HTMLCanvasElement,
  { onContextLost }: { readonly onContextLost?: () => void } = {},
): CadWebglRenderer | null {
  let gl: WebGL2RenderingContext | null = null;
  try {
    gl = canvas.getContext("webgl2", {
      alpha: true,
      antialias: true,
      depth: true,
      powerPreference: "high-performance",
      preserveDrawingBuffer: false,
    });
  } catch {
    return null;
  }
  if (!gl || gl.isContextLost()) return null;

  const faceProgram = compileProgram(gl, FACE_VERTEX_SHADER, FACE_FRAGMENT_SHADER);
  const edgeProgram = compileProgram(gl, EDGE_VERTEX_SHADER, EDGE_FRAGMENT_SHADER);
  if (!faceProgram || !edgeProgram) {
    if (faceProgram) gl.deleteProgram(faceProgram);
    if (edgeProgram) gl.deleteProgram(edgeProgram);
    return null;
  }

  const faceLoc = {
    viewProj: gl.getUniformLocation(faceProgram, "uViewProj"),
    normalView: gl.getUniformLocation(faceProgram, "uNormalView"),
    explodeOffset: gl.getUniformLocation(faceProgram, "uExplodeOffset"),
    sceneCenter: gl.getUniformLocation(faceProgram, "uSceneCenter"),
    baseColor: gl.getUniformLocation(faceProgram, "uBaseColor"),
    backColor: gl.getUniformLocation(faceProgram, "uBackColor"),
    sectionPlane: gl.getUniformLocation(faceProgram, "uSectionPlane"),
    params: gl.getUniformLocation(faceProgram, "uParams"),
    fogColor: gl.getUniformLocation(faceProgram, "uFogColor"),
  };

  const edgeLoc = {
    viewProj: gl.getUniformLocation(edgeProgram, "uViewProj"),
    explodeOffset: gl.getUniformLocation(edgeProgram, "uExplodeOffset"),
    sceneCenter: gl.getUniformLocation(edgeProgram, "uSceneCenter"),
    depthBias: gl.getUniformLocation(edgeProgram, "uDepthBias"),
    edgeColor: gl.getUniformLocation(edgeProgram, "uEdgeColor"),
    sectionPlane: gl.getUniformLocation(edgeProgram, "uSectionPlane"),
    sectionEnabled: gl.getUniformLocation(edgeProgram, "uSectionEnabled"),
  };

  let uploaded: UploadedScene | null = null;
  let disposed = false;
  let lost = false;

  const handleContextLost = () => {
    lost = true;
    uploaded = null;
    onContextLost?.();
  };
  canvas.addEventListener("webglcontextlost", handleContextLost);

  function releaseUploaded() {
    if (!uploaded || !gl) return;
    gl.deleteVertexArray(uploaded.faceVao);
    for (const buf of uploaded.faceBuffers) gl.deleteBuffer(buf);
    if (uploaded.edgeVao) gl.deleteVertexArray(uploaded.edgeVao);
    for (const buf of uploaded.edgeBuffers) gl.deleteBuffer(buf);
    if (uploaded.gridVao) gl.deleteVertexArray(uploaded.gridVao);
    if (uploaded.gridBuffer) gl.deleteBuffer(uploaded.gridBuffer);
    uploaded = null;
  }

  return {
    updateScene(scene, componentOrder) {
      if (disposed || lost || !gl || gl.isContextLost()) return false;
      releaseUploaded();

      let totalVertices = 0;
      let totalIndices = 0;
      for (const face of scene.faces) {
        if (face.indices.length === 0) continue;
        if (
          face.vertices.length === 0 ||
          face.vertices.length % 3 !== 0 ||
          face.normals.length !== face.vertices.length ||
          face.indices.length % 3 !== 0
        ) {
          return false;
        }
        totalVertices += face.vertices.length / 3;
        totalIndices += face.indices.length;
      }

      const vertexData = new Float32Array(totalVertices * 6);
      const indexData = new Uint32Array(totalIndices);
      const faceRecords: FaceDrawRecord[] = [];

      let vBase = 0;
      let iBase = 0;
      for (const face of scene.faces) {
        if (face.indices.length === 0) continue;
        const vCount = face.vertices.length / 3;
        let offset = vBase * 6;
        for (let i = 0; i < face.vertices.length; i += 3, offset += 6) {
          vertexData[offset] = face.vertices[i];
          vertexData[offset + 1] = face.vertices[i + 1];
          vertexData[offset + 2] = face.vertices[i + 2];
          vertexData[offset + 3] = face.normals[i];
          vertexData[offset + 4] = face.normals[i + 1];
          vertexData[offset + 5] = face.normals[i + 2];
        }
        const indexStart = iBase;
        for (let i = 0; i < face.indices.length; i += 1) {
          const idx = face.indices[i];
          if (!Number.isInteger(idx) || idx < 0 || idx >= vCount) return false;
          indexData[iBase++] = vBase + idx;
        }
        faceRecords.push({
          face,
          indexOffset: indexStart,
          indexCount: face.indices.length,
          componentIndex: face.component_id ? (componentOrder.get(face.component_id) ?? 0) : 0,
        });
        vBase += vCount;
      }

      // Prepare edge segments for depth-tested GL_LINES rendering
      const edgeCoords: number[] = [];
      const edgeRecords: EdgeDrawRecord[] = [];
      let edgeVertexCursor = 0;
      for (const edge of scene.edges) {
        const pts = normalizePolyline(edge.polyline);
        if (pts.length < 2) continue;
        const start = edgeVertexCursor;
        for (let i = 1; i < pts.length; i += 1) {
          const a = pts[i - 1];
          const b = pts[i];
          edgeCoords.push(a[0], a[1], a[2], b[0], b[1], b[2]);
          edgeVertexCursor += 2;
        }
        edgeRecords.push({
          edge,
          vertexOffset: start,
          vertexCount: edgeVertexCursor - start,
          componentIndex: edge.component_id ? (componentOrder.get(edge.component_id) ?? 0) : 0,
        });
      }

      const faceVao = gl.createVertexArray();
      const faceVbo = gl.createBuffer();
      const faceEbo = gl.createBuffer();
      if (!faceVao || !faceVbo || !faceEbo) {
        if (faceVao) gl.deleteVertexArray(faceVao);
        if (faceVbo) gl.deleteBuffer(faceVbo);
        if (faceEbo) gl.deleteBuffer(faceEbo);
        return false;
      }

      gl.bindVertexArray(faceVao);
      gl.bindBuffer(gl.ARRAY_BUFFER, faceVbo);
      gl.bufferData(gl.ARRAY_BUFFER, vertexData, gl.STATIC_DRAW);
      gl.enableVertexAttribArray(0);
      gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 24, 0);
      gl.enableVertexAttribArray(1);
      gl.vertexAttribPointer(1, 3, gl.FLOAT, false, 24, 12);
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, faceEbo);
      gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, indexData, gl.STATIC_DRAW);
      gl.bindVertexArray(null);
      gl.bindBuffer(gl.ARRAY_BUFFER, null);

      let edgeVao: WebGLVertexArrayObject | null = null;
      const edgeBuffers: WebGLBuffer[] = [];
      if (edgeCoords.length > 0) {
        edgeVao = gl.createVertexArray();
        const edgeVbo = gl.createBuffer();
        if (edgeVao && edgeVbo) {
          edgeBuffers.push(edgeVbo);
          gl.bindVertexArray(edgeVao);
          gl.bindBuffer(gl.ARRAY_BUFFER, edgeVbo);
          gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(edgeCoords), gl.STATIC_DRAW);
          gl.enableVertexAttribArray(0);
          gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 12, 0);
          gl.bindVertexArray(null);
          gl.bindBuffer(gl.ARRAY_BUFFER, null);
        } else {
          if (edgeVao) gl.deleteVertexArray(edgeVao);
          if (edgeVbo) gl.deleteBuffer(edgeVbo);
          edgeVao = null;
        }
      }

      // Prepare Z=0 reference grid (regular grid lines + X/Y principal axes)
      const gridCoords: number[] = [];
      const gridExtent = 100;
      const gridStep = 20;
      for (let g = -gridExtent; g <= gridExtent; g += gridStep) {
        if (g === 0) continue;
        gridCoords.push(g, -gridExtent, 0, g, gridExtent, 0);
        gridCoords.push(-gridExtent, g, 0, gridExtent, g, 0);
      }
      const gridRegularCount = gridCoords.length / 3;
      const gridAxisXOffset = gridRegularCount;
      gridCoords.push(-gridExtent, 0, 0, gridExtent, 0, 0);
      const gridAxisYOffset = gridCoords.length / 3;
      gridCoords.push(0, -gridExtent, 0, 0, gridExtent, 0);

      let gridVao: WebGLVertexArrayObject | null = null;
      let gridBuffer: WebGLBuffer | null = null;
      gridVao = gl.createVertexArray();
      gridBuffer = gl.createBuffer();
      if (gridVao && gridBuffer) {
        gl.bindVertexArray(gridVao);
        gl.bindBuffer(gl.ARRAY_BUFFER, gridBuffer);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(gridCoords), gl.STATIC_DRAW);
        gl.enableVertexAttribArray(0);
        gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 12, 0);
        gl.bindVertexArray(null);
        gl.bindBuffer(gl.ARRAY_BUFFER, null);
      } else {
        if (gridVao) gl.deleteVertexArray(gridVao);
        if (gridBuffer) gl.deleteBuffer(gridBuffer);
        gridVao = null;
        gridBuffer = null;
      }

      if (gl.getError() !== gl.NO_ERROR) {
        gl.deleteVertexArray(faceVao);
        gl.deleteBuffer(faceVbo);
        gl.deleteBuffer(faceEbo);
        if (edgeVao) gl.deleteVertexArray(edgeVao);
        for (const buf of edgeBuffers) gl.deleteBuffer(buf);
        if (gridVao) gl.deleteVertexArray(gridVao);
        if (gridBuffer) gl.deleteBuffer(gridBuffer);
        return false;
      }

      uploaded = {
        faceVao,
        faceBuffers: [faceVbo, faceEbo],
        faceRecords,
        edgeVao,
        edgeBuffers,
        edgeRecords,
        gridVao,
        gridBuffer,
        gridRegularCount,
        gridAxisXOffset,
        gridAxisYOffset,
      };
      return true;
    },

    render(camera, state) {
      if (disposed || lost || !gl || gl.isContextLost() || !uploaded) return false;
      const bufW = gl.drawingBufferWidth;
      const bufH = gl.drawingBufferHeight;
      if (bufW <= 0 || bufH <= 0) return false;

      const matrices = buildCadProjectionMatrix(camera);
      if (!matrices) return false;

      gl.viewport(0, 0, bufW, bufH);
      gl.clearColor(0, 0, 0, 0);
      gl.depthMask(true);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);

      const {
        displayState,
        sectionCut,
        cutCoord,
        explodeFactor,
        selectedTarget,
        hoveredTarget,
        selectedComponentId,
        measuredFaces,
      } = state;

      const isHlrGlobal = displayState.visualStyle === "hlr";
      const fogRgb: readonly [number, number, number] = isHlrGlobal
        ? [0.91, 0.93, 0.95]
        : [0.13, 0.16, 0.20];

      const sign = sectionCut.flip ? -1 : 1;
      const secNx = sectionCut.axis === "X" ? sign : 0;
      const secNy = sectionCut.axis === "Y" ? sign : 0;
      const secNz = sectionCut.axis === "Z" ? sign : 0;
      const secW = sign * cutCoord;

      // Classify faces into opaque pass and translucent pass
      type FacePassItem = {
        readonly record: FaceDrawRecord;
        readonly color: readonly [number, number, number, number];
        readonly backColor: readonly [number, number, number, number];
        readonly isHlr: boolean;
        readonly specStrength: number;
      };
      const opaqueItems: FacePassItem[] = [];
      const translucentItems: FacePassItem[] = [];

      for (const record of uploaded.faceRecords) {
        const faceDisplay = resolveFaceDisplay(displayState, record.face);
        if (!faceDisplay.visible || !faceDisplay.drawSurface) continue;

        const isSelectedFace =
          selectedTarget?.kind === "face" &&
          selectedTarget.face.face_id === record.face.face_id;
        const isHoveredFace =
          !isSelectedFace &&
          hoveredTarget?.kind === "face" &&
          hoveredTarget.face.face_id === record.face.face_id;
        const isSelectedComp =
          Boolean(selectedComponentId) &&
          record.face.component_id === selectedComponentId;
        const isMeasuredFace = Boolean(measuredFaces?.has(record.face));

        const alpha = isSelectedFace || isMeasuredFace ? 1.0 : faceDisplay.opacity;
        let rgb: readonly [number, number, number];
        let backRgb: readonly [number, number, number] = [0.46, 0.37, 0.33];
        let specStrength = 0.28;

        if (isMeasuredFace) {
          rgb = [0.20, 0.72, 0.78];
          backRgb = [0.14, 0.48, 0.54];
        } else if (isSelectedFace || isSelectedComp) {
          rgb = [0.98, 0.80, 0.18];
          backRgb = [0.72, 0.56, 0.14];
          specStrength = 0.34;
        } else if (isHoveredFace) {
          rgb = [0.34, 0.76, 0.98];
          backRgb = [0.22, 0.52, 0.72];
          specStrength = 0.34;
        } else {
          const overrideRgb = parseHexRgb(faceDisplay.colorOverride);
          if (overrideRgb) {
            rgb = overrideRgb;
          } else if (faceDisplay.isSemanticHighlighted) {
            rgb = [0.36, 0.72, 0.92];
            backRgb = [0.24, 0.46, 0.62];
            specStrength = 0.36;
          } else {
            // Crisp industrial machined-aluminum slate blue-silver
            rgb = [0.66, 0.72, 0.79];
            // Warm bronze-charcoal back/cavity shading for clear front-vs-back distinction
            backRgb = [0.48, 0.38, 0.34];
          }
        }

        const item: FacePassItem = {
          record,
          color: [rgb[0], rgb[1], rgb[2], alpha],
          backColor: [backRgb[0], backRgb[1], backRgb[2], alpha],
          isHlr: faceDisplay.visualStyle === "hlr" && !isSelectedFace && !isHoveredFace && !isSelectedComp && !isMeasuredFace,
          specStrength,
        };
        if (alpha < 0.99) translucentItems.push(item);
        else opaqueItems.push(item);
      }

      // Pass 1: Opaque faces with hardware depth test & polygon offset
      gl.useProgram(faceProgram);
      gl.bindVertexArray(uploaded.faceVao);
      gl.uniformMatrix4fv(faceLoc.viewProj, false, matrices.viewProj);
      gl.uniformMatrix3fv(faceLoc.normalView, false, matrices.normalView);
      gl.uniform3f(faceLoc.sceneCenter, camera.center[0], camera.center[1], camera.center[2]);
      gl.uniform4f(faceLoc.sectionPlane, secNx, secNy, secNz, secW);
      gl.uniform3f(faceLoc.fogColor, fogRgb[0], fogRgb[1], fogRgb[2]);

      gl.enable(gl.DEPTH_TEST);
      gl.depthFunc(gl.LEQUAL);
      gl.enable(gl.POLYGON_OFFSET_FILL);
      gl.polygonOffset(1.0, 1.0);

      if (opaqueItems.length > 0) {
        gl.disable(gl.BLEND);
        gl.depthMask(true);
        for (const item of opaqueItems) {
          const [ox, oy, oz] = componentExplodeOffset(item.record.componentIndex, explodeFactor);
          gl.uniform3f(faceLoc.explodeOffset, ox, oy, oz);
          gl.uniform4f(faceLoc.baseColor, item.color[0], item.color[1], item.color[2], 1.0);
          gl.uniform4f(faceLoc.backColor, item.backColor[0], item.backColor[1], item.backColor[2], 1.0);
          gl.uniform4f(
            faceLoc.params,
            sectionCut.enabled ? 1.0 : 0.0,
            item.isHlr ? 1.0 : 0.0,
            item.specStrength,
            item.isHlr ? 0.04 : 0.11,
          );
          gl.drawElements(gl.TRIANGLES, item.record.indexCount, gl.UNSIGNED_INT, item.record.indexOffset * 4);
        }
      }

      // Pass 1.5: Depth-tested Z=0 reference grid (occluded behind opaque solids, visible in front)
      if (displayState.categories.grid && uploaded.gridVao) {
        gl.useProgram(edgeProgram);
        gl.bindVertexArray(uploaded.gridVao);
        gl.uniformMatrix4fv(edgeLoc.viewProj, false, matrices.viewProj);
        gl.uniform3f(edgeLoc.sceneCenter, camera.center[0], camera.center[1], camera.center[2]);
        gl.uniform3f(edgeLoc.explodeOffset, 0, 0, 0);
        gl.uniform1f(edgeLoc.depthBias, 0.0);
        gl.uniform1f(edgeLoc.sectionEnabled, 0.0);
        gl.enable(gl.DEPTH_TEST);
        gl.depthFunc(gl.LEQUAL);
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        gl.depthMask(false);

        if (isHlrGlobal) {
          gl.uniform4f(edgeLoc.edgeColor, 0.28, 0.33, 0.41, 0.16);
        } else {
          gl.uniform4f(edgeLoc.edgeColor, 0.45, 0.56, 0.72, 0.18);
        }
        gl.drawArrays(gl.LINES, 0, uploaded.gridRegularCount);
        // X axis (subtle red) & Y axis (subtle green) for instant spatial orientation
        gl.uniform4f(edgeLoc.edgeColor, 0.96, 0.44, 0.44, isHlrGlobal ? 0.42 : 0.36);
        gl.drawArrays(gl.LINES, uploaded.gridAxisXOffset, 2);
        gl.uniform4f(edgeLoc.edgeColor, 0.29, 0.85, 0.50, isHlrGlobal ? 0.42 : 0.36);
        gl.drawArrays(gl.LINES, uploaded.gridAxisYOffset, 2);

        gl.depthMask(true);
        gl.useProgram(faceProgram);
        gl.bindVertexArray(uploaded.faceVao);
      }

      // Pass 2: Translucent faces (depth-tested against opaque, depth writes disabled)
      if (translucentItems.length > 0) {
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        gl.depthMask(false);
        for (const item of translucentItems) {
          const [ox, oy, oz] = componentExplodeOffset(item.record.componentIndex, explodeFactor);
          gl.uniform3f(faceLoc.explodeOffset, ox, oy, oz);
          gl.uniform4f(faceLoc.baseColor, item.color[0], item.color[1], item.color[2], item.color[3]);
          gl.uniform4f(faceLoc.backColor, item.backColor[0], item.backColor[1], item.backColor[2], item.backColor[3]);
          gl.uniform4f(
            faceLoc.params,
            sectionCut.enabled ? 1.0 : 0.0,
            item.isHlr ? 1.0 : 0.0,
            item.specStrength * 0.6,
            0.05,
          );
          gl.drawElements(gl.TRIANGLES, item.record.indexCount, gl.UNSIGNED_INT, item.record.indexOffset * 4);
        }
        gl.depthMask(true);
      }

      gl.disable(gl.POLYGON_OFFSET_FILL);

      // Pass 3: Depth-tested B-Rep topological edges
      if (uploaded.edgeVao && uploaded.edgeRecords.length > 0) {
        gl.useProgram(edgeProgram);
        gl.bindVertexArray(uploaded.edgeVao);
        gl.uniformMatrix4fv(edgeLoc.viewProj, false, matrices.viewProj);
        gl.uniform3f(edgeLoc.sceneCenter, camera.center[0], camera.center[1], camera.center[2]);
        gl.uniform4f(edgeLoc.sectionPlane, secNx, secNy, secNz, secW);
        gl.uniform1f(edgeLoc.sectionEnabled, sectionCut.enabled ? 1.0 : 0.0);

        const isWireframe = displayState.visualStyle === "wireframe";
        const isTranslucent = displayState.visualStyle === "translucent";
        if (isWireframe || isTranslucent) {
          gl.disable(gl.DEPTH_TEST);
        } else {
          gl.enable(gl.DEPTH_TEST);
          gl.depthFunc(gl.LEQUAL);
        }
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        gl.depthMask(false);
        gl.uniform1f(edgeLoc.depthBias, isWireframe ? 0.0 : 0.0028);

        for (const record of uploaded.edgeRecords) {
          const edgeDisplay = resolveEdgeDisplay(displayState, record.edge);
          if (!edgeDisplay.visible) continue;

          const isSelectedEdge =
            selectedTarget?.kind === "edge" &&
            selectedTarget.edge.edge_id === record.edge.edge_id;
          const isHoveredEdge =
            !isSelectedEdge &&
            hoveredTarget?.kind === "edge" &&
            hoveredTarget.edge.edge_id === record.edge.edge_id;

          let rgb: readonly [number, number, number];
          let alpha = edgeDisplay.opacity;
          if (isSelectedEdge) {
            rgb = [0.98, 0.80, 0.08];
            alpha = 1.0;
          } else if (isHoveredEdge) {
            rgb = [0.22, 0.74, 0.97];
            alpha = 1.0;
          } else {
            const overrideRgb = parseHexRgb(edgeDisplay.colorOverride);
            if (overrideRgb) {
              rgb = overrideRgb;
            } else if (isHlrGlobal) {
              rgb = [0.10, 0.14, 0.21];
            } else if (
              displayState.categories.semanticHighlights &&
              record.edge.provenance_tags.includes("bearing_seat_wall")
            ) {
              rgb = [0.25, 0.78, 0.98];
            } else if (isWireframe) {
              rgb = [0.58, 0.77, 0.99];
            } else {
              // Crisp dark-slate contour edges on shaded metal give clear visual hierarchy
              rgb = [0.13, 0.17, 0.23];
              alpha *= 0.88;
            }
          }

          const [ox, oy, oz] = componentExplodeOffset(record.componentIndex, explodeFactor);
          gl.uniform3f(edgeLoc.explodeOffset, ox, oy, oz);
          gl.uniform4f(edgeLoc.edgeColor, rgb[0], rgb[1], rgb[2], alpha);
          gl.drawArrays(gl.LINES, record.vertexOffset, record.vertexCount);
        }
        gl.depthMask(true);
      }

      gl.bindVertexArray(null);
      return true;
    },

    dispose() {
      if (disposed) return;
      disposed = true;
      canvas.removeEventListener("webglcontextlost", handleContextLost);
      releaseUploaded();
      if (gl) {
        gl.useProgram(null);
        gl.deleteProgram(faceProgram);
        gl.deleteProgram(edgeProgram);
      }
    },
  };
}
