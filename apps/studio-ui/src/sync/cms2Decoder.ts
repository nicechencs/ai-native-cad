/**
 * Zero-copy typed-array decoder for the Rust `cad-render` `CMS2` binary compact scene format
 * and its `CM2E` IPC transport envelope, plus a lossless `f64` RTE adapter into `SceneMeshPayload`.
 */

import type {
  AssemblyComponentInfo,
  CurveType,
  DiagnosticsReport,
  EditHandle,
  FaceMesh,
  HoleOffsetHandle,
  ModelKind,
  SceneMeshPayload,
  SourceSpan,
  SurfaceType,
  TopoEdge,
  Vec3,
} from "../types/scene.ts";

export const CMS2_MAGIC_U32 = 0x434d5332; // "CMS2" big-endian
export const CMS2_VERSION = 1;
export const CMS2_HEADER_BYTES = 32;
export const CMS2_DIRECTORY_BYTES = 32;
export const CMS2_VERTEX_STRIDE_BYTES = 32;
export const CMS2_EDGE_STRIDE_BYTES = 32;

export const CMS2_ENVELOPE_MAGIC_U32 = 0x434d3245; // "CM2E" big-endian
export const CMS2_ENVELOPE_VERSION = 1;
export const CMS2_ENVELOPE_HEADER_BYTES = 32;

const nativeLittleEndian = new Uint8Array(Uint32Array.of(1).buffer)[0] === 1;
const utf8Decoder = new TextDecoder("utf-8", { fatal: true });

export interface Cms2FaceRecord {
  readonly face_id: string;
  readonly surface_type: SurfaceType | (string & {});
  readonly exact_interface: boolean;
  readonly is_exact_analytic: boolean;
  readonly plane_origin_mm: Vec3;
  readonly plane_normal: Vec3;
  readonly axis_origin_mm: Vec3;
  readonly axis_dir: Vec3;
  readonly radius_mm: number;
  readonly provenance_tags: readonly string[];
  readonly ast_span: SourceSpan;
  readonly vertex_begin: number;
  readonly vertex_count: number;
  readonly index_begin: number;
  readonly index_count: number;
}

export interface Cms2EdgeRecord {
  readonly edge_id: string;
  readonly curve_type: CurveType | (string & {});
  readonly provenance_tags: readonly string[];
  readonly ast_span: SourceSpan;
  readonly segment_begin: number;
  readonly segment_count: number;
}

export interface Cms2SceneInstance {
  readonly prototype_index: number;
  readonly component_id: string | null;
  readonly origin_mm: Vec3;
  readonly rotation_deg: Vec3;
}

export interface DecodedPartPrototype {
  readonly part_name: string;
  readonly anchor_mm: Vec3;
  readonly vertexCount: number;
  readonly indexCount: number;
  readonly edgeSegmentCount: number;
  /** Interleaved 32-byte CadVertex view: [px, py, pz, face_slot_bits, nx, ny, nz, flags_bits]. */
  readonly vertexData: Float32Array;
  /** Same underlying memory as `vertexData` for zero-copy u32 slot/flags reads. */
  readonly vertexWords: Uint32Array;
  /** Raw byte slice ready for direct GPU upload (`queue.writeBuffer` / `gl.bufferData`). */
  readonly vertexBytes: Uint8Array;
  /** Zero-copy triangle index buffer into this prototype's vertices. */
  readonly indices: Uint32Array;
  /** Interleaved 32-byte CadEdgeSegment view: [ax, ay, az, edge_slot_bits, bx, by, bz, style_bits]. */
  readonly edgeData: Float32Array;
  /** Same underlying memory as `edgeData` for zero-copy u32 slot/style reads. */
  readonly edgeWords: Uint32Array;
  /** Raw byte slice ready for direct GPU upload. */
  readonly edgeBytes: Uint8Array;
  readonly face_table: readonly Cms2FaceRecord[];
  readonly edge_table: readonly Cms2EdgeRecord[];
}

export interface DecodedCompactScene {
  readonly part_name: string;
  readonly kind: ModelKind;
  readonly units: "mm";
  readonly prototypes: readonly DecodedPartPrototype[];
  readonly instances: readonly Cms2SceneInstance[];
}

