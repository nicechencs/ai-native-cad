import type { CadSessionTransport, HlodLevel } from "./cadSessionTransport.ts";
import type { Vec3 } from "../types/scene.ts";

const HLOD_HEADER_BYTES = 56;
const HLOD_CODEC_VERSION = 1;
const DEFAULT_MAX_CACHED_CHUNKS = 32;
const HLOD_LEVEL_BY_WIRE: readonly HlodLevel[] = ["hlod0", "hlod1", "hlod2"];

/** One complete prototype HLOD mesh decoded from a `.cadpack` chunk. */
export interface DecodedHlodChunk {
  readonly prototypeId: number;
  readonly level: HlodLevel;
  readonly anchorMm: Vec3;
  /** Flattened XYZ local-space vertex positions. */
  readonly positions: Float32Array;
  /** Flattened XYZ vertex normals. */
  readonly normals: Float32Array;
  readonly faceSlotIds: Uint32Array;
  readonly indices: Uint32Array;
}

/** A malformed or unsupported binary HLOD chunk. */
export class HlodDecodeError extends Error {
  readonly code:
    | "truncated"
    | "invalid-magic"
    | "unsupported-version"
    | "invalid-level"
    | "length-mismatch";

  constructor(
    code: HlodDecodeError["code"],
    message: string,
  ) {
    super(message);
    this.name = "HlodDecodeError";
    this.code = code;
  }
}

function readU32(view: DataView, offset: number): number {
  return view.getUint32(offset, true);
}

/** Decode one complete little-endian `HLD1` prototype chunk. */
export function decodeHlodPrototypeChunk(bytes: Uint8Array): DecodedHlodChunk {
  if (bytes.byteLength < HLOD_HEADER_BYTES) {
    throw new HlodDecodeError(
      "truncated",
      `HLOD chunk header is truncated: need ${HLOD_HEADER_BYTES} bytes, got ${bytes.byteLength}`,
    );
  }

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint8(0) !== 0x48 || view.getUint8(1) !== 0x4c || view.getUint8(2) !== 0x44 || view.getUint8(3) !== 0x31) {
    const magic = String.fromCharCode(view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
    throw new HlodDecodeError("invalid-magic", `Invalid HLOD magic ${JSON.stringify(magic)}, expected "HLD1"`);
  }

  const version = readU32(view, 4);
  if (version !== HLOD_CODEC_VERSION) {
    throw new HlodDecodeError("unsupported-version", `Unsupported HLOD codec version ${version}, expected ${HLOD_CODEC_VERSION}`);
  }

  const levelByte = view.getUint8(12);
  const level = HLOD_LEVEL_BY_WIRE[levelByte];
  if (!level) {
    throw new HlodDecodeError("invalid-level", `Invalid HLOD level ${levelByte}, expected 0, 1, or 2`);
  }

  const vertexCount = readU32(view, 40);
  const normalCount = readU32(view, 44);
  const faceSlotCount = readU32(view, 48);
  const indexCount = readU32(view, 52);
  const expectedLength =
    HLOD_HEADER_BYTES + (vertexCount + normalCount) * 12 + (faceSlotCount + indexCount) * 4;
  if (bytes.byteLength !== expectedLength) {
    throw new HlodDecodeError(
      "length-mismatch",
      `HLOD chunk length mismatch: expected ${expectedLength} bytes from header counts, got ${bytes.byteLength}`,
    );
  }

  const anchorMm: Vec3 = [
    view.getFloat64(16, true),
    view.getFloat64(24, true),
    view.getFloat64(32, true),
  ];
  const positions = new Float32Array(vertexCount * 3);
  const normals = new Float32Array(normalCount * 3);
  const faceSlotIds = new Uint32Array(faceSlotCount);
  const indices = new Uint32Array(indexCount);

  let cursor = HLOD_HEADER_BYTES;
  for (let i = 0; i < positions.length; i += 1) {
    positions[i] = view.getFloat32(cursor, true);
    cursor += 4;
  }
  for (let i = 0; i < normals.length; i += 1) {
    normals[i] = view.getFloat32(cursor, true);
    cursor += 4;
  }
  for (let i = 0; i < faceSlotIds.length; i += 1) {
    faceSlotIds[i] = readU32(view, cursor);
    cursor += 4;
  }
  for (let i = 0; i < indices.length; i += 1) {
    indices[i] = readU32(view, cursor);
    cursor += 4;
  }

  return {
    prototypeId: readU32(view, 8),
    level,
    anchorMm,
    positions,
    normals,
    faceSlotIds,
    indices,
  };
}

