/**
 * Modular viewport adapter connecting `CenterViewport` to the zero-copy `CMS2` WebGPU
 * renderer (`cms2WebgpuRenderer.ts`) with automatic graceful `webgl2-fallback` degradation.
 */

import type { ChangePreview } from "../../core/session/engineeringSession.ts";
import type { SelectionFilterMode, ViewportDisplayState } from "../../core/viewport/displayState.ts";
import type { DecodedCompactScene } from "../../sync/cms2Decoder.ts";
import type {
  SceneMeshPayload,
  SectionCutState,
  SelectionTarget,
  Vec3,
} from "../../types/scene.ts";
import type {
  Cms2FrameState,
  Cms2PickResult,
  Cms2RenderPlan,
  Cms2SectionPlaneConfig,
  WboitChangePreviewKind,
  WboitInstanceOverlay,
  WebGpuCapabilityResult,
} from "../../utils/cms2WebgpuRenderer.ts";

export type ViewportRenderBackend = "webgpu-cms2" | "webgl2-fallback";

export interface ViewportRenderBackendStatus {
  readonly backend: ViewportRenderBackend;
  readonly label: string;
  readonly reason: string;
  readonly webgpuAvailable: boolean;
  readonly hasCompactScene: boolean;
  readonly lastFrameStats?: Cms2RenderPlan["stats"] | null;
}

export interface SelectViewportRenderBackendInput {
  readonly compactScene: DecodedCompactScene | null | undefined;
  readonly webgpuCapability: WebGpuCapabilityResult | null | undefined;
  readonly webgpuRendererReady: boolean;
  readonly webgpuDeviceLost?: boolean;
  readonly displayState?: ViewportDisplayState;
  readonly lastFrameStats?: Cms2RenderPlan["stats"] | null;
}

/**
 * Selects between `"webgpu-cms2"` and `"webgl2-fallback"` based on runtime WebGPU
 * availability, active `DecodedCompactScene`, device health, and visual style.
 */
export function selectViewportRenderBackend(
  input: SelectViewportRenderBackendInput,
): ViewportRenderBackendStatus {
  const hasCompactScene = Boolean(
    input.compactScene &&
      input.compactScene.prototypes.length > 0 &&
      input.compactScene.instances.length > 0,
  );
  const webgpuAvailable = Boolean(input.webgpuCapability?.available) && !input.webgpuDeviceLost;

  if (!hasCompactScene) {
    return {
      backend: "webgl2-fallback",
      label: "WebGL2 Fallback",
      reason: "No active CMS2 compact scene",
      webgpuAvailable,
      hasCompactScene: false,
      lastFrameStats: null,
    };
  }

  if (input.webgpuDeviceLost) {
    return {
      backend: "webgl2-fallback",
      label: "WebGL2 Fallback",
      reason: "WebGPU device lost; degraded to WebGL2",
      webgpuAvailable: false,
      hasCompactScene: true,
      lastFrameStats: null,
    };
  }

  if (!input.webgpuCapability?.available) {
    return {
      backend: "webgl2-fallback",
      label: "WebGL2 Fallback",
      reason: input.webgpuCapability?.reason ?? "WebGPU unavailable in this runtime",
      webgpuAvailable: false,
      hasCompactScene: true,
      lastFrameStats: null,
    };
  }

  if (!input.webgpuRendererReady) {
    return {
      backend: "webgl2-fallback",
      label: "WebGL2 Fallback",
      reason: "WebGPU CMS2 pipeline initializing",
      webgpuAvailable: true,
      hasCompactScene: true,
      lastFrameStats: null,
    };
  }

  if (
    input.displayState &&
    (input.displayState.visualStyle === "wireframe" || input.displayState.visualStyle === "hlr")
  ) {
    return {
      backend: "webgl2-fallback",
      label: "WebGL2 Fallback",
      reason: `Visual style "${input.displayState.visualStyle}" uses WebGL2 fallback`,
      webgpuAvailable: true,
      hasCompactScene: true,
      lastFrameStats: null,
    };
  }

  return {
    backend: "webgpu-cms2",
    label: "WebGPU · CMS2",
    reason: input.webgpuCapability.reason || "Zero-copy CMS2 WebGPU active",
    webgpuAvailable: true,
    hasCompactScene: true,
    lastFrameStats: input.lastFrameStats ?? null,
  };
}

/**
 * Converts `CenterViewport`'s `SectionCutState` + `cutCoord` into the world-space
 * `Cms2SectionPlaneConfig` matching `clipToSection()` discard semantics:
 * discard when `dot(worldPos, normal) > offsetMm`.
 */