export interface Cms2ArtifactMetadata {
  readonly diagnostics: DiagnosticsReport;
  readonly elapsed_ms: number;
  readonly trace_id?: string;
  readonly format_version?: "0.1";
  readonly edit_handles?: readonly EditHandle[];
  readonly hole_offsets?: readonly HoleOffsetHandle[];
  readonly components?: readonly AssemblyComponentInfo[];
}

export interface DecodedCms2Envelope<TMetadata = Cms2ArtifactMetadata> {
  readonly metadata: TMetadata;
  readonly scene: DecodedCompactScene;
}

export interface ResolvedFaceRecord {
  readonly instanceIndex: number;
  readonly prototypeIndex: number;
  readonly faceSlotId: number;
  readonly instance: Cms2SceneInstance;
  readonly prototype: DecodedPartPrototype;
  readonly face: Cms2FaceRecord;
  readonly planeOriginWorldMm: [number, number, number];
  readonly planeNormalWorld: [number, number, number];
  readonly axisOriginWorldMm: [number, number, number];
  readonly axisDirWorld: [number, number, number];
}

export interface ResolvedEdgeRecord {
  readonly instanceIndex: number;
  readonly prototypeIndex: number;
  readonly edgeSlotId: number;
  readonly instance: Cms2SceneInstance;
  readonly prototype: DecodedPartPrototype;
  readonly edge: Cms2EdgeRecord;
}

export interface CompactSceneToMeshOptions {
  readonly diagnostics?: DiagnosticsReport;
  readonly edit_handles?: readonly EditHandle[];
  readonly hole_offsets?: readonly HoleOffsetHandle[];
  readonly components?: readonly AssemblyComponentInfo[];
}

interface RawCms2Meta {
  readonly part_name: string;
  readonly kind: string;
  readonly units: string;
  readonly prototypes: readonly {
    readonly part_name: string;
    readonly anchor_mm: Vec3;
    readonly face_table: readonly Cms2FaceRecord[];
    readonly edge_table: readonly Cms2EdgeRecord[];
  }[];
  readonly instances: readonly Cms2SceneInstance[];
}

type Mat3 = readonly [
  number, number, number,
  number, number, number,
  number, number, number,
];

function normalizeBufferInput(
  input: ArrayBuffer | ArrayBufferView | readonly number[],
  byteOffset?: number,
  byteLength?: number,
): { readonly buffer: ArrayBuffer; readonly offset: number; readonly length: number } {
  if (Array.isArray(input)) {
    const bytes = Uint8Array.from(input);
    return { buffer: bytes.buffer, offset: 0, length: bytes.byteLength };
  }
  if (ArrayBuffer.isView(input)) {
    if (!(input.buffer instanceof ArrayBuffer)) {
      const copy = new Uint8Array(input.byteLength);
      copy.set(new Uint8Array(input.buffer, input.byteOffset, input.byteLength));
      return { buffer: copy.buffer, offset: 0, length: copy.byteLength };
    }
    const baseOffset = input.byteOffset + (byteOffset ?? 0);
    const length = byteLength ?? input.byteLength - (byteOffset ?? 0);
    if (baseOffset < input.byteOffset || length < 0 || baseOffset + length > input.byteOffset + input.byteLength) {
      throw new Error("CMS2 buffer view slice is out of bounds");
    }
    return { buffer: input.buffer, offset: baseOffset, length };
  }
  const ab = input as ArrayBuffer;
  const offset = byteOffset ?? 0;
  const length = byteLength ?? ab.byteLength - offset;
  if (offset < 0 || length < 0 || offset + length > ab.byteLength) {
    throw new Error("CMS2 buffer slice is out of bounds");
  }
  return { buffer: ab, offset, length };
}

