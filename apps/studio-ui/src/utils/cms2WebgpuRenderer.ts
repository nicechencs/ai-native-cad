/**
 * Zero-copy `CMS2` WebGPU viewport renderer for Horizon 2 (`H2-E02` Phase 2).
 *
 * Design highlights:
 * - Consumes `DecodedCompactScene` directly from `cms2Decoder.ts` without expanding
 *   vertices/indices/edges into JS objects or `SceneMeshPayload`.
 * - Computes camera-relative RTE (`Relative-to-Eye`) instance transforms in `f64`
 *   before packing `f32` GPU uniforms so parts at `100,000+ mm` render without jitter.
 * - Multi-pass WGSL pipeline architecture:
 *   1. Opaque CAD 3-point shaded faces + screen-space anti-aliased wide crease edges.
 *   2. Hardware two-sided stencil section-cap pass + 45° ISO mechanical hatch overlay.
 *   3. 1-pixel `rgba32uint` GPU ID picking pass (`1x1` scissor + `256`-byte aligned readback)
 *      with lossless `encodePickTexel`/`decodePickTexel` and deterministic CPU fallback.
 *   4. Weighted Blended Order-Independent Transparency (WBOIT) overlay pass for
 *      AI `ChangePreview` ghost/diff geometry without CPU triangle sorting.
 * - Graceful `detectWebGpuCapability()` fallback to WebGL2 when WebGPU is unavailable.
 */

import {
  CMS2_EDGE_STRIDE_BYTES,
  CMS2_VERTEX_STRIDE_BYTES,
  lookupEdgeRecord,
  lookupFaceRecord,
  restoreCameraRelativeTranslation,
  type DecodedCompactScene,
  type DecodedPartPrototype,
} from "../sync/cms2Decoder.ts";
import type { ProjectionMode } from "../core/viewport/displayState.ts";
import type { SourceSpan, Vec3 } from "../types/scene.ts";

// ---------------------------------------------------------------------------
// Alignment & Binary Layout Constants (WebGPU Spec Invariants)
// ---------------------------------------------------------------------------

/** WebGPU minimum uniform buffer offset alignment & texture copy `bytesPerRow` alignment. */
export const WEBGPU_UNIFORM_ALIGN_BYTES = 256;
/** WGSL uniform struct / vec4 alignment in bytes. */
export const WEBGPU_STRUCT_ALIGN_BYTES = 16;
/** Byte size of `FrameUniforms` (16-byte & 256-byte aligned). */
export const FRAME_UNIFORM_BYTES = 256;
/** Byte stride of one `InstanceUniforms` dynamic uniform block (256-byte aligned). */
export const INSTANCE_UNIFORM_STRIDE_BYTES = 256;
/** `rgba32uint` texel size in bytes (4 x u32). */
export const PICK_TEXEL_BYTES = 16;
/** Minimum `bytesPerRow` for WebGPU `copyTextureToBuffer` on a 1x1 `rgba32uint` readback. */
export const PICK_STAGING_BYTES_PER_ROW = 256;
/** Bit mask encoding an edge hit in channel G of the `rgba32uint` pick texture. */
export const PICK_EDGE_BIT_MASK = 0x8000_0000;
/** Bit mask extracting the 31-bit face or edge slot index from channel G. */
export const PICK_SLOT_MASK = 0x7fff_ffff;

// ---------------------------------------------------------------------------
// Minimal Structural WebGPU Types (zero `@webgpu/types` dependency)
// ---------------------------------------------------------------------------

export interface WebGpuBufferLike {
  readonly size: number;
  destroy(): void;
  mapAsync?(mode: number, offset?: number, size?: number): Promise<void>;
  getMappedRange?(offset?: number, size?: number): ArrayBufferLike;
  unmap?(): void;
}

export interface WebGpuTextureViewLike {}

export interface WebGpuTextureLike {
  createView(descriptor?: Record<string, unknown>): WebGpuTextureViewLike;
  destroy(): void;
}

export interface WebGpuRenderPassEncoderLike {
  setPipeline(pipeline: unknown): void;
  setBindGroup(index: number, bindGroup: unknown, dynamicOffsets?: readonly number[]): void;
  setVertexBuffer(slot: number, buffer: WebGpuBufferLike, offset?: number, size?: number): void;
  setIndexBuffer(buffer: WebGpuBufferLike, format: "uint32", offset?: number, size?: number): void;
  setScissorRect?(x: number, y: number, width: number, height: number): void;
  setStencilReference?(reference: number): void;
  draw(vertexCount: number, instanceCount?: number, firstVertex?: number, firstInstance?: number): void;
  drawIndexed(indexCount: number, instanceCount?: number, firstIndex?: number, baseVertex?: number, firstInstance?: number): void;
  end(): void;
}

export interface WebGpuCommandEncoderLike {
  beginRenderPass(descriptor: Record<string, unknown>): WebGpuRenderPassEncoderLike;
  copyTextureToBuffer(
    source: Record<string, unknown>,
    destination: Record<string, unknown>,
    copySize: readonly [number, number, number],
  ): void;
  finish(): unknown;
}

export interface WebGpuQueueLike {
  writeBuffer(
    buffer: WebGpuBufferLike,
    bufferOffset: number,
    data: ArrayBuffer | ArrayBufferView,
    dataOffset?: number,
    size?: number,
  ): void;
  submit(commandBuffers: readonly unknown[]): void;
  onSubmittedWorkDone?(): Promise<void>;
}

export interface WebGpuDeviceLike {
  readonly queue: WebGpuQueueLike;
  readonly lost?: Promise<{ readonly reason?: string; readonly message?: string }>;
  createShaderModule(descriptor: { readonly label?: string; readonly code: string }): unknown;
  createBindGroupLayout(descriptor: Record<string, unknown>): unknown;
  createPipelineLayout(descriptor: Record<string, unknown>): unknown;
  createRenderPipeline(descriptor: Record<string, unknown>): unknown;
  createBuffer(descriptor: { readonly label?: string; readonly size: number; readonly usage: number }): WebGpuBufferLike;
  createTexture(descriptor: Record<string, unknown>): WebGpuTextureLike;
  createBindGroup(descriptor: Record<string, unknown>): unknown;
  createCommandEncoder(descriptor?: Record<string, unknown>): WebGpuCommandEncoderLike;
  destroy(): void;
}

export interface WebGpuAdapterLike {
  readonly info?: {
    readonly vendor?: string;
    readonly architecture?: string;
    readonly description?: string;
  };
  requestDevice(descriptor?: Record<string, unknown>): Promise<WebGpuDeviceLike | null>;
}

export interface WebGpuNavigatorGpuLike {
  requestAdapter(options?: { readonly powerPreference?: "high-performance" | "low-power" }): Promise<WebGpuAdapterLike | null>;
  getPreferredCanvasFormat?(): string;
}

export interface WebGpuCanvasContextLike {
  configure(config: Record<string, unknown>): void;
  getCurrentTexture(): WebGpuTextureLike;
  unconfigure?(): void;
}

const GPU_BUFFER_USAGE = {
  MAP_READ: 0x0001,
  COPY_SRC: 0x0004,
  COPY_DST: 0x0008,
  INDEX: 0x0010,
  VERTEX: 0x0020,
  UNIFORM: 0x0040,
  STORAGE: 0x0080,
} as const;

const GPU_TEXTURE_USAGE = {
  COPY_SRC: 0x0001,
  TEXTURE_BINDING: 0x0004,
  RENDER_ATTACHMENT: 0x0010,
} as const;

const GPU_MAP_MODE_READ = 0x0001;

// ---------------------------------------------------------------------------
// Public Renderer Contracts
// ---------------------------------------------------------------------------

export interface Cms2CameraState {
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
  /** World-space camera target in `f64` millimetres (RTE origin). */
  readonly center: Vec3;
  readonly modelRadius: number;
}

export interface Cms2SectionPlaneConfig {
  readonly enabled: boolean;
  /** World-space plane normal pointing into the discarded half-space. */
  readonly normal: Vec3;
  /** World-space plane constant `d` (discard when `dot(worldPos, normal) > offsetMm`). */
  readonly offsetMm: number;
  /** Enable hardware two-sided stencil cap + 45° ISO hatch fill. */
  readonly stencilCap: boolean;
  /** Optional hatch spacing in pixels (default 8.0). */
  readonly hatchSpacingPx?: number;
}

export type WboitChangePreviewKind = "ghost" | "added" | "removed" | "modified";

export interface WboitInstanceOverlay {
  readonly instanceIndex: number;
  readonly kind: WboitChangePreviewKind;
  readonly opacity: number;
  readonly color?: readonly [number, number, number];
}

export interface Cms2FrameState {
  readonly sectionPlane?: Cms2SectionPlaneConfig;
  readonly explodeFactor?: number;
  readonly showEdges?: boolean;
  readonly edgeWidthPx?: number;
  readonly selectedInstanceIndex?: number | null;
  readonly selectedFaceSlot?: number | null;
  readonly hoveredInstanceIndex?: number | null;
  readonly hoveredFaceSlot?: number | null;
  readonly selectedEdgeSlot?: number | null;
  readonly hoveredEdgeSlot?: number | null;
  /** Translucent AI ChangePreview overlays rendered via WBOIT without CPU sorting. */
  readonly wboitOverlays?: readonly WboitInstanceOverlay[];
  readonly backgroundColor?: readonly [number, number, number, number];
  readonly baseColor?: readonly [number, number, number, number];
  readonly backColor?: readonly [number, number, number, number];
  readonly fogStrength?: number;
}

export interface WebGpuCapabilityResult {
  readonly available: boolean;
  readonly backend: "webgpu" | "webgl2-fallback";
  readonly reason: string;
  readonly adapterInfo?: {
    readonly vendor?: string;
    readonly architecture?: string;
    readonly description?: string;
  };
}

export interface PickTexelData {
  readonly kind: "face" | "edge";
  readonly instanceIndex: number;
  readonly faceSlotId: number | null;
  readonly edgeSlotId: number | null;
  readonly flags: number;
  /** Normalized depth in WebGPU NDC `[0, 1]` (0 = near, 1 = far). */
  readonly depthNdc: number;
}

export interface Cms2PickResult extends PickTexelData {
  readonly prototypeIndex: number;
  readonly componentId: string | null;
  readonly faceId: string | null;
  readonly edgeId: string | null;
  readonly worldPointMm: readonly [number, number, number];
  readonly astSpan: SourceSpan | null;
}

export interface Cms2PrototypeUploadPlan {
  readonly prototypeIndex: number;
  readonly partName: string;
  readonly vertexCount: number;
  readonly indexCount: number;
  readonly edgeSegmentCount: number;
  readonly vertexStrideBytes: number;
  readonly edgeStrideBytes: number;
  readonly vertexBytes: Uint8Array;
  readonly indices: Uint32Array;
  readonly edgeBytes: Uint8Array;
  /** True when the views directly alias the source `CMS2` buffer without copying. */
  readonly zeroCopy: boolean;
}

export type Cms2RenderPassKind =
  | "opaque-solid"
  | "opaque-edges"
  | "stencil-cap-mask"
  | "stencil-cap-hatch"
  | "wboit-accum"
  | "wboit-composite"
  | "gpu-pick-1px";

export interface Cms2RenderPassDescriptor {
  readonly kind: Cms2RenderPassKind;
  readonly drawCalls: number;
  readonly instanceIndices: readonly number[];
  readonly scissorRect?: {
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
  };
  readonly readbackBytesPerRow?: number;
}

export interface Cms2RenderPlan {
  readonly prototypes: readonly Cms2PrototypeUploadPlan[];
  readonly frameUniforms: Float32Array;
  readonly instanceUniforms: ArrayBuffer;
  readonly instanceStrideBytes: number;
  readonly instanceCount: number;
  readonly passes: readonly Cms2RenderPassDescriptor[];
  readonly stats: {
    readonly drawCalls: number;
    readonly triangles: number;
    readonly edgeSegments: number;
    readonly wboitInstances: number;
    readonly stencilCapActive: boolean;
  };
}