export function buildCms2SectionPlane(
  sectionCut: SectionCutState,
  cutCoord: number,
): Cms2SectionPlaneConfig | undefined {
  if (!sectionCut.enabled || !Number.isFinite(cutCoord)) {
    return undefined;
  }
  const sign = sectionCut.flip ? -1 : 1;
  const normal: Vec3 =
    sectionCut.axis === "Z"
      ? [0, 0, sign]
      : sectionCut.axis === "Y"
        ? [0, sign, 0]
        : [sign, 0, 0];
  return {
    enabled: true,
    normal,
    offsetMm: sign * cutCoord,
    stencilCap: true,
    hatchSpacingPx: 8.0,
  };
}

function previewKindFromFeatureChange(
  change: ChangePreview["featureDiffs"][number]["change"],
): WboitChangePreviewKind {
  switch (change) {
    case "added":
    case "unsuppressed":
      return "added";
    case "removed":
    case "suppressed":
      return "removed";
    case "modified":
    case "renamed":
      return "modified";
  }
}

export interface BuildCms2WboitOverlaysOptions {
  readonly changePreview?: ChangePreview | null;
  readonly explicitOverlays?: readonly WboitInstanceOverlay[];
  readonly displayState?: ViewportDisplayState;
}

/**
 * Derives WBOIT translucent overlay descriptors from an active AI `ChangePreview`,
 * explicit caller overrides, or translucent display style.
 */
export function buildCms2WboitOverlays(
  scene: DecodedCompactScene,
  options: BuildCms2WboitOverlaysOptions = {},
): readonly WboitInstanceOverlay[] {
  if (options.explicitOverlays && options.explicitOverlays.length > 0) {
    return options.explicitOverlays.filter(
      (item) => item.instanceIndex >= 0 && item.instanceIndex < scene.instances.length,
    );
  }

  const overlays: WboitInstanceOverlay[] = [];
  const preview = options.changePreview;
  if (preview && scene.instances.length > 0) {
    const matchedIndices = new Set<number>();
    for (const diff of preview.featureDiffs) {
      const kind = previewKindFromFeatureChange(diff.change);
      for (let i = 0; i < scene.instances.length; i += 1) {
        const compId = scene.instances[i]?.component_id;
        if (compId && compId === diff.nodeId && !matchedIndices.has(i)) {
          matchedIndices.add(i);
          overlays.push({ instanceIndex: i, kind, opacity: 0.45 });
        }
      }
    }

    if (overlays.length === 0) {
      const defaultKind: WboitChangePreviewKind =
        preview.featureDiffs.length > 0
          ? previewKindFromFeatureChange(preview.featureDiffs[0].change)
          : preview.mode === "generate"
            ? "added"
            : "ghost";
      for (let i = 0; i < scene.instances.length; i += 1) {
        overlays.push({ instanceIndex: i, kind: defaultKind, opacity: 0.42 });
      }
    }
    return overlays;
  }

  if (options.displayState?.visualStyle === "translucent") {
    return scene.instances.map((_, instanceIndex) => ({
      instanceIndex,
      kind: "ghost" as const,
      opacity: 0.36,
    }));
  }

  return overlays;
}

export interface Cms2SelectionState {
  readonly selectedInstanceIndex: number | null;
  readonly selectedFaceSlot: number | null;
  readonly selectedEdgeSlot: number | null;
  readonly hoveredInstanceIndex: number | null;
  readonly hoveredFaceSlot: number | null;
  readonly hoveredEdgeSlot: number | null;
}

function findTargetSlots(
  scene: DecodedCompactScene,
  target: SelectionTarget,
): { readonly instanceIndex: number; readonly faceSlot: number | null; readonly edgeSlot: number | null } | null {
  if (!target) return null;
  if (target.kind === "face") {
    const targetComp = target.face.component_id ?? null;
    for (let i = 0; i < scene.instances.length; i += 1) {
      const inst = scene.instances[i];
      if (targetComp !== null && inst.component_id !== targetComp) continue;
      const proto = scene.prototypes[inst.prototype_index];
      if (!proto) continue;
      const faceSlot = proto.face_table.findIndex((f) => f.face_id === target.face.face_id);
      if (faceSlot >= 0) {
        return { instanceIndex: i, faceSlot, edgeSlot: null };
      }
    }
  } else {
    const targetComp = target.edge.component_id ?? null;
    for (let i = 0; i < scene.instances.length; i += 1) {
      const inst = scene.instances[i];
      if (targetComp !== null && inst.component_id !== targetComp) continue;
      const proto = scene.prototypes[inst.prototype_index];
      if (!proto) continue;
      const edgeSlot = proto.edge_table.findIndex((e) => e.edge_id === target.edge.edge_id);
      if (edgeSlot >= 0) {
        return { instanceIndex: i, faceSlot: null, edgeSlot };
      }
    }
  }
  return null;
}

/**
 * Resolves `CenterViewport` face/edge/component selection and hover targets into
 * zero-copy `CMS2` instance & slot indices.
 */