function viewFloat32(buffer: ArrayBuffer, byteOffset: number, count: number): Float32Array {
  if (count === 0) return new Float32Array(buffer, byteOffset, 0);
  if (nativeLittleEndian && byteOffset % 4 === 0) {
    return new Float32Array(buffer, byteOffset, count);
  }
  const view = new DataView(buffer, byteOffset, count * 4);
  const out = new Float32Array(count);
  for (let i = 0; i < count; i += 1) {
    out[i] = view.getFloat32(i * 4, true);
  }
  return out;
}

function viewUint32(buffer: ArrayBuffer, byteOffset: number, count: number): Uint32Array {
  if (count === 0) return new Uint32Array(buffer, byteOffset, 0);
  if (nativeLittleEndian && byteOffset % 4 === 0) {
    return new Uint32Array(buffer, byteOffset, count);
  }
  const view = new DataView(buffer, byteOffset, count * 4);
  const out = new Uint32Array(count);
  for (let i = 0; i < count; i += 1) {
    out[i] = view.getUint32(i * 4, true);
  }
  return out;
}

function checkRange(offset: number, length: number, blobStart: number, jsonOffset: number): void {
  if (length === 0) return;
  if (offset < blobStart || offset + length > jsonOffset) {
    throw new Error("CMS2 binary region is outside the payload");
  }
}

function checkFaceSpans(faces: readonly Cms2FaceRecord[], vertexCount: number, indexCount: number): void {
  let vertexCursor = 0;
  let indexCursor = 0;
  for (const face of faces) {
    if (face.vertex_begin !== vertexCursor || face.index_begin !== indexCursor) {
      throw new Error("CMS2 face ranges are not packed");
    }
    vertexCursor += face.vertex_count;
    indexCursor += face.index_count;
  }
  if (vertexCursor !== vertexCount || indexCursor !== indexCount) {
    throw new Error("CMS2 face ranges do not cover the mesh");
  }
}

function checkEdgeSpans(edges: readonly Cms2EdgeRecord[], segmentCount: number): void {
  let cursor = 0;
  for (const edge of edges) {
    if (edge.segment_begin !== cursor) {
      throw new Error("CMS2 edge ranges are not packed");
    }
    cursor += edge.segment_count;
  }
  if (cursor !== segmentCount) {
    throw new Error("CMS2 edge ranges do not cover the segments");
  }
}

/**
 * Decodes a raw `CMS2` binary payload into zero-copy typed-array views backed by `buffer`.
 */