export interface Cms2WebgpuRenderer {
  readonly backend: "webgpu";
  updateScene(scene: DecodedCompactScene): boolean;
  render(camera: Cms2CameraState, state?: Cms2FrameState): Cms2RenderPlan | null;
  pickPixel(
    camera: Cms2CameraState,
    pixelX: number,
    pixelY: number,
    state?: Cms2FrameState,
  ): Promise<Cms2PickResult | null>;
  dispose(): void;
}

// ---------------------------------------------------------------------------
// Alignment & Bit-Packing Utilities
// ---------------------------------------------------------------------------

/** Rounds `byteLength` up to the nearest multiple of `alignment` (power of two). */
export function alignTo(byteLength: number, alignment: number): number {
  if (alignment <= 0 || (alignment & (alignment - 1)) !== 0) {
    throw new Error(`Alignment must be a positive power of two, got ${alignment}`);
  }
  return (byteLength + alignment - 1) & ~(alignment - 1);
}

const f32Scratch = new Float32Array(1);
const u32Scratch = new Uint32Array(f32Scratch.buffer);

export function float32ToBits(value: number): number {
  f32Scratch[0] = value;
  return u32Scratch[0] >>> 0;
}

export function bitsToFloat32(bits: number): number {
  u32Scratch[0] = bits >>> 0;
  return f32Scratch[0];
}

// ---------------------------------------------------------------------------
// Graceful WebGPU Capability Detection
// ---------------------------------------------------------------------------

export interface WebGpuCapabilityEnv {
  readonly navigator?: { readonly gpu?: WebGpuNavigatorGpuLike } | null;
}

/**
 * Synchronous pre-check for WebGPU availability (never throws in Node/SSR/headless).
 */
export function detectWebGpuCapabilitySync(env?: WebGpuCapabilityEnv): WebGpuCapabilityResult {
  const nav = env ? env.navigator : (typeof navigator !== "undefined" ? (navigator as unknown as { readonly gpu?: WebGpuNavigatorGpuLike }) : null);
  if (!nav || !nav.gpu || typeof nav.gpu.requestAdapter !== "function") {
    return {
      available: false,
      backend: "webgl2-fallback",
      reason: "WebGPU API (navigator.gpu) is not available in this runtime",
    };
  }
  return {
    available: true,
    backend: "webgpu",
    reason: "WebGPU API detected; adapter probe ready",
  };
}

/**
 * Async capability probe that verifies a real `GPUAdapter` can be acquired and
 * degrades cleanly to `"webgl2-fallback"` in headless Node or unsupported browsers.
 */
export async function detectWebGpuCapability(env?: WebGpuCapabilityEnv): Promise<WebGpuCapabilityResult> {
  const sync = detectWebGpuCapabilitySync(env);
  if (!sync.available) {
    return sync;
  }
  const nav = env ? env.navigator : (navigator as unknown as { readonly gpu?: WebGpuNavigatorGpuLike });
  try {
    const adapter = await nav!.gpu!.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) {
      return {
        available: false,
        backend: "webgl2-fallback",
        reason: "No compatible WebGPU adapter available",
      };
    }
    return {
      available: true,
      backend: "webgpu",
      reason: "WebGPU high-performance adapter available",
      ...(adapter.info ? { adapterInfo: adapter.info } : {}),
    };
  } catch (error) {
    return {
      available: false,
      backend: "webgl2-fallback",
      reason: `WebGPU adapter request failed: ${error instanceof Error ? error.message : String(error)}`,
    };
  }
}

// ---------------------------------------------------------------------------
// RTE f64 Math & WebGPU Projection Matrix ([0, 1] NDC Z)
// ---------------------------------------------------------------------------

type Mat3F64 = readonly [
  number, number, number,
  number, number, number,
  number, number, number,
];

/** Matches Rust `cad_kernel_occt::Mat4::rotate_euler_deg` and `cms2Decoder.ts`: Rz * Ry * Rx. */
function rotationMatrixDegF64(rotationDeg: Vec3): Mat3F64 {
  const degToRad = Math.PI / 180;
  const rx = rotationDeg[0] * degToRad;
  const ry = rotationDeg[1] * degToRad;
  const rz = rotationDeg[2] * degToRad;
  const sx = Math.sin(rx);
  const cx = Math.cos(rx);
  const sy = Math.sin(ry);
  const cy = Math.cos(ry);
  const sz = Math.sin(rz);
  const cz = Math.cos(rz);

  return [
    cz * cy,
    cz * sy * sx - sz * cx,
    cz * sy * cx + sz * sx,
    sz * cy,
    sz * sy * sx + cz * cx,
    sz * sy * cx - cz * sx,
    -sy,
    cy * sx,
    cy * cx,
  ];
}

function mulMat3F64(m: Mat3F64, x: number, y: number, z: number): readonly [number, number, number] {
  return [
    m[0] * x + m[1] * y + m[2] * z,
    m[3] * x + m[4] * y + m[5] * z,
    m[6] * x + m[7] * y + m[8] * z,
  ];
}

function instanceExplodeOffset(instanceIndex: number, explodeFactor: number): Vec3 {
  if (instanceIndex <= 0 || explodeFactor <= 0) return [0, 0, 0];
  return [instanceIndex * 14 * explodeFactor, 0, instanceIndex * 28 * explodeFactor];
}

export interface RteInstanceTransform {
  /** Column-major 4x4 model matrix in camera-relative RTE coordinates (packed to f32). */
  readonly modelRte: Float32Array;
  /** Column-major 3x3 rotation/normal matrix packed into 3 vec4 columns (12 floats = 48B). */
  readonly normalCols: Float32Array;
  /** Exact `f64` camera-relative translation `(origin - cameraTarget + R * anchor + explode)`. */
  readonly translationF64: readonly [number, number, number];
  /** `f32` rounded RTE translation matching `restoreCameraRelativeTranslation`. */
  readonly translationF32: readonly [number, number, number];
  /** Exact `f64` rotation matrix (row-major 3x3). */
  readonly rotationF64: Mat3F64;
}

/**
 * Computes the camera-relative RTE instance transform in `f64` before casting to `f32`
 * uniforms, preserving sub-micrometer precision even at `100,000+ mm` world offsets.
 */
export function computeRteInstanceTransform(
  prototype: Pick<DecodedPartPrototype, "anchor_mm">,
  instance: { readonly origin_mm: Vec3; readonly rotation_deg: Vec3 },
  cameraTargetMm: Vec3,
  explodeOffsetMm: Vec3 = [0, 0, 0],
): RteInstanceTransform {
  const rot = rotationMatrixDegF64(instance.rotation_deg);
  const [ax, ay, az] = mulMat3F64(rot, prototype.anchor_mm[0], prototype.anchor_mm[1], prototype.anchor_mm[2]);

  const txF64 = instance.origin_mm[0] - cameraTargetMm[0] + ax + explodeOffsetMm[0];
  const tyF64 = instance.origin_mm[1] - cameraTargetMm[1] + ay + explodeOffsetMm[1];
  const tzF64 = instance.origin_mm[2] - cameraTargetMm[2] + az + explodeOffsetMm[2];
  const translationF32 = restoreCameraRelativeTranslation(prototype, instance, cameraTargetMm, explodeOffsetMm);

  // Column-major 4x4 matrix: col0, col1, col2, col3
  const modelRte = new Float32Array([
    rot[0], rot[3], rot[6], 0,
    rot[1], rot[4], rot[7], 0,
    rot[2], rot[5], rot[8], 0,
    translationF32[0], translationF32[1], translationF32[2], 1,
  ]);

  // 3 vec4 columns (48 bytes) for std140/WGSL uniform alignment
  const normalCols = new Float32Array([
    rot[0], rot[3], rot[6], 0,
    rot[1], rot[4], rot[7], 0,
    rot[2], rot[5], rot[8], 0,
  ]);

  return {
    modelRte,
    normalCols,
    translationF64: [txF64, tyF64, tzF64],
    translationF32,
    rotationF64: rot,
  };
}

export interface WebGpuProjectionMatrices {
  /** Column-major 4x4 view-projection matrix mapping RTE world coords -> WebGPU clip space (z in [0, w]). */
  readonly viewProj: Float32Array;
  /** Exact `f64` column-major view-projection matrix for deterministic CPU picking. */
  readonly viewProjF64: Float64Array;
  /** Column-major 3x3 normal matrix packed as 3 vec4 columns (12 floats = 48B) for WGSL. */
  readonly normalViewCols: Float32Array;
}

/**
 * Builds a WebGPU-native (`ndcZ in [0, 1]`, smaller = nearer) column-major 4x4 projection matrix
 * matching `CenterViewport.projectPoint()` pixel-for-pixel in X/Y for orthographic & perspective modes.
 * Input coordinates are relative to `camera.center` (RTE).
 */
export function buildWebGpuProjectionMatrix(camera: Cms2CameraState): WebGpuProjectionMatrices | null {
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

  // Camera basis rows:
  // right  (x1):  cosY * dx - sinY * dy
  // up     (z2):  sinY*sinP * dx + cosY*sinP * dy + cosP * dz
  // toward (y2):  sinY*cosP * dx + cosY*cosP * dy - sinP * dz  (larger = nearer)
  const rx0 = cosY, rx1 = -sinY, rx2 = 0;
  const ry0 = sinY * sinP, ry1 = cosY * sinP, ry2 = cosP;
  const rz0 = sinY * cosP, rz1 = cosY * cosP, rz2 = -sinP;

  const normalViewCols = new Float32Array([
    rx0, ry0, rz0, 0,
    rx1, ry1, rz1, 0,
    rx2, ry2, rz2, 0,
  ]);

  const sx = (2 * zoom) / width;
  const sy = (2 * zoom) / height;
  const tx = (2 * (cx + panX)) / width - 1;
  const ty = 1 - (2 * (cy + panY)) / height;
  const safeRadius = Math.max(10, modelRadius);

  let viewProjF64: Float64Array;
  if (projection === "perspective") {
    const dist = Math.max(240, safeRadius * 4.2);
    const invDist = 1 / dist;
    const R = Math.min(dist * 0.85, safeRadius * 2.2);
    const wNear = 1 - R * invDist;
    const wFar = 1 + R * invDist;
    // Map y2 = +R (near) -> ndcZ = 0.04, y2 = -R (far) -> ndcZ = 0.96
    const zNear = 0.04 * wNear;
    const zFar = 0.96 * wFar;
    const za = (zNear - zFar) / (2 * R);
    const zb = (zNear + zFar) / 2;

    viewProjF64 = new Float64Array([
      sx * rx0 - tx * invDist * rz0, sy * ry0 - ty * invDist * rz0, za * rz0, -invDist * rz0,
      sx * rx1 - tx * invDist * rz1, sy * ry1 - ty * invDist * rz1, za * rz1, -invDist * rz1,
      sx * rx2 - tx * invDist * rz2, sy * ry2 - ty * invDist * rz2, za * rz2, -invDist * rz2,
      tx, ty, zb, 1,
    ]);
  } else {
    const depthRange = Math.max(1, safeRadius * 2.5);
    const za = -0.5 / depthRange;
    const zb = 0.5;
    viewProjF64 = new Float64Array([
      sx * rx0, sy * ry0, za * rz0, 0,
      sx * rx1, sy * ry1, za * rz1, 0,
      sx * rx2, sy * ry2, za * rz2, 0,
      tx, ty, zb, 1,
    ]);
  }

  const viewProj = Float32Array.from(viewProjF64);
  if (!viewProj.every(Number.isFinite)) return null;
  return { viewProj, viewProjF64, normalViewCols };
}

// ---------------------------------------------------------------------------
// 1-Pixel Pick Texel Encoding / Decoding (Lossless `rgba32uint`)
// ---------------------------------------------------------------------------