export function resolveCms2SelectionState(
  scene: DecodedCompactScene,
  selectedTarget: SelectionTarget,
  hoveredTarget: SelectionTarget,
  selectedComponentId?: string | null,
): Cms2SelectionState {
  const selected = findTargetSlots(scene, selectedTarget);
  const hovered = findTargetSlots(scene, hoveredTarget);

  let selectedInstanceIndex = selected?.instanceIndex ?? null;
  if (selectedInstanceIndex === null && selectedComponentId) {
    const idx = scene.instances.findIndex((inst) => inst.component_id === selectedComponentId);
    selectedInstanceIndex = idx >= 0 ? idx : null;
  }

  return {
    selectedInstanceIndex,
    selectedFaceSlot: selected?.faceSlot ?? null,
    selectedEdgeSlot: selected?.edgeSlot ?? null,
    hoveredInstanceIndex: hovered?.instanceIndex ?? null,
    hoveredFaceSlot: hovered?.faceSlot ?? null,
    hoveredEdgeSlot: hovered?.edgeSlot ?? null,
  };
}

export interface BuildCms2FrameStateInput {
  readonly scene: DecodedCompactScene;
  readonly displayState: ViewportDisplayState;
  readonly sectionCut: SectionCutState;
  readonly cutCoord: number;
  readonly explodeFactor: number;
  readonly selectedTarget: SelectionTarget;
  readonly hoveredTarget: SelectionTarget;
  readonly selectedComponentId?: string | null;
  readonly changePreview?: ChangePreview | null;
  readonly wboitOverlays?: readonly WboitInstanceOverlay[];
}

/**
 * Builds the full `Cms2FrameState` consumed by `cms2WebgpuRenderer.render()` and
 * `cms2WebgpuRenderer.pickPixel()`.
 */
export function buildCms2FrameState(input: BuildCms2FrameStateInput): Cms2FrameState {
  const sectionPlane = buildCms2SectionPlane(input.sectionCut, input.cutCoord);
  const selection = resolveCms2SelectionState(
    input.scene,
    input.selectedTarget,
    input.hoveredTarget,
    input.selectedComponentId,
  );
  const wboitOverlays = buildCms2WboitOverlays(input.scene, {
    changePreview: input.changePreview,
    explicitOverlays: input.wboitOverlays,
    displayState: input.displayState,
  });
  const showEdges =
    input.displayState.visualStyle !== "shaded" && input.displayState.categories.edges;

  return {
    ...(sectionPlane ? { sectionPlane } : {}),
    explodeFactor: input.explodeFactor,
    showEdges,
    edgeWidthPx: 1.5,
    selectedInstanceIndex: selection.selectedInstanceIndex,
    selectedFaceSlot: selection.selectedFaceSlot,
    selectedEdgeSlot: selection.selectedEdgeSlot,
    hoveredInstanceIndex: selection.hoveredInstanceIndex,
    hoveredFaceSlot: selection.hoveredFaceSlot,
    hoveredEdgeSlot: selection.hoveredEdgeSlot,
    ...(wboitOverlays.length > 0 ? { wboitOverlays } : {}),
  };
}

export interface ResolvedViewportPick {
  readonly target: SelectionTarget;
  readonly componentId: string | null;
  readonly label: string | null;
}

/**
 * Maps a 1-pixel `CMS2` GPU pick result back to `CenterViewport`'s `SelectionTarget`
 * and component selection while respecting `selectionFilter`.
 */
export function resolveCms2PickToViewportTarget(
  pick: Cms2PickResult | null,
  sceneMesh: SceneMeshPayload,
  selectionFilter: SelectionFilterMode,
): ResolvedViewportPick | null {
  if (!pick) return null;

  if (pick.kind === "edge" && pick.edgeId) {
    if (selectionFilter === "face" || selectionFilter === "body") {
      return null;
    }
    const edge =
      sceneMesh.edges.find(
        (item) =>
          item.edge_id === pick.edgeId &&
          (pick.componentId == null || item.component_id === pick.componentId),
      ) ?? sceneMesh.edges.find((item) => item.edge_id === pick.edgeId);
    if (!edge) return null;
    return {
      target: { kind: "edge", edge },
      componentId: null,
      label: `Edge: ${edge.edge_id}`,
    };
  }

  if (pick.kind === "face" && pick.faceId) {
    if (selectionFilter === "edge") {
      return null;
    }
    const face =
      sceneMesh.faces.find(
        (item) =>
          item.face_id === pick.faceId &&
          (pick.componentId == null || item.component_id === pick.componentId),
      ) ?? sceneMesh.faces.find((item) => item.face_id === pick.faceId);
    if (!face) return null;
    if (selectionFilter === "body") {
      const compId = face.component_id ?? pick.componentId ?? null;
      return {
        target: { kind: "face", face },
        componentId: compId,
        label: compId ? `Body: ${compId}` : `Face: ${face.face_id}`,
      };
    }
    return {
      target: { kind: "face", face },
      componentId: null,
      label: `Face: ${face.face_id}`,
    };
  }

  return null;
}