export function decodeCms2Binary(
  input: ArrayBuffer | ArrayBufferView | readonly number[],
  byteOffset?: number,
  byteLength?: number,
): DecodedCompactScene {
  const { buffer, offset: baseOffset, length } = normalizeBufferInput(input, byteOffset, byteLength);
  if (length < CMS2_HEADER_BYTES) {
    throw new Error("Truncated CMS2 header");
  }
  const view = new DataView(buffer, baseOffset, length);
  if (view.getUint32(0, false) !== CMS2_MAGIC_U32) {
    throw new Error("CMS2 magic mismatch");
  }
  const version = view.getUint32(4, true);
  if (version !== CMS2_VERSION) {
    throw new Error(`Unsupported CMS2 version ${version}`);
  }
  if (view.getUint32(8, true) !== 0 || view.getUint32(28, true) !== 0) {
    throw new Error("Unsupported CMS2 header flags");
  }

  const prototypeCount = view.getUint32(12, true);
  const instanceCount = view.getUint32(16, true);
  const jsonOffset = view.getUint32(20, true);
  const jsonLen = view.getUint32(24, true);
  const blobStart = CMS2_HEADER_BYTES + prototypeCount * CMS2_DIRECTORY_BYTES;
  if (jsonOffset < blobStart || jsonOffset + jsonLen !== length) {
    throw new Error("CMS2 metadata is outside the payload");
  }

  let meta: RawCms2Meta;
  try {
    meta = JSON.parse(utf8Decoder.decode(new Uint8Array(buffer, baseOffset + jsonOffset, jsonLen))) as RawCms2Meta;
  } catch (error) {
    throw new Error(`Invalid CMS2 metadata JSON: ${error instanceof Error ? error.message : String(error)}`);
  }

  if (meta?.units !== "mm") {
    throw new Error("CMS2 units must be millimetres");
  }
  if (!Array.isArray(meta.prototypes) || !Array.isArray(meta.instances)) {
    throw new Error("Invalid CMS2 metadata tables");
  }
  if (meta.prototypes.length !== prototypeCount || meta.instances.length !== instanceCount) {
    throw new Error("CMS2 counts do not match the header");
  }

  const prototypes: DecodedPartPrototype[] = new Array(prototypeCount);
  for (let i = 0; i < prototypeCount; i += 1) {
    const entry = CMS2_HEADER_BYTES + i * CMS2_DIRECTORY_BYTES;
    const vertexCount = view.getUint32(entry, true);
    const indexCount = view.getUint32(entry + 4, true);
    const edgeCount = view.getUint32(entry + 8, true);
    const faceCount = view.getUint32(entry + 12, true);
    const vertexOffset = view.getUint32(entry + 16, true);
    const indexOffset = view.getUint32(entry + 20, true);
    const edgeOffset = view.getUint32(entry + 24, true);
    if (view.getUint32(entry + 28, true) !== 0) {
      throw new Error("CMS2 directory reserved field must be zero");
    }
    const protoMeta = meta.prototypes[i];
    if (!protoMeta || !Array.isArray(protoMeta.face_table) || !Array.isArray(protoMeta.edge_table)) {
      throw new Error("Invalid CMS2 prototype metadata");
    }
    if (faceCount !== protoMeta.face_table.length) {
      throw new Error("CMS2 face table length mismatch");
    }

    const vertexByteLen = vertexCount * CMS2_VERTEX_STRIDE_BYTES;
    const indexByteLen = indexCount * 4;
    const edgeByteLen = edgeCount * CMS2_EDGE_STRIDE_BYTES;

    checkRange(vertexOffset, vertexByteLen, blobStart, jsonOffset);
    checkRange(indexOffset, indexByteLen, blobStart, jsonOffset);
    checkRange(edgeOffset, edgeByteLen, blobStart, jsonOffset);
    if (
      indexOffset < vertexOffset + vertexByteLen ||
      edgeOffset < indexOffset + indexByteLen ||
      edgeOffset + edgeByteLen > jsonOffset
    ) {
      throw new Error("CMS2 prototype regions overlap");
    }
    checkFaceSpans(protoMeta.face_table, vertexCount, indexCount);
    checkEdgeSpans(protoMeta.edge_table, edgeCount);

    const vByteOffset = baseOffset + vertexOffset;
    const iByteOffset = baseOffset + indexOffset;
    const eByteOffset = baseOffset + edgeOffset;

    prototypes[i] = {
      part_name: protoMeta.part_name,
      anchor_mm: protoMeta.anchor_mm,
      vertexCount,
      indexCount,
      edgeSegmentCount: edgeCount,
      vertexData: viewFloat32(buffer, vByteOffset, vertexCount * 8),
      vertexWords: viewUint32(buffer, vByteOffset, vertexCount * 8),
      vertexBytes: new Uint8Array(buffer, vByteOffset, vertexByteLen),
      indices: viewUint32(buffer, iByteOffset, indexCount),
      edgeData: viewFloat32(buffer, eByteOffset, edgeCount * 8),
      edgeWords: viewUint32(buffer, eByteOffset, edgeCount * 8),
      edgeBytes: new Uint8Array(buffer, eByteOffset, edgeByteLen),
      face_table: protoMeta.face_table,
      edge_table: protoMeta.edge_table,
    };
  }

  for (const instance of meta.instances) {
    if (
      !Number.isInteger(instance.prototype_index) ||
      instance.prototype_index < 0 ||
      instance.prototype_index >= prototypeCount
    ) {
      throw new Error("CMS2 instance points at a missing prototype");
    }
  }

  return {
    part_name: meta.part_name,
    kind: meta.kind === "assembly" ? "assembly" : "part",
    units: "mm",
    prototypes,
    instances: meta.instances,
  };
}

/**
 * Decodes a `CM2E` IPC envelope into `{ metadata, scene }` without copying the `CMS2` binary buffers.
 */