/**
 * Encodes a pick hit into a 4-word `rgba32uint` texel matching Rust `cad-render`:
 * - `r`: `instanceIndex + 1` (`0` = background)
 * - `g`: `(kind === "edge" ? 0x8000_0000 : 0) | (slot & 0x7fff_ffff)`
 * - `b`: `flags` (vertex/edge semantic flags)
 * - `a`: `float32ToBits(depthNdc)`
 */
export function encodePickTexel(hit: PickTexelData): Uint32Array {
  const slot = hit.kind === "edge" ? (hit.edgeSlotId ?? 0) : (hit.faceSlotId ?? 0);
  const packedSlot = (hit.kind === "edge" ? PICK_EDGE_BIT_MASK : 0) | (slot & PICK_SLOT_MASK);
  return new Uint32Array([
    (hit.instanceIndex + 1) >>> 0,
    packedSlot >>> 0,
    hit.flags >>> 0,
    float32ToBits(hit.depthNdc),
  ]);
}

/**
 * Decodes a 4-word `rgba32uint` texel back into `PickTexelData`, or `null` for background (`r === 0`).
 */
export function decodePickTexel(texel: ArrayLike<number>): PickTexelData | null {
  if (texel.length < 4) return null;
  const instanceRaw = texel[0] >>> 0;
  if (instanceRaw === 0) return null;
  const instanceIndex = instanceRaw - 1;
  const packed = texel[1] >>> 0;
  const isEdge = (packed & PICK_EDGE_BIT_MASK) !== 0;
  const slot = (packed & PICK_SLOT_MASK) >>> 0;
  const flags = texel[2] >>> 0;
  const depthNdc = bitsToFloat32(texel[3] >>> 0);

  return {
    kind: isEdge ? "edge" : "face",
    instanceIndex,
    faceSlotId: isEdge ? null : slot,
    edgeSlotId: isEdge ? slot : null,
    flags,
    depthNdc,
  };
}

/** Resolves a decoded pick texel against `DecodedCompactScene` face/edge tables. */
export function resolvePickTexel(
  scene: DecodedCompactScene,
  texel: ArrayLike<number>,
  worldPointMm: readonly [number, number, number] = [0, 0, 0],
): Cms2PickResult | null {
  const decoded = decodePickTexel(texel);
  if (!decoded) return null;
  if (decoded.kind === "edge" && decoded.edgeSlotId !== null) {
    const resolved = lookupEdgeRecord(scene, decoded.instanceIndex, decoded.edgeSlotId);
    if (!resolved) return null;
    return {
      ...decoded,
      prototypeIndex: resolved.prototypeIndex,
      componentId: resolved.instance.component_id,
      faceId: null,
      edgeId: resolved.edge.edge_id,
      worldPointMm,
      astSpan: resolved.edge.ast_span,
    };
  }
  if (decoded.kind === "face" && decoded.faceSlotId !== null) {
    const resolved = lookupFaceRecord(scene, decoded.instanceIndex, decoded.faceSlotId);
    if (!resolved) return null;
    return {
      ...decoded,
      prototypeIndex: resolved.prototypeIndex,
      componentId: resolved.instance.component_id,
      faceId: resolved.face.face_id,
      edgeId: null,
      worldPointMm,
      astSpan: resolved.face.ast_span,
    };
  }
  return null;
}

// ---------------------------------------------------------------------------
// WBOIT Weight Function (McGuire & Bavoil 2013, NDC Depth Monotonic)
// ---------------------------------------------------------------------------

/**
 * Computes the WBOIT depth-and-alpha weight matching the WGSL WBOIT fragment shader.
 * Strictly monotonically decreasing with `depthNdc in [0, 1]` (nearer fragments weigh more)
 * and monotonically increasing with `alpha in [0, 1]`.
 */
export function computeWboitWeight(depthNdc: number, alpha: number): number {
  const clampedZ = Math.min(1, Math.max(0, depthNdc));
  const clampedA = Math.min(1, Math.max(0, alpha));
  const alphaTerm = Math.pow(Math.min(1, clampedA * 10) + 0.01, 3);
  const depthTerm = Math.pow(1 - clampedZ * 0.9, 3);
  return Math.min(3e3, Math.max(1e-2, alphaTerm * 2.5e3 * depthTerm));
}

// ---------------------------------------------------------------------------
// Deterministic CPU Fallback Picker on `DecodedCompactScene`
// ---------------------------------------------------------------------------

export interface PickCompactSceneOptions {
  readonly sectionPlane?: Cms2SectionPlaneConfig;
  readonly explodeFactor?: number;
  readonly pickMode?: "all" | "face" | "edge";
  readonly edgeTolerancePx?: number;
}

function mulMat4F64(
  m: Float64Array,
  x: number,
  y: number,
  z: number,
): readonly [number, number, number, number] {
  return [
    m[0] * x + m[4] * y + m[8] * z + m[12],
    m[1] * x + m[5] * y + m[9] * z + m[13],
    m[2] * x + m[6] * y + m[10] * z + m[14],
    m[3] * x + m[7] * y + m[11] * z + m[15],
  ];
}

function isClippedBySectionPlane(
  worldPos: readonly [number, number, number],
  sectionPlane?: Cms2SectionPlaneConfig,
): boolean {
  if (!sectionPlane || !sectionPlane.enabled) return false;
  const d =
    worldPos[0] * sectionPlane.normal[0] +
    worldPos[1] * sectionPlane.normal[1] +
    worldPos[2] * sectionPlane.normal[2];
  return d > sectionPlane.offsetMm;
}

/**
 * Deterministic CPU fallback pixel picker directly over `DecodedCompactScene`
 * (zero `SceneMeshPayload` expansion). Uses `f64` RTE transforms, supports
 * orthographic & perspective projections, section-plane clipping, and face/edge picking.
 */
export function pickCompactScenePixel(
  scene: DecodedCompactScene,
  camera: Cms2CameraState,
  pixelX: number,
  pixelY: number,
  options: PickCompactSceneOptions = {},
): Cms2PickResult | null {
  if (
    !Number.isFinite(pixelX) ||
    !Number.isFinite(pixelY) ||
    pixelX < 0 ||
    pixelY < 0 ||
    pixelX >= camera.width ||
    pixelY >= camera.height
  ) {
    return null;
  }
  const proj = buildWebGpuProjectionMatrix(camera);
  if (!proj) return null;

  const px = pixelX + 0.5;
  const py = pixelY + 0.5;
  const explodeFactor = options.explodeFactor ?? 0;
  const pickMode = options.pickMode ?? "all";
  const edgeTolerancePx = options.edgeTolerancePx ?? 4;

  let bestFace: Cms2PickResult | null = null;
  let bestEdge: Cms2PickResult | null = null;

  for (let instIdx = 0; instIdx < scene.instances.length; instIdx += 1) {
    const instance = scene.instances[instIdx];
    const proto = scene.prototypes[instance.prototype_index];
    if (!proto) continue;

    const explode = instanceExplodeOffset(instIdx, explodeFactor);
    const rte = computeRteInstanceTransform(proto, instance, camera.center, explode);
    const [tx, ty, tz] = rte.translationF64;
    const rot = rte.rotationF64;

    if (pickMode !== "edge" && proto.indexCount >= 3) {
      const vData = proto.vertexData;
      const vWords = proto.vertexWords;
      const idxBuf = proto.indices;

      for (let t = 0; t + 2 < proto.indexCount; t += 3) {
        const i0 = idxBuf[t];
        const i1 = idxBuf[t + 1];
        const i2 = idxBuf[t + 2];
        const b0 = i0 * 8;
        const b1 = i1 * 8;
        const b2 = i2 * 8;

        const [r0x, r0y, r0z] = mulMat3F64(rot, vData[b0], vData[b0 + 1], vData[b0 + 2]);
        const [r1x, r1y, r1z] = mulMat3F64(rot, vData[b1], vData[b1 + 1], vData[b1 + 2]);
        const [r2x, r2y, r2z] = mulMat3F64(rot, vData[b2], vData[b2 + 1], vData[b2 + 2]);

        const rel0x = r0x + tx, rel0y = r0y + ty, rel0z = r0z + tz;
        const rel1x = r1x + tx, rel1y = r1y + ty, rel1z = r1z + tz;
        const rel2x = r2x + tx, rel2y = r2y + ty, rel2z = r2z + tz;

        const c0 = mulMat4F64(proj.viewProjF64, rel0x, rel0y, rel0z);
        const c1 = mulMat4F64(proj.viewProjF64, rel1x, rel1y, rel1z);
        const c2 = mulMat4F64(proj.viewProjF64, rel2x, rel2y, rel2z);
        if (c0[3] <= 1e-6 || c1[3] <= 1e-6 || c2[3] <= 1e-6) continue;

        const sx0 = ((c0[0] / c0[3] + 1) * camera.width) * 0.5;
        const sy0 = ((1 - c0[1] / c0[3]) * camera.height) * 0.5;
        const sz0 = c0[2] / c0[3];

        const sx1 = ((c1[0] / c1[3] + 1) * camera.width) * 0.5;
        const sy1 = ((1 - c1[1] / c1[3]) * camera.height) * 0.5;
        const sz1 = c1[2] / c1[3];

        const sx2 = ((c2[0] / c2[3] + 1) * camera.width) * 0.5;
        const sy2 = ((1 - c2[1] / c2[3]) * camera.height) * 0.5;
        const sz2 = c2[2] / c2[3];

        // 2D bounding box quick reject
        if (
          px < Math.min(sx0, sx1, sx2) - 1e-6 ||
          px > Math.max(sx0, sx1, sx2) + 1e-6 ||
          py < Math.min(sy0, sy1, sy2) - 1e-6 ||
          py > Math.max(sy0, sy1, sy2) + 1e-6
        ) {
          continue;
        }

        const denom = (sy1 - sy2) * (sx0 - sx2) + (sx2 - sx1) * (sy0 - sy2);
        if (Math.abs(denom) < 1e-9) continue;

        const w0 = ((sy1 - sy2) * (px - sx2) + (sx2 - sx1) * (py - sy2)) / denom;
        const w1 = ((sy2 - sy0) * (px - sx2) + (sx0 - sx2) * (py - sy2)) / denom;
        const w2 = 1 - w0 - w1;
        if (w0 < -1e-6 || w1 < -1e-6 || w2 < -1e-6) continue;

        // Perspective-correct interpolation weights
        const invW0 = w0 / c0[3];
        const invW1 = w1 / c1[3];
        const invW2 = w2 / c2[3];
        const invWSum = invW0 + invW1 + invW2;
        const p0 = invW0 / invWSum;
        const p1 = invW1 / invWSum;
        const p2 = invW2 / invWSum;

        const depthNdc = w0 * sz0 + w1 * sz1 + w2 * sz2;
        if (depthNdc < 0 || depthNdc > 1) continue;

        const worldPointMm: [number, number, number] = [
          camera.center[0] + p0 * rel0x + p1 * rel1x + p2 * rel2x,
          camera.center[1] + p0 * rel0y + p1 * rel1y + p2 * rel2y,
          camera.center[2] + p0 * rel0z + p1 * rel1z + p2 * rel2z,
        ];

        if (isClippedBySectionPlane(worldPointMm, options.sectionPlane)) continue;

        if (!bestFace || depthNdc < bestFace.depthNdc) {
          const faceSlotId = vWords[b0 + 3] >>> 0;
          const flags = vWords[b0 + 7] >>> 0;
          const faceRec = proto.face_table[faceSlotId];
          bestFace = {
            kind: "face",
            instanceIndex: instIdx,
            prototypeIndex: instance.prototype_index,
            faceSlotId,
            edgeSlotId: null,
            flags,
            depthNdc,
            componentId: instance.component_id,
            faceId: faceRec?.face_id ?? null,
            edgeId: null,
            worldPointMm,
            astSpan: faceRec?.ast_span ?? null,
          };
        }
      }
    }

    if (pickMode !== "face" && proto.edgeSegmentCount > 0) {
      const eData = proto.edgeData;
      const eWords = proto.edgeWords;
      for (let s = 0; s < proto.edgeSegmentCount; s += 1) {
        const base = s * 8;
        const [rax, ray, raz] = mulMat3F64(rot, eData[base], eData[base + 1], eData[base + 2]);
        const [rbx, rby, rbz] = mulMat3F64(rot, eData[base + 4], eData[base + 5], eData[base + 6]);
        const relAx = rax + tx, relAy = ray + ty, relAz = raz + tz;
        const relBx = rbx + tx, relBy = rby + ty, relBz = rbz + tz;

        const ca = mulMat4F64(proj.viewProjF64, relAx, relAy, relAz);
        const cb = mulMat4F64(proj.viewProjF64, relBx, relBy, relBz);
        if (ca[3] <= 1e-6 || cb[3] <= 1e-6) continue;

        const ax = ((ca[0] / ca[3] + 1) * camera.width) * 0.5;
        const ay = ((1 - ca[1] / ca[3]) * camera.height) * 0.5;
        const az = ca[2] / ca[3];
        const bx = ((cb[0] / cb[3] + 1) * camera.width) * 0.5;
        const by = ((1 - cb[1] / cb[3]) * camera.height) * 0.5;
        const bz = cb[2] / cb[3];

        const dx = bx - ax;
        const dy = by - ay;
        const lenSq = dx * dx + dy * dy;
        const u = lenSq > 1e-9 ? Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / lenSq)) : 0;
        const cx = ax + u * dx;
        const cy = ay + u * dy;
        const dist = Math.hypot(px - cx, py - cy);
        if (dist > edgeTolerancePx) continue;

        const depthNdc = az + u * (bz - az);
        if (depthNdc < 0 || depthNdc > 1) continue;

        const worldPointMm: [number, number, number] = [
          camera.center[0] + relAx + u * (relBx - relAx),
          camera.center[1] + relAy + u * (relBy - relAy),
          camera.center[2] + relAz + u * (relBz - relAz),
        ];
        if (isClippedBySectionPlane(worldPointMm, options.sectionPlane)) continue;

        if (!bestEdge || depthNdc < bestEdge.depthNdc) {
          const edgeSlotId = eWords[base + 3] >>> 0;
          const flags = eWords[base + 7] >>> 0;
          const edgeRec = proto.edge_table[edgeSlotId];
          bestEdge = {
            kind: "edge",
            instanceIndex: instIdx,
            prototypeIndex: instance.prototype_index,
            faceSlotId: null,
            edgeSlotId,
            flags,
            depthNdc,
            componentId: instance.component_id,
            faceId: null,
            edgeId: edgeRec?.edge_id ?? null,
            worldPointMm,
            astSpan: edgeRec?.ast_span ?? null,
          };
        }
      }
    }
  }

  if (pickMode === "edge") return bestEdge;
  if (pickMode === "face") return bestFace;
  if (bestEdge && (!bestFace || bestEdge.depthNdc <= bestFace.depthNdc + 0.002)) {
    return bestEdge;
  }
  return bestFace;
}

