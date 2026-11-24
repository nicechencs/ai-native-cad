import type { TpmsFamily } from "../../types/graphIr.ts";

type PointMm = readonly [number, number, number];

export interface SubdControlVertexHandle {
  readonly vertexIndex: number;
  readonly positionMm: PointMm;
  readonly creaseWeight: number;
  readonly selected: boolean;
}

export interface SubdCreaseEdgeHandle {
  readonly edgeId: string;
  readonly v0: number;
  readonly v1: number;
  readonly sharpness: number;
}

export interface TpmsFieldPreviewState {
  readonly enabled: boolean;
  readonly variant: TpmsFamily;
  readonly periodMm: number;
  readonly isoLevel: number;
  readonly blendBrepDepth: boolean;
}

export interface SubdFieldViewportState {
  readonly mode: "brep_only" | "subd_cage" | "hybrid_field";
  readonly vertices: readonly SubdControlVertexHandle[];
  readonly creases: readonly SubdCreaseEdgeHandle[];
  readonly fieldPreview: TpmsFieldPreviewState;
  readonly selectedVertexIndex: number | null;
}

const MIN_PERIOD_MM = 0.5;

export function createInitialSubdFieldViewportState(
  vertices: readonly SubdControlVertexHandle[] = [],
  creases: readonly SubdCreaseEdgeHandle[] = [],
): SubdFieldViewportState {
  const selectedVertexOffset = vertices.findIndex((vertex) => vertex.selected);

  return {
    mode: "brep_only",
    vertices: vertices.map((vertex, offset) => ({
      ...vertex,
      positionMm: [...vertex.positionMm],
      selected: offset === selectedVertexOffset,
    })),
    creases: creases.map((edge) => ({ ...edge })),
    fieldPreview: {
      enabled: false,
      variant: "gyroid",
      periodMm: 10,
      isoLevel: 0,
      blendBrepDepth: true,
    },
    selectedVertexIndex: vertices[selectedVertexOffset]?.vertexIndex ?? null,
  };
}

export function dragSubdVertexHandle(
  state: SubdFieldViewportState,
  vertexIndex: number,
  [dx, dy, dz]: PointMm,
): SubdFieldViewportState {
  const target = state.vertices.find((vertex) => vertex.vertexIndex === vertexIndex);
  if (!target || (dx === 0 && dy === 0 && dz === 0)) return state;

  const [x, y, z] = target.positionMm;
  const moved: SubdControlVertexHandle = { ...target, positionMm: [x + dx, y + dy, z + dz] };
  return {
    ...state,
    vertices: state.vertices.map((vertex) => (vertex === target ? moved : vertex)),
  };
}

export function setSubdVertexCreaseWeight(
  state: SubdFieldViewportState,
  vertexIndex: number,
  weight: number,
): SubdFieldViewportState {
  const target = state.vertices.find((vertex) => vertex.vertexIndex === vertexIndex);
  const creaseWeight = Math.min(10, Math.max(0, weight));
  if (!target || Number.isNaN(creaseWeight) || creaseWeight === target.creaseWeight) return state;

  return {
    ...state,
    vertices: state.vertices.map((vertex) =>
      vertex === target ? { ...vertex, creaseWeight } : vertex,
    ),
  };
}

export function setSubdEdgeSharpness(
  state: SubdFieldViewportState,
  edgeId: string,
  sharpness: number,
): SubdFieldViewportState {
  const target = state.creases.find((edge) => edge.edgeId === edgeId);
  const clamped = Math.min(10, Math.max(0, sharpness));
  if (!target || Number.isNaN(clamped) || clamped === target.sharpness) return state;

  return {
    ...state,
    creases: state.creases.map((edge) =>
      edge === target ? { ...edge, sharpness: clamped } : edge,
    ),
  };
}

export function updateTpmsFieldPreview(
  state: SubdFieldViewportState,
  patch: Partial<TpmsFieldPreviewState>,
): SubdFieldViewportState {
  const preview = state.fieldPreview;
  const periodMm = Math.max(MIN_PERIOD_MM, patch.periodMm ?? preview.periodMm);
  const isoLevel = Math.min(1, Math.max(-1, patch.isoLevel ?? preview.isoLevel));

  return {
    ...state,
    fieldPreview: {
      enabled: patch.enabled ?? preview.enabled,
      variant: patch.variant ?? preview.variant,
      periodMm: Number.isFinite(periodMm) ? periodMm : preview.periodMm,
      isoLevel: Number.isNaN(isoLevel) ? preview.isoLevel : isoLevel,
      blendBrepDepth: patch.blendBrepDepth ?? preview.blendBrepDepth,
    },
  };
}

export function evaluateTpmsPreviewSample(
  variant: TpmsFieldPreviewState["variant"],
  pointMm: PointMm,
  periodMm: number,
): number {
  if (!Number.isFinite(periodMm) || periodMm <= 0) return Number.NaN;

  // Reduce each coordinate to one period before converting to radians.
  const [x, y, z] = pointMm.map((coordinate) => ((coordinate % periodMm) / periodMm) * (2 * Math.PI));
  const sx = Math.sin(x);
  const sy = Math.sin(y);
  const sz = Math.sin(z);
  const cx = Math.cos(x);
  const cy = Math.cos(y);
  const cz = Math.cos(z);

  // Use the level-set conventions in crates/cad-implicit/src/tpms.rs.
  switch (variant) {
    case "gyroid":
      return sx * cy + sy * cz + sz * cx;
    case "schwarz_p":
      return cx + cy + cz;
    case "diamond":
      return sx * sy * sz + sx * cy * cz + cx * sy * cz + cx * cy * sz;
    case "lidinoid": {
      const sineTerms = Math.sin(2 * x) * cy * sz + Math.sin(2 * y) * cz * sx + Math.sin(2 * z) * cx * sy;
      const c2x = Math.cos(2 * x);
      const c2y = Math.cos(2 * y);
      const c2z = Math.cos(2 * z);
      return 0.5 * sineTerms - 0.5 * (c2x * c2y + c2y * c2z + c2z * c2x) + 0.15;
    }
  }
}
