import type { HlodLevel, VirtualTreeNode } from "../sync/cadSessionTransport.ts";
import type { DecodedHlodChunk, HlodChunkStreamer } from "../sync/hlodStreamer.ts";
import type { BoundingBox3D, FaceMesh, SceneMeshPayload, SourceSpan } from "../types/scene.ts";

type HlodViewportScene = SceneMeshPayload & { readonly bbox: BoundingBox3D };

interface FaceGeometry {
  readonly localIndices: Map<number, number>;
  readonly vertices: number[];
  readonly normals: number[];
  readonly indices: number[];
}

const AXES = [0, 1, 2] as const;
const PREVIEW_SPAN: SourceSpan = { start_line: 1, start_col: 1, end_line: 1, end_col: 1 };

/** Convert indexed HLOD geometry into world-space, face-local viewport buffers. */
export function hlodChunkToSceneMesh(
  chunk: DecodedHlodChunk,
  { node, componentId = node && String(node.instance_id) }: {
    readonly node?: VirtualTreeNode;
    readonly componentId?: string;
  } = {},
): HlodViewportScene {
  const { anchorMm, positions, normals, faceSlotIds, indices } = chunk;
  if (positions.length !== faceSlotIds.length * 3 || normals.length !== positions.length || indices.length % 3 !== 0) {
    throw new Error("Invalid HLOD mesh buffer lengths");
  }

  const partName = node?.part_number || node?.name || `hlod_prototype_${chunk.prototypeId}`;
  const groups = new Map<number, FaceGeometry>();
  const min: [number, number, number] = [Infinity, Infinity, Infinity];
  const max: [number, number, number] = [-Infinity, -Infinity, -Infinity];
  for (let triangle = 0; triangle < indices.length; triangle += 3) {
    // The pack writer splits vertices at face boundaries, so all three corners share a slot.
    const slot = faceSlotIds[indices[triangle]];
    let group = groups.get(slot);
    if (!group) {
      group = { localIndices: new Map(), vertices: [], normals: [], indices: [] };
      groups.set(slot, group);
    }
    for (const corner of AXES) {
      const sourceIndex = indices[triangle + corner];
      let localIndex = group.localIndices.get(sourceIndex);
      if (localIndex === undefined) {
        if (sourceIndex >= faceSlotIds.length) throw new Error("Invalid HLOD triangle index");
        localIndex = group.vertices.length / 3;
        group.localIndices.set(sourceIndex, localIndex);
        const offset = sourceIndex * 3;
        for (const axis of AXES) {
          const world = positions[offset + axis] + anchorMm[axis];
          const normal = normals[offset + axis];
          if (!Number.isFinite(world) || !Number.isFinite(normal)) {
            throw new Error("Invalid HLOD vertex or normal");
          }
          group.vertices.push(world);
          group.normals.push(normal);
          min[axis] = Math.min(min[axis], world);
          max[axis] = Math.max(max[axis], world);
        }
      }
      group.indices.push(localIndex);
    }
  }

  const faces = [...groups.entries()].sort(([a], [b]) => a - b).map(
    ([slot, { vertices, normals, indices }]): FaceMesh => ({
      face_id: `hlod_face_${slot}`,
      surface_type: "mesh",
      vertices,
      normals,
      indices,
      provenance_tags: [],
      component_id: componentId,
      part_name: partName,
      ast_span: PREVIEW_SPAN,
    }),
  );
  const bbox: BoundingBox3D = faces.length > 0
    ? { min, max }
    : { min: node?.bbox_min_mm ?? anchorMm, max: node?.bbox_max_mm ?? anchorMm };

  return { format_version: "0.1", part_name: partName, units: "mm", faces, edges: [], edit_handles: [], bbox };
}

interface ProgressiveViewportOptions {
  readonly streamer: Pick<HlodChunkStreamer, "requestProgressiveMesh">;
  readonly onSceneReady: (scene: HlodViewportScene, level: HlodLevel) => void;
  readonly onError?: (error: unknown) => void;
}

interface FocusInstanceOptions {
  readonly sessionId: string;
  readonly node: VirtualTreeNode;
  readonly chunkIdsByLevel: Record<HlodLevel, number>;
  readonly cameraDistanceMm: number;
}

/** Publish each available level for the current focus and resolve with the requested scene. */
export function createProgressiveViewportController({ streamer, onSceneReady, onError }: ProgressiveViewportOptions) {
  let focusId = 0;
  return {
    async focusInstance({ sessionId, node, chunkIdsByLevel, cameraDistanceMm }: FocusInstanceOptions): Promise<HlodViewportScene> {
      const currentFocusId = ++focusId;
      const { bbox_min_mm: min, bbox_max_mm: max } = node;
      const bboxDiagMm = Math.hypot(max[0] - min[0], max[1] - min[1], max[2] - min[2]);
      let lastReady: { chunk: DecodedHlodChunk; scene: HlodViewportScene } | undefined;
      try {
        const finalChunk = await streamer.requestProgressiveMesh(
          sessionId,
          chunkIdsByLevel,
          cameraDistanceMm,
          bboxDiagMm,
          (chunk) => {
            if (currentFocusId !== focusId) return;
            const scene = hlodChunkToSceneMesh(chunk, { node });
            lastReady = { chunk, scene };
            onSceneReady(scene, chunk.level);
          },
        );
        return lastReady?.chunk === finalChunk ? lastReady.scene : hlodChunkToSceneMesh(finalChunk, { node });
      } catch (error) {
        if (currentFocusId === focusId) onError?.(error);
        throw error;
      }
    },
  };
}