// ---------------------------------------------------------------------------
// Modular WGSL Shader Builders (4 CAD Pipeline Passes)
// ---------------------------------------------------------------------------

const WGSL_SHARED_UNIFORMS = /* wgsl */ `
struct FrameUniforms {
  view_proj: mat4x4<f32>,
  normal_view_col0: vec4<f32>,
  normal_view_col1: vec4<f32>,
  normal_view_col2: vec4<f32>,
  section_plane_rte: vec4<f32>, // xyz = normal, w = rte_offset_mm
  params: vec4<f32>,            // x = sectionEnabled, y = edgeWidthPx, z = fogStrength, w = hatchSpacingPx
  viewport: vec4<f32>,          // xy = width, height; zw = 1/width, 1/height
  fog_color: vec4<f32>,
  cap_color: vec4<f32>,
  wboit_params: vec4<f32>,      // x = weightScale, y = revealageEpsilon, zw = reserved
  _pad0: vec4<f32>,
  _pad1: vec4<f32>,
};

struct InstanceUniforms {
  model_rte: mat4x4<f32>,
  normal_col0: vec4<f32>,
  normal_col1: vec4<f32>,
  normal_col2: vec4<f32>,
  base_color: vec4<f32>,
  back_color: vec4<f32>,
  highlight_color: vec4<f32>,
  edge_color: vec4<f32>,
  ids: vec4<u32>,            // x = instanceIdOneBased, y = selectedFaceSlot (0xffffffff=none), z = hoveredFaceSlot, w = flags
  edge_selection: vec4<u32>, // x = selectedEdgeSlot, y = hoveredEdgeSlot, z = wboitMode, w = reserved
  wboit_tint: vec4<f32>,     // rgb = tint, a = opacity
  _pad0: vec4<f32>,
  _pad1: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameUniforms;
@group(1) @binding(0) var<uniform> inst: InstanceUniforms;

fn normal_to_view(local_normal: vec3<f32>) -> vec3<f32> {
  let world_n = normalize(
    inst.normal_col0.xyz * local_normal.x +
    inst.normal_col1.xyz * local_normal.y +
    inst.normal_col2.xyz * local_normal.z
  );
  return normalize(
    frame.normal_view_col0.xyz * world_n.x +
    frame.normal_view_col1.xyz * world_n.y +
    frame.normal_view_col2.xyz * world_n.z
  );
}
`;

/**
 * Pass 1: Opaque CAD 3-point matte shaded solid pass + screen-space anti-aliased wide crease edges.
 * Consumes 32-byte `CadVertex` and 32-byte `CadEdgeSegment` buffers directly without expansion.
 */
export function buildOpaqueCadWgsl(): string {
  return `${WGSL_SHARED_UNIFORMS}

struct CadVertexInput {
  @location(0) position: vec3<f32>,
  @location(1) face_slot_id: u32,
  @location(2) normal: vec3<f32>,
  @location(3) flags: u32,
};

struct SolidVertexOut {
  @builtin(position) clip_pos: vec4<f32>,
  @location(0) view_normal: vec3<f32>,
  @location(1) rte_pos: vec3<f32>,
  @location(2) @interpolate(flat) face_slot_id: u32,
  @location(3) @interpolate(flat) flags: u32,
};

@vertex
fn vs_solid(in: CadVertexInput) -> SolidVertexOut {
  let rte_world = inst.model_rte * vec4<f32>(in.position, 1.0);
  var out: SolidVertexOut;
  out.clip_pos = frame.view_proj * rte_world;
  out.view_normal = normal_to_view(in.normal);
  out.rte_pos = rte_world.xyz;
  out.face_slot_id = in.face_slot_id;
  out.flags = in.flags;
  return out;
}

@fragment
fn fs_solid(in: SolidVertexOut, @builtin(front_facing) is_front: bool) -> @location(0) vec4<f32> {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }

  var N = normalize(in.view_normal);
  if (!is_front) {
    N = -N;
  }

  let V = vec3<f32>(0.0, 0.0, 1.0);
  let L1 = normalize(vec3<f32>(0.45, 0.62, 0.72));
  let L2 = normalize(vec3<f32>(-0.68, -0.32, 0.42));
  let L3 = normalize(vec3<f32>(0.0, -0.85, 0.35));

  let wrap1 = max(0.0, (dot(N, L1) + 0.22) / 1.22);
  let diff2 = max(dot(N, L2), 0.0);
  let bounce = max(dot(N, L3), 0.0);
  let H1 = normalize(L1 + V);
  let spec1 = pow(max(dot(N, H1), 0.0), 42.0);
  let rim = pow(1.0 - max(dot(N, V), 0.0), 2.6);

  var albedo = inst.base_color.rgb;
  let is_selected = in.face_slot_id == inst.ids.y;
  let is_hovered = in.face_slot_id == inst.ids.z;
  let is_exact_interface = (in.flags & 1u) != 0u;

  if (is_selected) {
    albedo = mix(albedo, inst.highlight_color.rgb, 0.68);
  } else if (is_hovered) {
    albedo = mix(albedo, vec3<f32>(0.36, 0.78, 0.98), 0.42);
  } else if (is_exact_interface) {
    albedo = mix(albedo, vec3<f32>(0.24, 0.62, 0.88), 0.18);
  }

  var lit: vec3<f32>;
  if (!is_front) {
    let lum = 0.26 + 0.48 * wrap1 + 0.14 * diff2;
    lit = inst.back_color.rgb * lum + vec3<f32>(0.75, 0.62, 0.52) * rim * 0.08;
  } else {
    let lum = 0.24 + 0.62 * wrap1 + 0.18 * diff2 + 0.06 * bounce;
    let specular = vec3<f32>(0.95, 0.98, 1.0) * spec1 * 0.18;
    let rim_glow = vec3<f32>(0.62, 0.78, 0.96) * rim * 0.18;
    lit = albedo * lum + specular + rim_glow;
  }

  let depth_norm = clamp(in.clip_pos.z, 0.0, 1.0);
  lit = mix(lit, frame.fog_color.rgb, depth_norm * frame.params.z);
  return vec4<f32>(clamp(lit, vec3<f32>(0.0), vec3<f32>(1.0)), inst.base_color.a);
}

struct EdgeSegmentInput {
  @location(0) point_a: vec3<f32>,
  @location(1) edge_slot_id: u32,
  @location(2) point_b: vec3<f32>,
  @location(3) style_flags: u32,
};

struct EdgeVertexOut {
  @builtin(position) clip_pos: vec4<f32>,
  @location(0) rte_pos: vec3<f32>,
  @location(1) edge_dist: f32,
  @location(2) @interpolate(flat) edge_slot_id: u32,
};

@vertex
fn vs_edge(in: EdgeSegmentInput, @builtin(vertex_index) vid: u32) -> EdgeVertexOut {
  let rte_a = inst.model_rte * vec4<f32>(in.point_a, 1.0);
  let rte_b = inst.model_rte * vec4<f32>(in.point_b, 1.0);
  let clip_a = frame.view_proj * rte_a;
  let clip_b = frame.view_proj * rte_b;

  let ndc_a = clip_a.xy / max(abs(clip_a.w), 1e-6);
  let ndc_b = clip_b.xy / max(abs(clip_b.w), 1e-6);
  let dir_px = (ndc_b - ndc_a) * frame.viewport.xy * 0.5;
  let len_px = max(length(dir_px), 1e-4);
  let normal_px = vec2<f32>(-dir_px.y, dir_px.x) / len_px;

  // 6-vertex quad strip: (0,1,2) and (2,1,3)
  var end_t = 0.0;
  var side = -1.0;
  if (vid == 1u || vid == 4u) { end_t = 0.0; side = 1.0; }
  else if (vid == 2u || vid == 3u) { end_t = 1.0; side = -1.0; }
  else if (vid == 5u) { end_t = 1.0; side = 1.0; }

  let is_hi = in.edge_slot_id == inst.edge_selection.x || in.edge_slot_id == inst.edge_selection.y;
  let half_width_px = select(frame.params.y, frame.params.y * 1.85, is_hi) * 0.5 + 0.75;

  var clip_p = mix(clip_a, clip_b, end_t);
  let offset_ndc = normal_px * side * half_width_px * 2.0 * frame.viewport.zw;
  clip_p.x = clip_p.x + offset_ndc.x * clip_p.w;
  clip_p.y = clip_p.y + offset_ndc.y * clip_p.w;
  // View-space depth bias preventing Z-fighting with coplanar faces
  clip_p.z = clip_p.z - 0.00045 * abs(clip_p.w);

  var out: EdgeVertexOut;
  out.clip_pos = clip_p;
  out.rte_pos = mix(rte_a.xyz, rte_b.xyz, end_t);
  out.edge_dist = side;
  out.edge_slot_id = in.edge_slot_id;
  return out;
}

@fragment
fn fs_edge(in: EdgeVertexOut) -> @location(0) vec4<f32> {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }
  let aa = 1.0 - smoothstep(0.6, 1.0, abs(in.edge_dist));
  var color = inst.edge_color.rgb;
  if (in.edge_slot_id == inst.edge_selection.x) {
    color = inst.highlight_color.rgb;
  } else if (in.edge_slot_id == inst.edge_selection.y) {
    color = vec3<f32>(0.36, 0.78, 0.98);
  }
  return vec4<f32>(color, inst.edge_color.a * aa);
}
`;
}