export function decodeCms2Envelope<TMetadata = Cms2ArtifactMetadata>(
  payload: ArrayBuffer | ArrayBufferView | readonly number[],
): DecodedCms2Envelope<TMetadata> {
  const { buffer, offset: baseOffset, length } = normalizeBufferInput(payload);
  if (length < CMS2_ENVELOPE_HEADER_BYTES) {
    throw new Error("Truncated CMS2 envelope header");
  }
  const view = new DataView(buffer, baseOffset, length);
  if (view.getUint32(0, false) !== CMS2_ENVELOPE_MAGIC_U32) {
    throw new Error("Invalid CMS2 envelope magic");
  }
  const version = view.getUint32(4, true);
  if (version !== CMS2_ENVELOPE_VERSION) {
    throw new Error(`Unsupported CMS2 envelope version ${version}`);
  }
  const cms2Offset = view.getUint32(8, true);
  const cms2Len = view.getUint32(12, true);
  const metadataOffset = view.getUint32(16, true);
  const metadataLen = view.getUint32(20, true);
  if (view.getUint32(24, true) !== 0 || view.getUint32(28, true) !== 0) {
    throw new Error("Unsupported CMS2 envelope flags");
  }
  if (cms2Offset < CMS2_ENVELOPE_HEADER_BYTES || cms2Offset % 16 !== 0) {
    throw new Error("CMS2 envelope payload offset must be >= 32 and 16-byte aligned");
  }
  if (cms2Offset + cms2Len > metadataOffset || metadataOffset + metadataLen !== length) {
    throw new Error("CMS2 envelope regions are out of bounds");
  }

  const scene = decodeCms2Binary(buffer, baseOffset + cms2Offset, cms2Len);
  let metadata: TMetadata;
  try {
    metadata = JSON.parse(
      utf8Decoder.decode(new Uint8Array(buffer, baseOffset + metadataOffset, metadataLen)),
    ) as TMetadata;
  } catch (error) {
    throw new Error(
      `Invalid CMS2 envelope metadata JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  return { metadata, scene };
}

/** Matches Rust `cad_kernel_occt::Mat4::rotate_euler_deg`: Rz(rz) * Ry(ry) * Rx(rx). */
function rotationMatrixDeg(rotationDeg: Vec3): Mat3 {
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

function mulMat3(m: Mat3, x: number, y: number, z: number): [number, number, number] {
  return [
    m[0] * x + m[1] * y + m[2] * z,
    m[3] * x + m[4] * y + m[5] * z,
    m[6] * x + m[7] * y + m[8] * z,
  ];
}

function normalizeDir(x: number, y: number, z: number): [number, number, number] {
  const len = Math.hypot(x, y, z);
  if (len < 1e-12) return [0, 0, 0];
  return [x / len, y / len, z / len];
}

/**
 * Restores a prototype-local anchor-relative `f32` position back to world `f64` millimetres:
 * `origin_mm + R(rotation_deg) * (anchor_mm + local_position)`.
 */
export function restoreWorldPositionMm(
  prototype: Pick<DecodedPartPrototype, "anchor_mm">,
  instance: Pick<Cms2SceneInstance, "origin_mm" | "rotation_deg">,
  localPosition: readonly [number, number, number],
): [number, number, number] {
  const rot = rotationMatrixDeg(instance.rotation_deg);
  const px = prototype.anchor_mm[0] + localPosition[0];
  const py = prototype.anchor_mm[1] + localPosition[1];
  const pz = prototype.anchor_mm[2] + localPosition[2];
  const [rx, ry, rz] = mulMat3(rot, px, py, pz);
  return [
    instance.origin_mm[0] + rx,
    instance.origin_mm[1] + ry,
    instance.origin_mm[2] + rz,
  ];
}

/**
 * Computes camera-relative RTE translation in `f64` before casting to `f32` for GPU uniforms,
 * matching Rust `cad_render::camera_relative_translation`.
 */
export function restoreCameraRelativeTranslation(
  prototype: Pick<DecodedPartPrototype, "anchor_mm">,
  instance: Pick<Cms2SceneInstance, "origin_mm" | "rotation_deg">,
  cameraTargetMm: Vec3,
  explodeOffsetMm: Vec3 = [0, 0, 0],
): [number, number, number] {
  const rot = rotationMatrixDeg(instance.rotation_deg);
  const [ax, ay, az] = mulMat3(rot, prototype.anchor_mm[0], prototype.anchor_mm[1], prototype.anchor_mm[2]);
  return [
    Math.fround(instance.origin_mm[0] - cameraTargetMm[0] + ax + explodeOffsetMm[0]),
    Math.fround(instance.origin_mm[1] - cameraTargetMm[1] + ay + explodeOffsetMm[1]),
    Math.fround(instance.origin_mm[2] - cameraTargetMm[2] + az + explodeOffsetMm[2]),
  ];
}

/** Exact analytic `FaceRecord` lookup by `(instanceIndex, faceSlotId)` with world-space parameters. */
export function lookupFaceRecord(
  scene: DecodedCompactScene,
  instanceIndex: number,
  faceSlotId: number,
): ResolvedFaceRecord | null {
  const instance = scene.instances[instanceIndex];
  if (!instance) return null;
  const prototype = scene.prototypes[instance.prototype_index];
  if (!prototype) return null;
  const face = prototype.face_table[faceSlotId];
  if (!face) return null;
  const rot = rotationMatrixDeg(instance.rotation_deg);
  const [poX, poY, poZ] = mulMat3(rot, face.plane_origin_mm[0], face.plane_origin_mm[1], face.plane_origin_mm[2]);
  const [pnX, pnY, pnZ] = mulMat3(rot, face.plane_normal[0], face.plane_normal[1], face.plane_normal[2]);
  const [aoX, aoY, aoZ] = mulMat3(rot, face.axis_origin_mm[0], face.axis_origin_mm[1], face.axis_origin_mm[2]);
  const [adX, adY, adZ] = mulMat3(rot, face.axis_dir[0], face.axis_dir[1], face.axis_dir[2]);
  return {
    instanceIndex,
    prototypeIndex: instance.prototype_index,
    faceSlotId,
    instance,
    prototype,
    face,
    planeOriginWorldMm: [instance.origin_mm[0] + poX, instance.origin_mm[1] + poY, instance.origin_mm[2] + poZ],
    planeNormalWorld: normalizeDir(pnX, pnY, pnZ),
    axisOriginWorldMm: [instance.origin_mm[0] + aoX, instance.origin_mm[1] + aoY, instance.origin_mm[2] + aoZ],
    axisDirWorld: normalizeDir(adX, adY, adZ),
  };
}

/** Exact `EdgeRecord` lookup by `(instanceIndex, edgeSlotId)`. */
export function lookupEdgeRecord(
  scene: DecodedCompactScene,
  instanceIndex: number,
  edgeSlotId: number,
): ResolvedEdgeRecord | null {
  const instance = scene.instances[instanceIndex];
  if (!instance) return null;
  const prototype = scene.prototypes[instance.prototype_index];
  if (!prototype) return null;
  const edge = prototype.edge_table[edgeSlotId];
  if (!edge) return null;
  return {
    instanceIndex,
    prototypeIndex: instance.prototype_index,
    edgeSlotId,
    instance,
    prototype,
    edge,
  };
}

/**
 * Adapts a decoded `CMS2` `DecodedCompactScene` into the existing `SceneMeshPayload` contract
 * with full `f64` `anchor_mm` RTE restoration so 2D HUD, measurement tools, and legacy viewports
 * consume `CMS2` payloads seamlessly.
 */
export function compactSceneToSceneMesh(
  scene: DecodedCompactScene,
  options?: DiagnosticsReport | CompactSceneToMeshOptions,
): SceneMeshPayload {
  const normalizedOptions: CompactSceneToMeshOptions =
    options && "diagnostics" in options && Array.isArray((options as DiagnosticsReport).diagnostics)
      ? { diagnostics: options as DiagnosticsReport }
      : ((options as CompactSceneToMeshOptions | undefined) ?? {});

  const faces: FaceMesh[] = [];
  const edges: TopoEdge[] = [];

  for (const instance of scene.instances) {
    const proto = scene.prototypes[instance.prototype_index];
    if (!proto) continue;
    const rot = rotationMatrixDeg(instance.rotation_deg);
    const [ox, oy, oz] = instance.origin_mm;
    const [ax, ay, az] = proto.anchor_mm;
    const componentId = instance.component_id ?? undefined;

    for (const face of proto.face_table) {
      const vertices = new Float64Array(face.vertex_count * 3);
      const normals = new Float64Array(face.vertex_count * 3);
      for (let v = 0; v < face.vertex_count; v += 1) {
        const src = (face.vertex_begin + v) * 8;
        const lx = ax + proto.vertexData[src];
        const ly = ay + proto.vertexData[src + 1];
        const lz = az + proto.vertexData[src + 2];
        const [wx, wy, wz] = mulMat3(rot, lx, ly, lz);
        const dst = v * 3;
        vertices[dst] = ox + wx;
        vertices[dst + 1] = oy + wy;
        vertices[dst + 2] = oz + wz;

        const [nx, ny, nz] = mulMat3(
          rot,
          proto.vertexData[src + 4],
          proto.vertexData[src + 5],
          proto.vertexData[src + 6],
        );
        const [unx, uny, unz] = normalizeDir(nx, ny, nz);
        normals[dst] = unx;
        normals[dst + 1] = uny;
        normals[dst + 2] = unz;
      }

      const indices = new Uint32Array(face.index_count);
      for (let k = 0; k < face.index_count; k += 1) {
        const absoluteIndex = proto.indices[face.index_begin + k];
        indices[k] = absoluteIndex >= face.vertex_begin ? absoluteIndex - face.vertex_begin : 0;
      }

      faces.push({
        face_id: face.face_id,
        surface_type: face.surface_type,
        vertices,
        normals,
        indices,
        provenance_tags: face.provenance_tags,
        exact_interface: face.exact_interface,
        ...(componentId !== undefined ? { component_id: componentId } : {}),
        part_name: proto.part_name,
        ast_span: face.ast_span,
      });
    }

    for (const edge of proto.edge_table) {
      const segCount = edge.segment_count;
      const polyline = new Float64Array(segCount > 0 ? (segCount + 1) * 3 : 0);
      if (segCount > 0) {
        const firstSrc = edge.segment_begin * 8;
        const [aWx, aWy, aWz] = mulMat3(
          rot,
          ax + proto.edgeData[firstSrc],
          ay + proto.edgeData[firstSrc + 1],
          az + proto.edgeData[firstSrc + 2],
        );
        polyline[0] = ox + aWx;
        polyline[1] = oy + aWy;
        polyline[2] = oz + aWz;

        for (let s = 0; s < segCount; s += 1) {
          const src = (edge.segment_begin + s) * 8;
          const [bWx, bWy, bWz] = mulMat3(
            rot,
            ax + proto.edgeData[src + 4],
            ay + proto.edgeData[src + 5],
            az + proto.edgeData[src + 6],
          );
          const dst = (s + 1) * 3;
          polyline[dst] = ox + bWx;
          polyline[dst + 1] = oy + bWy;
          polyline[dst + 2] = oz + bWz;
        }
      }

      edges.push({
        edge_id: edge.edge_id,
        curve_type: edge.curve_type,
        polyline,
        provenance_tags: edge.provenance_tags,
        ...(componentId !== undefined ? { component_id: componentId } : {}),
        ast_span: edge.ast_span,
      });
    }
  }

  const components = normalizedOptions.components ?? normalizedOptions.diagnostics?.components;
  const editHandles = normalizedOptions.edit_handles ?? [];
  const holeOffsets = normalizedOptions.hole_offsets;

  return {
    format_version: "0.1",
    part_name: scene.part_name || normalizedOptions.diagnostics?.part_name || "",
    kind: scene.kind,
    units: scene.units,
    ...(components && components.length > 0 ? { components } : {}),
    faces,
    edges,
    edit_handles: editHandles,
    ...(holeOffsets && holeOffsets.length > 0 ? { hole_offsets: holeOffsets } : {}),
  };
}
