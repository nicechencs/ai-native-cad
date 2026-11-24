/**
 * TES-1 / S8 — model-browser header badge vs status-bar counts.
 *
 * The title badge and the status bar must share kernel face/edge counts.
 * Preview-mesh lengths stay reserved for the pickable face/edge lists.
 */
export interface KernelTopoCounts {
  readonly face_count?: number;
  readonly edge_count?: number;
}

export interface PreviewMeshCounts {
  readonly faces?: { readonly length: number };
  readonly edges?: { readonly length: number };
}

export interface ModelBrowserTopoBadge {
  readonly faces: number;
  readonly edges: number;
  readonly source: "kernel" | "preview-mesh";
}

function isFiniteCount(value: number | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

export function modelBrowserTopoBadge(
  metrics?: KernelTopoCounts | null,
  sceneMesh?: PreviewMeshCounts | null,
): ModelBrowserTopoBadge {
  if (isFiniteCount(metrics?.face_count) && isFiniteCount(metrics?.edge_count)) {
    return { faces: metrics.face_count, edges: metrics.edge_count, source: "kernel" };
  }
  return {
    faces: sceneMesh?.faces?.length ?? 0,
    edges: sceneMesh?.edges?.length ?? 0,
    source: "preview-mesh",
  };
}