/**
 * Pass 2: Hardware two-sided stencil section-cap mask + 45° ISO mechanical cross-hatch fill.
 */
export function buildStencilSectionCapWgsl(): string {
  return `${WGSL_SHARED_UNIFORMS}

struct CapVertexInput {
  @location(0) position: vec3<f32>,
};

struct StencilOut {
  @builtin(position) clip_pos: vec4<f32>,
  @location(0) rte_pos: vec3<f32>,
};

@vertex
fn vs_stencil_mask(in: CapVertexInput) -> StencilOut {
  let rte_world = inst.model_rte * vec4<f32>(in.position, 1.0);
  var out: StencilOut;
  out.clip_pos = frame.view_proj * rte_world;
  out.rte_pos = rte_world.xyz;
  return out;
}

@fragment
fn fs_stencil_mask(in: StencilOut) {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }
}

struct FullscreenOut {
  @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_cap_hatch(@builtin(vertex_index) vid: u32) -> FullscreenOut {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  var out: FullscreenOut;
  out.clip_pos = vec4<f32>(positions[vid], 0.0, 1.0);
  return out;
}

@fragment
fn fs_cap_hatch(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
  // 45-degree ISO 128 mechanical cross-hatching in screen space
  let spacing = max(4.0, frame.params.w);
  let diag = (frag_coord.x + frag_coord.y) / spacing;
  let phase = fract(diag);
  let stripe = select(0.0, 1.0, phase < 0.18);
  let base_fill = frame.cap_color.rgb;
  let hatch_line = base_fill * 0.52;
  let color = mix(base_fill, hatch_line, stripe);
  return vec4<f32>(color, 1.0);
}
`;
}

/**
 * Pass 3: 1-Pixel GPU ID Picking pass writing `rgba32uint`
 * `(instanceIdOneBased, packedKindAndSlot, flags, bitcast<u32>(ndcDepth))`.
 */
export function buildGpuPickWgsl(): string {
  return `${WGSL_SHARED_UNIFORMS}

struct PickVertexInput {
  @location(0) position: vec3<f32>,
  @location(1) face_slot_id: u32,
  @location(2) normal: vec3<f32>,
  @location(3) flags: u32,
};

struct PickVertexOut {
  @builtin(position) clip_pos: vec4<f32>,
  @location(0) rte_pos: vec3<f32>,
  @location(1) @interpolate(flat) slot_id: u32,
  @location(2) @interpolate(flat) flags: u32,
};

@vertex
fn vs_pick_face(in: PickVertexInput) -> PickVertexOut {
  let rte_world = inst.model_rte * vec4<f32>(in.position, 1.0);
  var out: PickVertexOut;
  out.clip_pos = frame.view_proj * rte_world;
  out.rte_pos = rte_world.xyz;
  out.slot_id = in.face_slot_id & 0x7fffffffu;
  out.flags = in.flags;
  return out;
}

@fragment
fn fs_pick_face(in: PickVertexOut) -> @location(0) vec4<u32> {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }
  return vec4<u32>(
    inst.ids.x,
    in.slot_id,
    in.flags,
    bitcast<u32>(in.clip_pos.z),
  );
}

struct PickEdgeInput {
  @location(0) point_a: vec3<f32>,
  @location(1) edge_slot_id: u32,
  @location(2) point_b: vec3<f32>,
  @location(3) style_flags: u32,
};

@vertex
fn vs_pick_edge(in: PickEdgeInput, @builtin(vertex_index) vid: u32) -> PickVertexOut {
  let rte_a = inst.model_rte * vec4<f32>(in.point_a, 1.0);
  let rte_b = inst.model_rte * vec4<f32>(in.point_b, 1.0);
  let clip_a = frame.view_proj * rte_a;
  let clip_b = frame.view_proj * rte_b;

  let ndc_a = clip_a.xy / max(abs(clip_a.w), 1e-6);
  let ndc_b = clip_b.xy / max(abs(clip_b.w), 1e-6);
  let dir_px = (ndc_b - ndc_a) * frame.viewport.xy * 0.5;
  let len_px = max(length(dir_px), 1e-4);
  let normal_px = vec2<f32>(-dir_px.y, dir_px.x) / len_px;

  var end_t = 0.0;
  var side = -1.0;
  if (vid == 1u || vid == 4u) { end_t = 0.0; side = 1.0; }
  else if (vid == 2u || vid == 3u) { end_t = 1.0; side = -1.0; }
  else if (vid == 5u) { end_t = 1.0; side = 1.0; }

  // 4px pick halo for effortless edge clicking
  let half_width_px = max(3.5, frame.params.y * 2.0);
  var clip_p = mix(clip_a, clip_b, end_t);
  let offset_ndc = normal_px * side * half_width_px * 2.0 * frame.viewport.zw;
  clip_p.x = clip_p.x + offset_ndc.x * clip_p.w;
  clip_p.y = clip_p.y + offset_ndc.y * clip_p.w;
  clip_p.z = clip_p.z - 0.001 * abs(clip_p.w);

  var out: PickVertexOut;
  out.clip_pos = clip_p;
  out.rte_pos = mix(rte_a.xyz, rte_b.xyz, end_t);
  out.slot_id = (in.edge_slot_id & 0x7fffffffu) | 0x80000000u;
  out.flags = in.style_flags;
  return out;
}

@fragment
fn fs_pick_edge(in: PickVertexOut) -> @location(0) vec4<u32> {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }
  return vec4<u32>(
    inst.ids.x,
    in.slot_id,
    in.flags,
    bitcast<u32>(in.clip_pos.z),
  );
}
`;
}

/**
 * Pass 4: Weighted Blended Order-Independent Transparency (WBOIT) accum + composite pass
 * for AI `ChangePreview` translucent ghost/diff overlays without CPU triangle sorting.
 */
export function buildWboitOverlayWgsl(): string {
  return `${WGSL_SHARED_UNIFORMS}

struct WboitVertexInput {
  @location(0) position: vec3<f32>,
  @location(1) face_slot_id: u32,
  @location(2) normal: vec3<f32>,
  @location(3) flags: u32,
};

struct WboitVertexOut {
  @builtin(position) clip_pos: vec4<f32>,
  @location(0) view_normal: vec3<f32>,
  @location(1) rte_pos: vec3<f32>,
};

struct WboitFragOut {
  @location(0) accum: vec4<f32>,
  @location(1) revealage: f32,
};

fn wboit_weight(depth_ndc: f32, alpha: f32) -> f32 {
  let z = clamp(depth_ndc, 0.0, 1.0);
  let a = clamp(alpha, 0.0, 1.0);
  let alpha_term = pow(min(1.0, a * 10.0) + 0.01, 3.0);
  let depth_term = pow(1.0 - z * 0.9, 3.0);
  return clamp(alpha_term * 2500.0 * depth_term, 1e-2, 3e3);
}

@vertex
fn vs_wboit(in: WboitVertexInput) -> WboitVertexOut {
  let rte_world = inst.model_rte * vec4<f32>(in.position, 1.0);
  var out: WboitVertexOut;
  out.clip_pos = frame.view_proj * rte_world;
  out.view_normal = normal_to_view(in.normal);
  out.rte_pos = rte_world.xyz;
  return out;
}

@fragment
fn fs_wboit(in: WboitVertexOut, @builtin(front_facing) is_front: bool) -> WboitFragOut {
  if (frame.params.x > 0.5) {
    if (dot(in.rte_pos, frame.section_plane_rte.xyz) > frame.section_plane_rte.w) {
      discard;
    }
  }
  var N = normalize(in.view_normal);
  if (!is_front) {
    N = -N;
  }
  let V = vec3<f32>(0.0, 0.0, 1.0);
  let fresnel = pow(1.0 - max(dot(N, V), 0.0), 2.0);
  let shade = 0.45 + 0.45 * max(dot(N, normalize(vec3<f32>(0.45, 0.62, 0.72))), 0.0) + 0.25 * fresnel;
  let alpha = clamp(inst.wboit_tint.a * (0.75 + 0.45 * fresnel), 0.05, 0.92);
  let color = clamp(inst.wboit_tint.rgb * shade, vec3<f32>(0.0), vec3<f32>(1.0));
  let w = wboit_weight(in.clip_pos.z, alpha);

  var out: WboitFragOut;
  out.accum = vec4<f32>(color * alpha * w, alpha * w);
  out.revealage = alpha;
  return out;
}

@group(0) @binding(1) var accum_tex: texture_2d<f32>;
@group(0) @binding(2) var reveal_tex: texture_2d<f32>;

struct CompositeOut {
  @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_wboit_composite(@builtin(vertex_index) vid: u32) -> CompositeOut {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  var out: CompositeOut;
  out.clip_pos = vec4<f32>(positions[vid], 0.0, 1.0);
  return out;
}

@fragment
fn fs_wboit_composite(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
  let coords = vec2<i32>(frag_coord.xy);
  let accum = textureLoad(accum_tex, coords, 0);
  let revealage = textureLoad(reveal_tex, coords, 0).r;
  let eps = max(frame.wboit_params.y, 1e-5);
  let avg_color = accum.rgb / max(accum.a, eps);
  let out_alpha = clamp(1.0 - revealage, 0.0, 1.0);
  return vec4<f32>(avg_color, out_alpha);
}
`;
}

// ---------------------------------------------------------------------------
// Pure GPU Uniform Packing & Multi-Pass Frame Planner (Testable in Node)
// ---------------------------------------------------------------------------

const WBOIT_DEFAULT_COLORS: Record<WboitChangePreviewKind, readonly [number, number, number]> = {
  ghost: [0.52, 0.72, 0.94],
  added: [0.18, 0.82, 0.48],
  removed: [0.94, 0.32, 0.34],
  modified: [0.96, 0.72, 0.18],
};

/**
 * Packs `FrameUniforms` into a 256-byte `Float32Array` aligned to WebGPU uniform rules.
 * Converts the world-space section plane into camera-relative RTE coordinates in `f64`.
 */
export function packFrameUniforms(
  camera: Cms2CameraState,
  proj: WebGpuProjectionMatrices,
  state: Cms2FrameState = {},
): Float32Array {
  const buf = new Float32Array(FRAME_UNIFORM_BYTES / 4);
  // 0..15: view_proj (64B)
  buf.set(proj.viewProj, 0);
  // 16..27: normal_view_col0..2 (48B)
  buf.set(proj.normalViewCols, 16);

  // 28..31: section_plane_rte (16B): dot(worldPos, n) > offsetMm <=> dot(rtePos, n) > offsetMm - dot(center, n)
  const sp = state.sectionPlane;
  if (sp && sp.enabled) {
    const centerDot =
      camera.center[0] * sp.normal[0] +
      camera.center[1] * sp.normal[1] +
      camera.center[2] * sp.normal[2];
    buf[28] = sp.normal[0];
    buf[29] = sp.normal[1];
    buf[30] = sp.normal[2];
    buf[31] = Math.fround(sp.offsetMm - centerDot);
  }

  // 32..35: params (16B)
  buf[32] = sp && sp.enabled ? 1 : 0;
  buf[33] = state.edgeWidthPx ?? 1.5;
  buf[34] = state.fogStrength ?? 0.08;
  buf[35] = sp?.hatchSpacingPx ?? 8.0;

  // 36..39: viewport (16B)
  buf[36] = camera.width;
  buf[37] = camera.height;
  buf[38] = 1 / camera.width;
  buf[39] = 1 / camera.height;

  // 40..43: fog_color (16B)
  const bg = state.backgroundColor ?? [0.09, 0.11, 0.14, 1.0];
  buf[40] = bg[0];
  buf[41] = bg[1];
  buf[42] = bg[2];
  buf[43] = bg[3];

  // 44..47: cap_color (16B)
  buf[44] = 0.78;
  buf[45] = 0.82;
  buf[46] = 0.86;
  buf[47] = 1.0;

  // 48..51: wboit_params (16B)
  buf[48] = 1.0;
  buf[49] = 1e-5;
  buf[50] = 0.0;
  buf[51] = 0.0;

  return buf;
}