/** Choose the desired HLOD representation from camera distance and box size. */
export function selectHlodLevelForDistance(distanceMm: number, bboxDiagMm: number): HlodLevel {
  const normalizedDistance = distanceMm / Math.max(bboxDiagMm, 1);
  if (normalizedDistance <= 2.5) return "hlod0";
  if (normalizedDistance <= 8.0) return "hlod1";
  return "hlod2";
}

export interface HlodChunkStreamerOptions {
  readonly transport: CadSessionTransport;
  readonly maxCachedChunks?: number;
}

export interface HlodCacheStats {
  readonly size: number;
  readonly maxSize: number;
  readonly inFlight: number;
}

function normalizeMaxCachedChunks(value: number | undefined): number {
  if (value === undefined) return DEFAULT_MAX_CACHED_CHUNKS;
  if (!Number.isFinite(value) || value < 0) return 0;
  return Math.floor(value);
}

function makeCacheKey(sessionId: string, chunkId: number, byteRange: readonly [number, number] | null | undefined): string {
  const rangeKey = byteRange == null ? "full" : `${byteRange[0]}-${byteRange[1]}`;
  return `${sessionId}:${chunkId}:${rangeKey}`;
}

/** LRU decoded-chunk cache with request deduplication and progressive refinement. */
export class HlodChunkStreamer {
  private readonly transport: CadSessionTransport;
  private readonly maxCachedChunks: number;
  private readonly cache = new Map<string, DecodedHlodChunk>();
  private readonly inFlight = new Map<string, Promise<DecodedHlodChunk>>();
  private progressiveRequestId = 0;

  constructor(options: HlodChunkStreamerOptions) {
    this.transport = options.transport;
    this.maxCachedChunks = normalizeMaxCachedChunks(options.maxCachedChunks);
  }

  async loadChunk(
    sessionId: string,
    chunkId: number,
    byteRange?: readonly [number, number] | null,
  ): Promise<DecodedHlodChunk> {
    const key = makeCacheKey(sessionId, chunkId, byteRange);
    const cached = this.cache.get(key);
    if (cached) {
      this.cache.delete(key);
      this.cache.set(key, cached);
      return cached;
    }

    const pending = this.inFlight.get(key);
    if (pending) return pending;

    const request = (byteRange == null
      ? this.transport.fetchHlodChunk(sessionId, chunkId)
      : this.transport.fetchHlodChunk(sessionId, chunkId, byteRange)
    ).then((chunkBytes) => {
      const decoded = decodeHlodPrototypeChunk(chunkBytes);
      if (this.maxCachedChunks > 0) {
        this.cache.set(key, decoded);
        const oldestKey = this.cache.keys().next().value;
        if (this.cache.size > this.maxCachedChunks && oldestKey !== undefined) this.cache.delete(oldestKey);
      }
      return decoded;
    });
    this.inFlight.set(key, request);
    try {
      return await request;
    } finally {
      if (this.inFlight.get(key) === request) this.inFlight.delete(key);
    }
  }

  /**
   * Load coarse geometry before the selected refinement. A later call on this
   * streamer supersedes callbacks from an earlier call, while the earlier
   * promise still resolves with the chunk it requested.
   */
  async requestProgressiveMesh(
    sessionId: string,
    chunkIdsByLevel: Record<HlodLevel, number>,
    cameraDistanceMm: number,
    bboxDiagMm: number,
    onChunkReady: (chunk: DecodedHlodChunk) => void,
  ): Promise<DecodedHlodChunk> {
    const requestId = ++this.progressiveRequestId;
    const targetLevel = selectHlodLevelForDistance(cameraDistanceMm, bboxDiagMm);
    const coarse = await this.loadChunk(sessionId, chunkIdsByLevel.hlod2);
    if (requestId === this.progressiveRequestId) onChunkReady(coarse);
    if (targetLevel === "hlod2") return coarse;

    const refined = await this.loadChunk(sessionId, chunkIdsByLevel[targetLevel]);
    if (requestId === this.progressiveRequestId) onChunkReady(refined);
    return refined;
  }

  /** Clear completed cache entries. Outstanding fetches may still fill it. */
  clearCache(): void {
    this.cache.clear();
  }

  cacheStats(): HlodCacheStats {
    return {
      size: this.cache.size,
      maxSize: this.maxCachedChunks,
      inFlight: this.inFlight.size,
    };
  }
}