/**
 * Plans zero-copy prototype uploads, 256-byte-aligned RTE instance uniforms, and
 * ordered multi-pass draw descriptors for a `DecodedCompactScene`.
 */
export function planCms2RenderFrame(
  scene: DecodedCompactScene,
  camera: Cms2CameraState,
  state: Cms2FrameState = {},
  pickPixelCoord?: { readonly x: number; readonly y: number },
): Cms2RenderPlan | null {
  const proj = buildWebGpuProjectionMatrix(camera);
  if (!proj) return null;

  const prototypes: Cms2PrototypeUploadPlan[] = scene.prototypes.map((proto, index) => ({
    prototypeIndex: index,
    partName: proto.part_name,
    vertexCount: proto.vertexCount,
    indexCount: proto.indexCount,
    edgeSegmentCount: proto.edgeSegmentCount,
    vertexStrideBytes: CMS2_VERTEX_STRIDE_BYTES,
    edgeStrideBytes: CMS2_EDGE_STRIDE_BYTES,
    vertexBytes: proto.vertexBytes,
    indices: proto.indices,
    edgeBytes: proto.edgeBytes,
    zeroCopy:
      proto.vertexBytes.buffer === proto.vertexData.buffer &&
      proto.edgeBytes.buffer === proto.edgeData.buffer,
  }));

  const frameUniforms = packFrameUniforms(camera, proj, state);
  const instanceCount = scene.instances.length;
  const instanceUniforms = new ArrayBuffer(Math.max(1, instanceCount) * INSTANCE_UNIFORM_STRIDE_BYTES);
  const f32View = new Float32Array(instanceUniforms);
  const u32View = new Uint32Array(instanceUniforms);

  const wboitMap = new Map<number, WboitInstanceOverlay>();
  for (const overlay of state.wboitOverlays ?? []) {
    wboitMap.set(overlay.instanceIndex, overlay);
  }

  const opaqueInstanceIndices: number[] = [];
  const wboitInstanceIndices: number[] = [];
  let totalTriangles = 0;
  let totalEdgeSegments = 0;

  const explodeFactor = state.explodeFactor ?? 0;
  const baseColor = state.baseColor ?? [0.68, 0.74, 0.81, 1.0];
  const backColor = state.backColor ?? [0.42, 0.34, 0.30, 1.0];

  for (let i = 0; i < instanceCount; i += 1) {
    const inst = scene.instances[i];
    const proto = scene.prototypes[inst.prototype_index];
    if (!proto) continue;

    const explode = instanceExplodeOffset(i, explodeFactor);
    const rte = computeRteInstanceTransform(proto, inst, camera.center, explode);
    const wordBase = (i * INSTANCE_UNIFORM_STRIDE_BYTES) / 4;

    // 0..15: model_rte (64B)
    f32View.set(rte.modelRte, wordBase);
    // 16..27: normal_col0..2 (48B)
    f32View.set(rte.normalCols, wordBase + 16);
    // 28..31: base_color (16B)
    f32View.set(baseColor, wordBase + 28);
    // 32..35: back_color (16B)
    f32View.set(backColor, wordBase + 32);
    // 36..39: highlight_color (16B)
    f32View.set([0.98, 0.64, 0.16, 1.0], wordBase + 36);
    // 40..43: edge_color (16B)
    f32View.set([0.12, 0.16, 0.22, 1.0], wordBase + 40);

    // 44..47: ids (u32 x 4 = 16B)
    const isSelInst = state.selectedInstanceIndex === i;
    const isHovInst = state.hoveredInstanceIndex === i;
    u32View[wordBase + 44] = (i + 1) >>> 0;
    u32View[wordBase + 45] =
      isSelInst && state.selectedFaceSlot != null ? (state.selectedFaceSlot >>> 0) : 0xffff_ffff;
    u32View[wordBase + 46] =
      isHovInst && state.hoveredFaceSlot != null ? (state.hoveredFaceSlot >>> 0) : 0xffff_ffff;
    u32View[wordBase + 47] = 0;

    // 48..51: edge_selection (u32 x 4 = 16B)
    const wboit = wboitMap.get(i);
    u32View[wordBase + 48] =
      isSelInst && state.selectedEdgeSlot != null ? (state.selectedEdgeSlot >>> 0) : 0xffff_ffff;
    u32View[wordBase + 49] =
      isHovInst && state.hoveredEdgeSlot != null ? (state.hoveredEdgeSlot >>> 0) : 0xffff_ffff;
    u32View[wordBase + 50] = wboit ? 1 : 0;
    u32View[wordBase + 51] = 0;

    // 52..55: wboit_tint (16B)
    if (wboit) {
      const tint = wboit.color ?? WBOIT_DEFAULT_COLORS[wboit.kind];
      f32View[wordBase + 52] = tint[0];
      f32View[wordBase + 53] = tint[1];
      f32View[wordBase + 54] = tint[2];
      f32View[wordBase + 55] = Math.min(1, Math.max(0.05, wboit.opacity));
      wboitInstanceIndices.push(i);
    } else {
      opaqueInstanceIndices.push(i);
    }

    totalTriangles += Math.floor(proto.indexCount / 3);
    totalEdgeSegments += proto.edgeSegmentCount;
  }

  const passes: Cms2RenderPassDescriptor[] = [];
  let totalDrawCalls = 0;

  if (opaqueInstanceIndices.length > 0) {
    passes.push({
      kind: "opaque-solid",
      drawCalls: opaqueInstanceIndices.length,
      instanceIndices: opaqueInstanceIndices,
    });
    totalDrawCalls += opaqueInstanceIndices.length;
  }

  const stencilCapActive = Boolean(state.sectionPlane?.enabled && state.sectionPlane?.stencilCap && opaqueInstanceIndices.length > 0);
  if (stencilCapActive) {
    passes.push({
      kind: "stencil-cap-mask",
      drawCalls: opaqueInstanceIndices.length,
      instanceIndices: opaqueInstanceIndices,
    });
    passes.push({
      kind: "stencil-cap-hatch",
      drawCalls: 1,
      instanceIndices: [],
    });
    totalDrawCalls += opaqueInstanceIndices.length + 1;
  }

  if ((state.showEdges ?? true) && opaqueInstanceIndices.length > 0 && totalEdgeSegments > 0) {
    passes.push({
      kind: "opaque-edges",
      drawCalls: opaqueInstanceIndices.length,
      instanceIndices: opaqueInstanceIndices,
    });
    totalDrawCalls += opaqueInstanceIndices.length;
  }

  if (wboitInstanceIndices.length > 0) {
    passes.push({
      kind: "wboit-accum",
      drawCalls: wboitInstanceIndices.length,
      instanceIndices: wboitInstanceIndices,
    });
    passes.push({
      kind: "wboit-composite",
      drawCalls: 1,
      instanceIndices: [],
    });
    totalDrawCalls += wboitInstanceIndices.length + 1;
  }

  if (pickPixelCoord) {
    const clampedX = Math.max(0, Math.min(Math.floor(camera.width) - 1, Math.floor(pickPixelCoord.x)));
    const clampedY = Math.max(0, Math.min(Math.floor(camera.height) - 1, Math.floor(pickPixelCoord.y)));
    passes.push({
      kind: "gpu-pick-1px",
      drawCalls: instanceCount,
      instanceIndices: Array.from({ length: instanceCount }, (_, idx) => idx),
      scissorRect: { x: clampedX, y: clampedY, width: 1, height: 1 },
      readbackBytesPerRow: PICK_STAGING_BYTES_PER_ROW,
    });
    totalDrawCalls += instanceCount;
  }

  return {
    prototypes,
    frameUniforms,
    instanceUniforms,
    instanceStrideBytes: INSTANCE_UNIFORM_STRIDE_BYTES,
    instanceCount,
    passes,
    stats: {
      drawCalls: totalDrawCalls,
      triangles: totalTriangles,
      edgeSegments: totalEdgeSegments,
      wboitInstances: wboitInstanceIndices.length,
      stencilCapActive,
    },
  };
}

// ---------------------------------------------------------------------------
// Real WebGPU Multi-Pass Renderer Implementation
// ---------------------------------------------------------------------------

interface UploadedGpuPrototype {
  readonly vertexBuffer: WebGpuBufferLike;
  readonly indexBuffer: WebGpuBufferLike;
  readonly edgeBuffer: WebGpuBufferLike | null;
  readonly indexCount: number;
  readonly edgeSegmentCount: number;
}

export interface CreateCms2WebgpuRendererOptions {
  readonly navigatorGpu?: WebGpuNavigatorGpuLike;
  readonly onDeviceLost?: (reason: string) => void;
}

/**
 * Creates a live zero-copy `CMS2` WebGPU renderer bound to an `HTMLCanvasElement`.
 * Returns `null` cleanly if WebGPU is unavailable so callers can fall back to WebGL2.
 */
export async function createCms2WebgpuRenderer(
  canvas: HTMLCanvasElement,
  options: CreateCms2WebgpuRendererOptions = {},
): Promise<Cms2WebgpuRenderer | null> {
  const gpu =
    options.navigatorGpu ??
    (typeof navigator !== "undefined"
      ? (navigator as unknown as { readonly gpu?: WebGpuNavigatorGpuLike }).gpu
      : undefined);
  if (!gpu || typeof gpu.requestAdapter !== "function") {
    return null;
  }

  let adapter: WebGpuAdapterLike | null = null;
  let device: WebGpuDeviceLike | null = null;
  try {
    adapter = await gpu.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) return null;
    device = await adapter.requestDevice();
    if (!device) return null;
  } catch {
    return null;
  }

  const context = (canvas.getContext("webgpu") as unknown as WebGpuCanvasContextLike | null);
  if (!context) {
    device.destroy();
    return null;
  }

  const presentationFormat = gpu.getPreferredCanvasFormat?.() ?? "bgra8unorm";
  context.configure({
    device,
    format: presentationFormat,
    alphaMode: "premultiplied",
  });

  // Compile all 4 modular WGSL shader modules
  const opaqueModule = device.createShaderModule({ label: "cms2-opaque-cad", code: buildOpaqueCadWgsl() });
  const stencilCapModule = device.createShaderModule({ label: "cms2-stencil-cap", code: buildStencilSectionCapWgsl() });
  const pickModule = device.createShaderModule({ label: "cms2-gpu-pick", code: buildGpuPickWgsl() });
  const wboitModule = device.createShaderModule({ label: "cms2-wboit", code: buildWboitOverlayWgsl() });

  const frameBindGroupLayout = device.createBindGroupLayout({
    label: "cms2-frame-bgl",
    entries: [{ binding: 0, visibility: 0x3, buffer: { type: "uniform" } }],
  });
  const instanceBindGroupLayout = device.createBindGroupLayout({
    label: "cms2-instance-bgl",
    entries: [{ binding: 0, visibility: 0x3, buffer: { type: "uniform", hasDynamicOffset: true } }],
  });
  const compositeBindGroupLayout = device.createBindGroupLayout({
    label: "cms2-wboit-composite-bgl",
    entries: [
      { binding: 0, visibility: 0x3, buffer: { type: "uniform" } },
      { binding: 1, visibility: 0x2, texture: { sampleType: "unfilterable-float" } },
      { binding: 2, visibility: 0x2, texture: { sampleType: "unfilterable-float" } },
    ],
  });

  const sharedPipelineLayout = device.createPipelineLayout({
    bindGroupLayouts: [frameBindGroupLayout, instanceBindGroupLayout],
  });
  const compositePipelineLayout = device.createPipelineLayout({
    bindGroupLayouts: [compositeBindGroupLayout],
  });

  const cadVertexBufferLayout = {
    arrayStride: CMS2_VERTEX_STRIDE_BYTES,
    stepMode: "vertex",
    attributes: [
      { shaderLocation: 0, offset: 0, format: "float32x3" },
      { shaderLocation: 1, offset: 12, format: "uint32" },
      { shaderLocation: 2, offset: 16, format: "float32x3" },
      { shaderLocation: 3, offset: 28, format: "uint32" },
    ],
  };

  const edgeInstanceBufferLayout = {
    arrayStride: CMS2_EDGE_STRIDE_BYTES,
    stepMode: "instance",
    attributes: [
      { shaderLocation: 0, offset: 0, format: "float32x3" },
      { shaderLocation: 1, offset: 12, format: "uint32" },
      { shaderLocation: 2, offset: 16, format: "float32x3" },
      { shaderLocation: 3, offset: 28, format: "uint32" },
    ],
  };

  const solidPipeline = device.createRenderPipeline({
    label: "cms2-solid-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: opaqueModule, entryPoint: "vs_solid", buffers: [cadVertexBufferLayout] },
    fragment: { module: opaqueModule, entryPoint: "fs_solid", targets: [{ format: presentationFormat }] },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: { format: "depth24plus-stencil8", depthWriteEnabled: true, depthCompare: "less-equal" },
  });

  const edgePipeline = device.createRenderPipeline({
    label: "cms2-edge-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: opaqueModule, entryPoint: "vs_edge", buffers: [edgeInstanceBufferLayout] },
    fragment: {
      module: opaqueModule,
      entryPoint: "fs_edge",
      targets: [{
        format: presentationFormat,
        blend: {
          color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
          alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
        },
      }],
    },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: { format: "depth24plus-stencil8", depthWriteEnabled: false, depthCompare: "less-equal" },
  });

  const stencilMaskPipeline = device.createRenderPipeline({
    label: "cms2-stencil-mask-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: stencilCapModule, entryPoint: "vs_stencil_mask", buffers: [cadVertexBufferLayout] },
    fragment: { module: stencilCapModule, entryPoint: "fs_stencil_mask", targets: [] },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: {
      format: "depth24plus-stencil8",
      depthWriteEnabled: false,
      depthCompare: "always",
      stencilFront: { compare: "always", passOp: "decrement-wrap", failOp: "keep", depthFailOp: "keep" },
      stencilBack: { compare: "always", passOp: "increment-wrap", failOp: "keep", depthFailOp: "keep" },
    },
  });

  const capHatchPipeline = device.createRenderPipeline({
    label: "cms2-cap-hatch-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: stencilCapModule, entryPoint: "vs_cap_hatch", buffers: [] },
    fragment: { module: stencilCapModule, entryPoint: "fs_cap_hatch", targets: [{ format: presentationFormat }] },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: {
      format: "depth24plus-stencil8",
      depthWriteEnabled: false,
      depthCompare: "always",
      stencilFront: { compare: "not-equal", passOp: "keep", failOp: "keep", depthFailOp: "keep" },
      stencilBack: { compare: "not-equal", passOp: "keep", failOp: "keep", depthFailOp: "keep" },
    },
  });

  const pickFacePipeline = device.createRenderPipeline({
    label: "cms2-pick-face-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: pickModule, entryPoint: "vs_pick_face", buffers: [cadVertexBufferLayout] },
    fragment: { module: pickModule, entryPoint: "fs_pick_face", targets: [{ format: "rgba32uint" }] },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: { format: "depth24plus-stencil8", depthWriteEnabled: true, depthCompare: "less-equal" },
  });

  const pickEdgePipeline = device.createRenderPipeline({
    label: "cms2-pick-edge-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: pickModule, entryPoint: "vs_pick_edge", buffers: [edgeInstanceBufferLayout] },
    fragment: { module: pickModule, entryPoint: "fs_pick_edge", targets: [{ format: "rgba32uint" }] },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: { format: "depth24plus-stencil8", depthWriteEnabled: true, depthCompare: "less-equal" },
  });

  const wboitAccumPipeline = device.createRenderPipeline({
    label: "cms2-wboit-accum-pipeline",
    layout: sharedPipelineLayout,
    vertex: { module: wboitModule, entryPoint: "vs_wboit", buffers: [cadVertexBufferLayout] },
    fragment: {
      module: wboitModule,
      entryPoint: "fs_wboit",
      targets: [
        {
          format: "rgba16float",
          blend: {
            color: { srcFactor: "one", dstFactor: "one", operation: "add" },
            alpha: { srcFactor: "one", dstFactor: "one", operation: "add" },
          },
        },
        {
          format: "r8unorm",
          blend: {
            color: { srcFactor: "zero", dstFactor: "one-minus-src", operation: "add" },
            alpha: { srcFactor: "zero", dstFactor: "one-minus-src", operation: "add" },
          },
        },
      ],
    },
    primitive: { topology: "triangle-list", cullMode: "none" },
    depthStencil: { format: "depth24plus-stencil8", depthWriteEnabled: false, depthCompare: "less-equal" },
  });

  const wboitCompositePipeline = device.createRenderPipeline({
    label: "cms2-wboit-composite-pipeline",
    layout: compositePipelineLayout,
    vertex: { module: wboitModule, entryPoint: "vs_wboit_composite", buffers: [] },
    fragment: {
      module: wboitModule,
      entryPoint: "fs_wboit_composite",
      targets: [{
        format: presentationFormat,
        blend: {
          color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
          alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
        },
      }],
    },
    primitive: { topology: "triangle-list", cullMode: "none" },
  });

  const frameUniformBuffer = device.createBuffer({
    label: "cms2-frame-uniforms",
    size: FRAME_UNIFORM_BYTES,
    usage: GPU_BUFFER_USAGE.UNIFORM | GPU_BUFFER_USAGE.COPY_DST,
  });

  let instanceUniformBuffer = device.createBuffer({
    label: "cms2-instance-uniforms",
    size: INSTANCE_UNIFORM_STRIDE_BYTES,
    usage: GPU_BUFFER_USAGE.UNIFORM | GPU_BUFFER_USAGE.COPY_DST,
  });
  let instanceUniformCapacity = INSTANCE_UNIFORM_STRIDE_BYTES;

  const pickStagingBuffer = device.createBuffer({
    label: "cms2-pick-staging",
    size: PICK_STAGING_BYTES_PER_ROW,
    usage: GPU_BUFFER_USAGE.COPY_DST | GPU_BUFFER_USAGE.MAP_READ,
  });

  const frameBindGroup = device.createBindGroup({
    layout: frameBindGroupLayout,
    entries: [{ binding: 0, resource: { buffer: frameUniformBuffer } }],
  });

  let instanceBindGroup = device.createBindGroup({
    layout: instanceBindGroupLayout,
    entries: [{ binding: 0, resource: { buffer: instanceUniformBuffer, size: INSTANCE_UNIFORM_STRIDE_BYTES } }],
  });

  let currentScene: DecodedCompactScene | null = null;
  let uploadedPrototypes: UploadedGpuPrototype[] = [];
  let depthStencilTex: WebGpuTextureLike | null = null;
  let pickTex: WebGpuTextureLike | null = null;
  let wboitAccumTex: WebGpuTextureLike | null = null;
  let wboitRevealTex: WebGpuTextureLike | null = null;
  let wboitCompositeBindGroup: unknown = null;
  let targetWidth = 0;
  let targetHeight = 0;
  let disposed = false;
  let deviceLost = false;

  device.lost?.then((info) => {
    deviceLost = true;
    options.onDeviceLost?.(info?.message ?? info?.reason ?? "WebGPU device lost");
  });

  function ensureRenderTargets(width: number, height: number): void {
    const w = Math.max(1, Math.floor(width));
    const h = Math.max(1, Math.floor(height));
    if (w === targetWidth && h === targetHeight && depthStencilTex && pickTex && wboitAccumTex && wboitRevealTex) {
      return;
    }
    depthStencilTex?.destroy();
    pickTex?.destroy();
    wboitAccumTex?.destroy();
    wboitRevealTex?.destroy();

    depthStencilTex = device!.createTexture({
      label: "cms2-depth-stencil",
      size: [w, h, 1],
      format: "depth24plus-stencil8",
      usage: GPU_TEXTURE_USAGE.RENDER_ATTACHMENT,
    });
    pickTex = device!.createTexture({
      label: "cms2-pick-id",
      size: [w, h, 1],
      format: "rgba32uint",
      usage: GPU_TEXTURE_USAGE.RENDER_ATTACHMENT | GPU_TEXTURE_USAGE.COPY_SRC,
    });
    wboitAccumTex = device!.createTexture({
      label: "cms2-wboit-accum",
      size: [w, h, 1],
      format: "rgba16float",
      usage: GPU_TEXTURE_USAGE.RENDER_ATTACHMENT | GPU_TEXTURE_USAGE.TEXTURE_BINDING,
    });
    wboitRevealTex = device!.createTexture({
      label: "cms2-wboit-reveal",
      size: [w, h, 1],
      format: "r8unorm",
      usage: GPU_TEXTURE_USAGE.RENDER_ATTACHMENT | GPU_TEXTURE_USAGE.TEXTURE_BINDING,
    });
    wboitCompositeBindGroup = device!.createBindGroup({
      layout: compositeBindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: frameUniformBuffer } },
        { binding: 1, resource: wboitAccumTex.createView() },
        { binding: 2, resource: wboitRevealTex.createView() },
      ],
    });
    targetWidth = w;
    targetHeight = h;
  }

  function ensureInstanceBufferCapacity(requiredBytes: number): void {
    if (requiredBytes <= instanceUniformCapacity) return;
    instanceUniformBuffer.destroy();
    instanceUniformCapacity = alignTo(requiredBytes, WEBGPU_UNIFORM_ALIGN_BYTES);
    instanceUniformBuffer = device!.createBuffer({
      label: "cms2-instance-uniforms",
      size: instanceUniformCapacity,
      usage: GPU_BUFFER_USAGE.UNIFORM | GPU_BUFFER_USAGE.COPY_DST,
    });
    instanceBindGroup = device!.createBindGroup({
      layout: instanceBindGroupLayout,
      entries: [{ binding: 0, resource: { buffer: instanceUniformBuffer, size: INSTANCE_UNIFORM_STRIDE_BYTES } }],
    });
  }

  function releasePrototypes(): void {
    for (const proto of uploadedPrototypes) {
      proto.vertexBuffer.destroy();
      proto.indexBuffer.destroy();
      proto.edgeBuffer?.destroy();
    }
    uploadedPrototypes = [];
  }

  return {
    backend: "webgpu",

    updateScene(scene: DecodedCompactScene): boolean {
      if (disposed || deviceLost) return false;
      releasePrototypes();
      currentScene = scene;

      for (const proto of scene.prototypes) {
        const vSize = Math.max(4, alignTo(proto.vertexBytes.byteLength, 4));
        const iSize = Math.max(4, alignTo(proto.indices.byteLength, 4));
        const vertexBuffer = device!.createBuffer({
          label: `cms2-vb-${proto.part_name}`,
          size: vSize,
          usage: GPU_BUFFER_USAGE.VERTEX | GPU_BUFFER_USAGE.COPY_DST,
        });
        if (proto.vertexBytes.byteLength > 0) {
          device!.queue.writeBuffer(vertexBuffer, 0, proto.vertexBytes);
        }

        const indexBuffer = device!.createBuffer({
          label: `cms2-ib-${proto.part_name}`,
          size: iSize,
          usage: GPU_BUFFER_USAGE.INDEX | GPU_BUFFER_USAGE.COPY_DST,
        });
        if (proto.indices.byteLength > 0) {
          device!.queue.writeBuffer(indexBuffer, 0, proto.indices);
        }

        let edgeBuffer: WebGpuBufferLike | null = null;
        if (proto.edgeBytes.byteLength > 0) {
          const eSize = alignTo(proto.edgeBytes.byteLength, 4);
          edgeBuffer = device!.createBuffer({
            label: `cms2-eb-${proto.part_name}`,
            size: eSize,
            usage: GPU_BUFFER_USAGE.VERTEX | GPU_BUFFER_USAGE.COPY_DST,
          });
          device!.queue.writeBuffer(edgeBuffer, 0, proto.edgeBytes);
        }

        uploadedPrototypes.push({
          vertexBuffer,
          indexBuffer,
          edgeBuffer,
          indexCount: proto.indexCount,
          edgeSegmentCount: proto.edgeSegmentCount,
        });
      }
      return true;
    },

    render(camera: Cms2CameraState, state: Cms2FrameState = {}): Cms2RenderPlan | null {
      if (disposed || deviceLost || !currentScene) return null;
      const plan = planCms2RenderFrame(currentScene, camera, state);
      if (!plan) return null;

      ensureRenderTargets(camera.width, camera.height);
      ensureInstanceBufferCapacity(plan.instanceUniforms.byteLength);

      device!.queue.writeBuffer(frameUniformBuffer, 0, plan.frameUniforms);
      device!.queue.writeBuffer(instanceUniformBuffer, 0, plan.instanceUniforms);

      const colorView = context.getCurrentTexture().createView();
      const depthView = depthStencilTex!.createView();
      const bg = state.backgroundColor ?? [0.09, 0.11, 0.14, 1.0];
      const encoder = device!.createCommandEncoder({ label: "cms2-frame-encoder" });

      // Pass 1: Opaque CAD Solid + Edges
      const mainPass = encoder.beginRenderPass({
        colorAttachments: [{
          view: colorView,
          clearValue: { r: bg[0], g: bg[1], b: bg[2], a: bg[3] },
          loadOp: "clear",
          storeOp: "store",
        }],
        depthStencilAttachment: {
          view: depthView,
          depthClearValue: 1.0,
          depthLoadOp: "clear",
          depthStoreOp: "store",
          stencilClearValue: 0,
          stencilLoadOp: "clear",
          stencilStoreOp: "store",
        },
      });

      const opaquePass = plan.passes.find((p) => p.kind === "opaque-solid");
      if (opaquePass) {
        mainPass.setPipeline(solidPipeline);
        mainPass.setBindGroup(0, frameBindGroup);
        for (const instIdx of opaquePass.instanceIndices) {
          const inst = currentScene.instances[instIdx];
          const gpuProto = uploadedPrototypes[inst.prototype_index];
          if (!gpuProto || gpuProto.indexCount === 0) continue;
          mainPass.setBindGroup(1, instanceBindGroup, [instIdx * INSTANCE_UNIFORM_STRIDE_BYTES]);
          mainPass.setVertexBuffer(0, gpuProto.vertexBuffer);
          mainPass.setIndexBuffer(gpuProto.indexBuffer, "uint32");
          mainPass.drawIndexed(gpuProto.indexCount);
        }
      }

      // Pass 2: Hardware Stencil Section-Cap Mask + 45° ISO Hatch
      if (plan.stats.stencilCapActive && opaquePass) {
        mainPass.setPipeline(stencilMaskPipeline);
        mainPass.setBindGroup(0, frameBindGroup);
        for (const instIdx of opaquePass.instanceIndices) {
          const inst = currentScene.instances[instIdx];
          const gpuProto = uploadedPrototypes[inst.prototype_index];
          if (!gpuProto || gpuProto.indexCount === 0) continue;
          mainPass.setBindGroup(1, instanceBindGroup, [instIdx * INSTANCE_UNIFORM_STRIDE_BYTES]);
          mainPass.setVertexBuffer(0, gpuProto.vertexBuffer);
          mainPass.setIndexBuffer(gpuProto.indexBuffer, "uint32");
          mainPass.drawIndexed(gpuProto.indexCount);
        }
        mainPass.setPipeline(capHatchPipeline);
        mainPass.setStencilReference?.(0);
        mainPass.setBindGroup(0, frameBindGroup);
        mainPass.setBindGroup(1, instanceBindGroup, [0]);
        mainPass.draw(3);
      }

      const edgePass = plan.passes.find((p) => p.kind === "opaque-edges");
      if (edgePass) {
        mainPass.setPipeline(edgePipeline);
        mainPass.setBindGroup(0, frameBindGroup);
        for (const instIdx of edgePass.instanceIndices) {
          const inst = currentScene.instances[instIdx];
          const gpuProto = uploadedPrototypes[inst.prototype_index];
          if (!gpuProto?.edgeBuffer || gpuProto.edgeSegmentCount === 0) continue;
          mainPass.setBindGroup(1, instanceBindGroup, [instIdx * INSTANCE_UNIFORM_STRIDE_BYTES]);
          mainPass.setVertexBuffer(0, gpuProto.edgeBuffer);
          mainPass.draw(6, gpuProto.edgeSegmentCount);
        }
      }
      mainPass.end();

      // Pass 4: WBOIT Translucent Change-Preview Overlay
      const wboitPass = plan.passes.find((p) => p.kind === "wboit-accum");
      if (wboitPass && wboitAccumTex && wboitRevealTex) {
        const accumPass = encoder.beginRenderPass({
          colorAttachments: [
            { view: wboitAccumTex.createView(), clearValue: { r: 0, g: 0, b: 0, a: 0 }, loadOp: "clear", storeOp: "store" },
            { view: wboitRevealTex.createView(), clearValue: { r: 1, g: 1, b: 1, a: 1 }, loadOp: "clear", storeOp: "store" },
          ],
          depthStencilAttachment: {
            view: depthView,
            depthLoadOp: "load",
            depthStoreOp: "store",
            stencilLoadOp: "load",
            stencilStoreOp: "store",
          },
        });
        accumPass.setPipeline(wboitAccumPipeline);
        accumPass.setBindGroup(0, frameBindGroup);
        for (const instIdx of wboitPass.instanceIndices) {
          const inst = currentScene.instances[instIdx];
          const gpuProto = uploadedPrototypes[inst.prototype_index];
          if (!gpuProto || gpuProto.indexCount === 0) continue;
          accumPass.setBindGroup(1, instanceBindGroup, [instIdx * INSTANCE_UNIFORM_STRIDE_BYTES]);
          accumPass.setVertexBuffer(0, gpuProto.vertexBuffer);
          accumPass.setIndexBuffer(gpuProto.indexBuffer, "uint32");
          accumPass.drawIndexed(gpuProto.indexCount);
        }
        accumPass.end();

        const compPass = encoder.beginRenderPass({
          colorAttachments: [{ view: colorView, loadOp: "load", storeOp: "store" }],
        });
        compPass.setPipeline(wboitCompositePipeline);
        compPass.setBindGroup(0, wboitCompositeBindGroup);
        compPass.draw(3);
        compPass.end();
      }

      device!.queue.submit([encoder.finish()]);
      return plan;
    },

    async pickPixel(
      camera: Cms2CameraState,
      pixelX: number,
      pixelY: number,
      state: Cms2FrameState = {},
    ): Promise<Cms2PickResult | null> {
      if (!currentScene) return null;
      if (disposed || deviceLost || !pickStagingBuffer.mapAsync || !pickStagingBuffer.getMappedRange) {
        return pickCompactScenePixel(currentScene, camera, pixelX, pixelY, {
          sectionPlane: state.sectionPlane,
          explodeFactor: state.explodeFactor,
        });
      }

      const plan = planCms2RenderFrame(currentScene, camera, state, { x: pixelX, y: pixelY });
      if (!plan) return null;
      const pickDesc = plan.passes.find((p) => p.kind === "gpu-pick-1px");
      if (!pickDesc?.scissorRect) return null;

      ensureRenderTargets(camera.width, camera.height);
      ensureInstanceBufferCapacity(plan.instanceUniforms.byteLength);

      device!.queue.writeBuffer(frameUniformBuffer, 0, plan.frameUniforms);
      device!.queue.writeBuffer(instanceUniformBuffer, 0, plan.instanceUniforms);

      const encoder = device!.createCommandEncoder({ label: "cms2-pick-encoder" });
      const pass = encoder.beginRenderPass({
        colorAttachments: [{
          view: pickTex!.createView(),
          clearValue: { r: 0, g: 0, b: 0, a: 0 },
          loadOp: "clear",
          storeOp: "store",
        }],
        depthStencilAttachment: {
          view: depthStencilTex!.createView(),
          depthClearValue: 1.0,
          depthLoadOp: "clear",
          depthStoreOp: "store",
          stencilClearValue: 0,
          stencilLoadOp: "clear",
          stencilStoreOp: "store",
        },
      });
      const { x, y, width, height } = pickDesc.scissorRect;
      pass.setScissorRect?.(x, y, width, height);

      pass.setPipeline(pickFacePipeline);
      pass.setBindGroup(0, frameBindGroup);
      for (let i = 0; i < currentScene.instances.length; i += 1) {
        const inst = currentScene.instances[i];
        const gpuProto = uploadedPrototypes[inst.prototype_index];
        if (!gpuProto || gpuProto.indexCount === 0) continue;
        pass.setBindGroup(1, instanceBindGroup, [i * INSTANCE_UNIFORM_STRIDE_BYTES]);
        pass.setVertexBuffer(0, gpuProto.vertexBuffer);
        pass.setIndexBuffer(gpuProto.indexBuffer, "uint32");
        pass.drawIndexed(gpuProto.indexCount);
      }

      if (state.showEdges ?? true) {
        pass.setPipeline(pickEdgePipeline);
        pass.setBindGroup(0, frameBindGroup);
        for (let i = 0; i < currentScene.instances.length; i += 1) {
          const inst = currentScene.instances[i];
          const gpuProto = uploadedPrototypes[inst.prototype_index];
          if (!gpuProto?.edgeBuffer || gpuProto.edgeSegmentCount === 0) continue;
          pass.setBindGroup(1, instanceBindGroup, [i * INSTANCE_UNIFORM_STRIDE_BYTES]);
          pass.setVertexBuffer(0, gpuProto.edgeBuffer);
          pass.draw(6, gpuProto.edgeSegmentCount);
        }
      }
      pass.end();

      encoder.copyTextureToBuffer(
        { texture: pickTex!, origin: [x, y, 0] },
        { buffer: pickStagingBuffer, bytesPerRow: PICK_STAGING_BYTES_PER_ROW },
        [1, 1, 1],
      );
      device!.queue.submit([encoder.finish()]);

      await pickStagingBuffer.mapAsync(GPU_MAP_MODE_READ, 0, PICK_TEXEL_BYTES);
      const mapped = new Uint32Array(pickStagingBuffer.getMappedRange(0, PICK_TEXEL_BYTES).slice(0));
      pickStagingBuffer.unmap?.();

      return resolvePickTexel(currentScene, mapped);
    },

    dispose(): void {
      if (disposed) return;
      disposed = true;
      releasePrototypes();
      depthStencilTex?.destroy();
      pickTex?.destroy();
      wboitAccumTex?.destroy();
      wboitRevealTex?.destroy();
      frameUniformBuffer.destroy();
      instanceUniformBuffer.destroy();
      pickStagingBuffer.destroy();
      context.unconfigure?.();
      device?.destroy();
      currentScene = null;
    },
  };
}
