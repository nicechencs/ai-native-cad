import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  type ReactNode,
  type Ref,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import type {
  BoundingBox3D,
  EditHandle,
  FaceMesh,
  GeometryMetrics,
  HoleOffsetHandle,
  SceneMeshPayload,
  SectionCutState,
  SelectionTarget,
  SourceSpan,
  Vec3,
} from "../types/scene.ts";
import { AssociativeDrawingPanel } from "../features/drawing/AssociativeDrawingPanel.tsx";
import type { AssociativeDrawingSheetData } from "../features/drawing/drawingSheetModel.ts";
import { clipToSection, generateFallbackDrawingSvg, handleRange, normalizePolyline } from "../utils/studioHelpers.ts";
import { createAssemblyRenderer } from "../utils/assemblyRenderer.ts";
import { createCadWebglRenderer, type CadWebglRenderer } from "../utils/cadWebglRenderer.ts";
import {
  createCms2WebgpuRenderer,
  detectWebGpuCapability,
  detectWebGpuCapabilitySync,
  type Cms2CameraState,
  type Cms2RenderPlan,
  type Cms2WebgpuRenderer,
  type WboitInstanceOverlay,
  type WebGpuCapabilityResult,
} from "../utils/cms2WebgpuRenderer.ts";
import type { ChangePreview } from "../core/session/engineeringSession.ts";
import type { DecodedCompactScene } from "../sync/cms2Decoder.ts";
import {
  buildCms2FrameState,
  resolveCms2PickToViewportTarget,
  selectViewportRenderBackend,
  type ViewportRenderBackendStatus,
} from "../features/viewport/cms2ViewportAdapter.ts";
import { useFeatureTreeMount } from "./LeftPanel.tsx";
import {
  createDefaultDisplayState,
  isFastAssemblyGpuEligible,
  resolveEdgeDisplay,
  resolveFaceDisplay,
  SELECTION_FILTER_OPTIONS,
  type ProjectionMode,
  type SelectionFilterMode,
  type ViewportDisplayState,
  type ViewPreset,
} from "../core/viewport/displayState.ts";
import {
  getNavigationPresetMeta,
  RADIAL_MENU_SLOTS,
  resolveBoxSelectMode,
  resolvePointerDragAction,
  resolveRadialDirection,
  zoomAroundCursor,
  type NavigationPresetId,
  type PointerDragAction,
  type RadialDirection,
  type SelectionIntentRule,
} from "../core/viewport/navigationPreset.ts";
import { MiniToolbar } from "../features/viewport/MiniToolbar.tsx";
import { QuickPickPopover, type QuickPickCandidate } from "../features/viewport/QuickPickPopover.tsx";
import { RadialMenu } from "../features/viewport/RadialMenu.tsx";
import {
  BACK_YAW,
  FRONT_YAW,
  LEFT_YAW,
  RIGHT_YAW,
  SIDE_VIEW_PITCH,
  orthographicOffset,
  yawForOutwardNormal,
} from "../features/viewport/viewOrientation.ts";
import {
  collectCircles,
  makePlanePick,
  measureCircles,
  measurePlanes,
  measurementConst,
  measurementDecimal,
  type CirclePick,
  type DistanceMeasurement,
  type MeasurementMode,
  type PlanePick,
} from "../utils/viewportMeasurement.ts";

export type { ViewPreset };

type Point2D = readonly [number, number];

type RulerSelection =
  | { readonly kind: "circle"; readonly first: CirclePick; readonly second?: CirclePick }
  | { readonly kind: "face"; readonly first: PlanePick; readonly second?: PlanePick };

interface RulerState {
  readonly scene: SceneMeshPayload;
  readonly selection: RulerSelection;
  readonly result?: DistanceMeasurement;
}

interface ProjectedCircleSegment {
  readonly circle: CirclePick;
  readonly a: Vec3;
  readonly b: Vec3;
}

function pickCircle(segments: readonly ProjectedCircleSegment[], x: number, y: number) {
  let best: CirclePick | null = null;
  let bestDistance = 7;
  let bestDepth = -Infinity;
  for (const { circle, a, b } of segments) {
    const dx = b[0] - a[0];
    const dy = b[1] - a[1];
    const lengthSquared = dx * dx + dy * dy;
    const t = lengthSquared > 0
      ? Math.min(1, Math.max(0, ((x - a[0]) * dx + (y - a[1]) * dy) / lengthSquared)) : 0;
    const distance = Math.hypot(x - a[0] - t * dx, y - a[1] - t * dy);
    const depth = a[2] + t * (b[2] - a[2]);
    if (distance < bestDistance || (distance === bestDistance && depth > bestDepth)) {
      best = circle;
      bestDistance = distance;
      bestDepth = depth;
    }
  }
  return best;
}

export interface ViewportCommands {
  readonly applyPreset: (preset: ViewPreset) => void;
  readonly fitToView: () => void;
  readonly normalToSelection: () => void;
  readonly clearMeasurement: () => void;
  readonly zoomBy: (factor: number) => void;
  readonly rotateStep: (deltaYaw: number, deltaPitch?: number) => void;
  readonly orbitBy: (deltaYaw: number, deltaPitch: number) => void;
}

export interface CenterViewportProps {
  readonly ref?: Ref<ViewportCommands>;
  readonly viewportTab: ViewportTab;
  readonly onSelectViewportTab: (tab: ViewportTab) => void;
  readonly measurementMode: MeasurementMode | null;
  readonly onExitMeasurement: () => void;
  readonly showKeepInBox: boolean;
  readonly displayState?: ViewportDisplayState;
  readonly selectionFilter?: SelectionFilterMode;
  readonly selectionIntent?: SelectionIntentRule;
  readonly navigationPreset?: NavigationPresetId;
  readonly onCameraOrientationChange?: (orientation: { readonly yaw: number; readonly pitch: number }) => void;
  readonly sectionCut: SectionCutState;
  readonly explodeFactor: number;
  readonly isAssembly: boolean;
  readonly kclSource: string;
  readonly sceneMesh: SceneMeshPayload;
  readonly keepInBbox: BoundingBox3D | null;
  readonly metrics: GeometryMetrics;
  readonly selectedComponentId?: string | null;
  readonly onSelectComponent?: (componentId: string | null) => void;
  readonly selectedTarget: SelectionTarget;
  readonly onSelectTarget: (target: SelectionTarget) => void;
  readonly selectedTargets?: readonly NonNullable<SelectionTarget>[];
  readonly onChangeSelectedTargets?: (targets: readonly NonNullable<SelectionTarget>[]) => void;
  readonly onUpdateParameter: (paramName: string, value: number, unit: string) => void;
  readonly onOffsetHole: (scene: SceneMeshPayload, handle: HoleOffsetHandle, deltaMm: number) => Promise<void>;
  readonly holeOffsetPending: boolean;
  readonly onFocusAstSpan: (span: SourceSpan, label: string) => void;
  readonly hudOverlay?: ReactNode;
  readonly topViewportOverlay?: ReactNode;
  readonly onViewportContextMenu?: (event: ReactMouseEvent<HTMLCanvasElement>) => void;
  /** Called on Creo/NX MMB single-click in empty canvas when a feature dashboard or modal tool is active. */
  readonly onConfirmActiveTool?: () => void;
  /** Called when an NX 8-way radial gesture or MiniToolbar action executes a command. */
  readonly onExecuteViewportCommand?: (commandId: string) => void;
  /** True when a feature dashboard is active (suppresses MiniToolbar to keep canvas focused). */
  readonly dashboardActive?: boolean;
  /** Failed topology ids from a rolled-back feature. Empty hides the badge. */
  readonly rollbackFailedTopoRefs?: readonly string[];
  /** Feature node that owns `rollbackFailedTopoRefs`, when one is selected. */
  readonly rollbackNodeId?: string | null;
  /** Optional zero-copy `CMS2` compact scene for WebGPU rendering (`webgpu-cms2`). */
  readonly compactScene?: DecodedCompactScene | null;
  /** Optional active AI ChangePreview rendered via WBOIT overlay when `webgpu-cms2` is active. */
  readonly changePreview?: ChangePreview | null;
  /** Optional explicit WBOIT translucent instance overlays. */
  readonly wboitOverlays?: readonly WboitInstanceOverlay[];
  /** Optional callback notifying callers when the active render backend changes. */
  readonly onRenderBackendChange?: (status: ViewportRenderBackendStatus) => void;
}

/**
 * HUD copy when rollback keeps the last valid solid on screen.
 * An empty list hides the badge so a healthy model stays quiet.
 */
export function formatRollbackStatusLabel(
  failedTopoRefs: readonly string[] | null | undefined,
): string | null {
  const refs: string[] = [];
  const seen = new Set<string>();
  for (const topoRef of failedTopoRefs ?? []) {
    const trimmed = topoRef.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    refs.push(trimmed);
  }
  if (refs.length === 0) return null;
  return "回退保护中 · 失效拓扑: " + refs.join(", ");
}

/**
 * Overall X, Y, and Z sizes for one associative drawing sheet.
 * Lengths come from `metrics.bbox_mm`. Each size is tied to the face
 * whose vertices sit closest to that axis maximum. A topo ref listed in
 * `failedTopoRefs` is dangling. Every other callout stays resolved.
 */
export function buildAssociativeDrawingSheet(
  sceneMesh: SceneMeshPayload,
  metrics: GeometryMetrics,
  svgMarkup: string,
  failedTopoRefs?: readonly string[],
): AssociativeDrawingSheetData {
  const axes: readonly {
    readonly id: string;
    readonly label: string;
    readonly axis: 0 | 1 | 2;
    readonly view: "front" | "top" | "right";
  }[] = [
    { id: "envelope-x", label: "Overall X", axis: 0, view: "front" },
    { id: "envelope-y", label: "Overall Y", axis: 1, view: "top" },
    { id: "envelope-z", label: "Overall Z", axis: 2, view: "right" },
  ];
  const failed = new Set<string>();
  for (const topoRef of failedTopoRefs ?? []) {
    if (typeof topoRef !== "string") continue;
    const trimmed = topoRef.trim();
    if (trimmed !== "") failed.add(trimmed);
  }

  const anchorFaceId = (axis: 0 | 1 | 2): string => {
    const target = metrics.bbox_mm.max[axis];
    let bestId = "";
    let bestDistance = Number.POSITIVE_INFINITY;
    let found = false;
    for (const face of sceneMesh.faces) {
      const vertices = face.vertices;
      if (vertices == null) continue;
      for (let index = axis; index < vertices.length; index += 3) {
        const value = vertices[index];
        if (typeof value !== "number" || !Number.isFinite(value) || !Number.isFinite(target)) continue;
        const distance = Math.abs(value - target);
        if (distance < bestDistance) {
          bestDistance = distance;
          bestId = face.face_id;
          found = true;
        }
      }
    }
    if (found) return bestId;
    return sceneMesh.faces[0]?.face_id ?? "";
  };

  const axisLength = (axis: 0 | 1 | 2): number => {
    const min = metrics.bbox_mm.min[axis];
    const max = metrics.bbox_mm.max[axis];
    if (typeof min !== "number" || typeof max !== "number" || !Number.isFinite(min) || !Number.isFinite(max)) {
      return 0;
    }
    return Math.abs(max - min);
  };

  return {
    sheetId: "drawing:" + sceneMesh.part_name,
    title: sceneMesh.part_name,
    scaleLabel: "1:1",
    svgMarkup,
    views: ["front", "top", "right"],
    dimensions: axes.map((axis) => {
      const topoRefId = anchorFaceId(axis.axis);
      return {
        id: axis.id,
        label: axis.label,
        nominalMm: axisLength(axis.axis),
        toleranceText: "",
        status: topoRefId !== "" && failed.has(topoRefId) ? "dangling" : "resolved",
        topoRefId,
        view: axis.view,
      };
    }),
  };
}

interface ProjectedTriangle {
  readonly face: FaceMesh;
  readonly pts: readonly [Point2D, Point2D, Point2D];
  readonly worldPoints?: readonly [Vec3, Vec3, Vec3];
  readonly depth: number;
  readonly shade: number;
  readonly isBackFacing: boolean;
  readonly opacity: number;
  readonly visualStyle: ViewportDisplayState["visualStyle"];
  readonly colorOverride?: string;
  readonly isSemanticHighlighted: boolean;
}

interface ProjectedEdgeSegment {
  readonly edge: SceneMeshPayload["edges"][number];
  readonly a: Point2D;
  readonly b: Point2D;
  readonly depth: number;
}

interface ProjectedHandleAnchor {
  readonly handle: EditHandle;
  readonly x: number;
  readonly y: number;
  readonly radius: number;
}

function pickProjectedEdge(
  segments: readonly ProjectedEdgeSegment[],
  x: number,
  y: number,
  tolerance = 6,
): SceneMeshPayload["edges"][number] | null {
  let best: SceneMeshPayload["edges"][number] | null = null;
  let bestDistance = tolerance;
  let bestDepth = -Infinity;
  for (const { edge, a, b, depth } of segments) {
    const dx = b[0] - a[0];
    const dy = b[1] - a[1];
    const lenSq = dx * dx + dy * dy;
    const t = lenSq > 0 ? Math.min(1, Math.max(0, ((x - a[0]) * dx + (y - a[1]) * dy) / lenSq)) : 0;
    const dist = Math.hypot(x - a[0] - t * dx, y - a[1] - t * dy);
    if (dist < bestDistance || (Math.abs(dist - bestDistance) < 0.5 && depth > bestDepth)) {
      best = edge;
      bestDistance = dist;
      bestDepth = depth;
    }
  }
  return best;
}

function holeDragDelta(dx: number, radiusMm: number): number {
  if (Math.abs(dx) <= 2) return 0;
  const minRadius = Math.min(0.01, radiusMm / 2);
  const mmPerPixel = Math.min(0.25, radiusMm / 40);
  return Math.max(minRadius - radiusMm, Number((dx * mmPerPixel).toPrecision(6)));
}

interface CameraState {
  readonly yaw: number;
  readonly pitch: number;
  readonly zoom: number;
  readonly panX: number;
  readonly panY: number;
}

export type ViewportTab = "3d" | "drawing";

const VIEW_PRESETS: Readonly<Record<ViewPreset, Pick<CameraState, "yaw" | "pitch" | "zoom">>> = {
  iso: { yaw: 0.65, pitch: 0.58, zoom: 2.35 },
  "iso-back": { yaw: 0.65 + Math.PI, pitch: 0.58, zoom: 2.35 },
  top: { yaw: 0, pitch: Math.PI / 2 - 0.001, zoom: 2.4 },
  bottom: { yaw: 0, pitch: -(Math.PI / 2 - 0.001), zoom: 2.4 },
  front: { yaw: FRONT_YAW, pitch: SIDE_VIEW_PITCH, zoom: 2.4 },
  back: { yaw: BACK_YAW, pitch: SIDE_VIEW_PITCH, zoom: 2.4 },
  right: { yaw: RIGHT_YAW, pitch: SIDE_VIEW_PITCH, zoom: 2.4 },
  left: { yaw: LEFT_YAW, pitch: SIDE_VIEW_PITCH, zoom: 2.4 },
};

function lerpAngle(from: number, to: number, t: number): number {
  let diff = ((to - from + Math.PI) % (Math.PI * 2)) - Math.PI;
  if (diff < -Math.PI) diff += Math.PI * 2;
  return from + diff * t;
}

function projectPoint(
  x: number,
  y: number,
  z: number,
  camera: CameraState,
  cx: number,
  cy: number,
  center: Vec3,
  projection: ProjectionMode = "orthographic",
  modelRadius = 60,
): readonly [number, number, number] {
  const dx = x - center[0];
  const dy = y - center[1];
  const dz = z - center[2];
  const { screenX, screenUp, depth } = orthographicOffset(camera.yaw, camera.pitch, dx, dy, dz);

  let scale = camera.zoom;
  if (projection === "perspective") {
    const dist = Math.max(240, modelRadius * 4.2);
    scale *= dist / Math.max(24, dist - depth);
  }

  return [cx + camera.panX + screenX * scale, cy + camera.panY - screenUp * scale, depth];
}

function triangleHitWeights(
  px: number,
  py: number,
  a: Point2D,
  b: Point2D,
  c: Point2D,
): Vec3 | null {
  const v0x = c[0] - a[0];
  const v0y = c[1] - a[1];
  const v1x = b[0] - a[0];
  const v1y = b[1] - a[1];
  const v2x = px - a[0];
  const v2y = py - a[1];

  const dot00 = v0x * v0x + v0y * v0y;
  const dot01 = v0x * v1x + v0y * v1y;
  const dot02 = v0x * v2x + v0y * v2y;
  const dot11 = v1x * v1x + v1y * v1y;
  const dot12 = v1x * v2x + v1y * v2y;

  const denom = dot00 * dot11 - dot01 * dot01;
  if (Math.abs(denom) < 1e-6) return null;
  const invDenom = 1 / denom;
  const u = (dot11 * dot02 - dot01 * dot12) * invDenom;
  const v = (dot00 * dot12 - dot01 * dot02) * invDenom;
  return u >= 0 && v >= 0 && u + v <= 1 ? [1 - u - v, v, u] : null;
}

function computeComponentExplodeOffset(
  componentId: string | undefined,
  componentOrder: ReadonlyMap<string, number>,
  explodeFactor: number,
): Vec3 {
  if (!componentId || explodeFactor <= 0) {
    return [0, 0, 0];
  }
  const idx = componentOrder.get(componentId) ?? 0;
  if (idx === 0) {
    return [0, 0, 0];
  }
  // Offset subsequent assembly components along Z and slightly radially for clear inspection
  return [idx * 14 * explodeFactor, 0, idx * 28 * explodeFactor];
}

function resolveHandleAnchor(
  handle: EditHandle,
  index: number,
  meshBounds: { readonly minZ: number; readonly maxZ: number; readonly maxR: number },
): Vec3 {
  if (handle.anchor_point) {
    return handle.anchor_point;
  }
  const angle = (index * Math.PI) / 3;
  const r = Math.max(20, meshBounds.maxR * 0.85);
  const z =
    meshBounds.minZ +
    ((index + 1) / 7) * Math.max(10, meshBounds.maxZ - meshBounds.minZ);
  return [r * Math.cos(angle), r * Math.sin(angle), z];
}

export function CenterViewport({
  ref,
  viewportTab,
  onSelectViewportTab: _onSelectViewportTab,
  measurementMode,
  onExitMeasurement,
  showKeepInBox,
  displayState,
  selectionFilter = "all",
  selectionIntent = "single",
  navigationPreset = "nx-creo",
  onCameraOrientationChange,
  sectionCut,
  explodeFactor,
  isAssembly,
  kclSource,
  sceneMesh,
  keepInBbox,
  metrics,
  selectedComponentId,
  onSelectComponent,
  selectedTarget,
  onSelectTarget,
  selectedTargets,
  onChangeSelectedTargets,
  onUpdateParameter,
  onOffsetHole,
  holeOffsetPending,
  onFocusAstSpan,
  hudOverlay,
  topViewportOverlay,
  onViewportContextMenu,
  onConfirmActiveTool,
  onExecuteViewportCommand,
  dashboardActive = false,
  rollbackFailedTopoRefs,
  rollbackNodeId,
  compactScene,
  changePreview,
  wboitOverlays,
  onRenderBackendChange,
}: CenterViewportProps) {
  const resolvedDisplay = useMemo(
    () => displayState ?? createDefaultDisplayState(showKeepInBox),
    [displayState, showKeepInBox],
  );
  // App publishes the selection on the feature-tree mount. Explicit props override it.
  const mountedFeatureTree = useFeatureTreeMount();
  const rollbackRefs = rollbackFailedTopoRefs
    ?? mountedFeatureTree?.highlightedFailedTopoRefs
    ?? mountedFeatureTree?.highlightedTopoRefs
    ?? [];
  const rollbackNode = rollbackNodeId !== undefined
    ? rollbackNodeId
    : (mountedFeatureTree?.selectedGraphNodeId ?? null);
  const rollbackLabel = formatRollbackStatusLabel(rollbackRefs);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const cms2WebgpuCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const cms2WebgpuRendererRef = useRef<Cms2WebgpuRenderer | null>(null);
  const cms2WebgpuFrameRef = useRef(false);
  const cms2LastPlanRef = useRef<Cms2RenderPlan | null>(null);
  const cms2PickRequestIdRef = useRef(0);
  const [webgpuCapability, setWebgpuCapability] = useState<WebGpuCapabilityResult>(() =>
    detectWebGpuCapabilitySync(),
  );
  const [webgpuRendererReady, setWebgpuRendererReady] = useState(false);
  const [webgpuDeviceLost, setWebgpuDeviceLost] = useState(false);
  const cadWebglCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const cadWebglRendererRef = useRef<CadWebglRenderer | null>(null);
  const cadWebglFrameRef = useRef(false);
  const assemblyCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const assemblyRendererRef = useRef<Extract<ReturnType<typeof createAssemblyRenderer>, { status: "ready" }>["renderer"] | null>(null);
  const assemblyFrameRef = useRef(false);
  const [graphicsRevision, setGraphicsRevision] = useState(0);
  const projectedTrisRef = useRef<readonly ProjectedTriangle[]>([]);
  const projectedEdgesRef = useRef<readonly ProjectedEdgeSegment[]>([]);
  const pickProjectionKeyRef = useRef<string>("");
  const projectedHandlesRef = useRef<readonly ProjectedHandleAnchor[]>([]);
  const projectedHoleRef = useRef<{ handle: HoleOffsetHandle; x: number; y: number } | null>(null);
  const holeDragRef = useRef<{
    pointerId: number;
    startX: number;
    scene: SceneMeshPayload;
    handle: HoleOffsetHandle;
    commit: CenterViewportProps["onOffsetHole"];
  } | null>(null);
  const projectedCirclesRef = useRef<readonly ProjectedCircleSegment[]>([]);
  const orbitDragRef = useRef<{
    active: boolean;
    mode: PointerDragAction;
    button: number;
    moved: boolean;
    startX: number;
    startY: number;
    x: number;
    y: number;
  }>({
    active: false,
    mode: "none",
    button: -1,
    moved: false,
    startX: 0,
    startY: 0,
    x: 0,
    y: 0,
  });
  const lastMmbClickAtRef = useRef<number>(0);
  const suppressNextContextMenuRef = useRef<boolean>(false);
  const lastHoverLocalRef = useRef<{ readonly x: number; readonly y: number; readonly clientX: number; readonly clientY: number } | null>(null);
  const [boxSelectRect, setBoxSelectRect] = useState<{
    readonly startX: number;
    readonly startY: number;
    readonly endX: number;
    readonly endY: number;
  } | null>(null);
  const boxSelectRectRef = useRef<{
    readonly startX: number;
    readonly startY: number;
    readonly endX: number;
    readonly endY: number;
  } | null>(null);
  const [radialState, setRadialState] = useState<{
    readonly origin: { readonly x: number; readonly y: number };
    readonly direction: RadialDirection | null;
  } | null>(null);
  const radialStateRef = useRef<{
    readonly origin: { readonly x: number; readonly y: number };
    readonly direction: RadialDirection | null;
  } | null>(null);
  const [miniToolbarPos, setMiniToolbarPos] = useState<{ readonly x: number; readonly y: number } | null>(null);
  const [quickPickState, setQuickPickState] = useState<{
    readonly position: { readonly x: number; readonly y: number };
    readonly candidates: readonly QuickPickCandidate[];
    readonly activeIndex: number;
  } | null>(null);
  const [hoveredTarget, setHoveredTarget] = useState<SelectionTarget>(null);
  const effectiveSelectedTargets = useMemo<readonly NonNullable<SelectionTarget>[]>(() => {
    if (selectedTargets && selectedTargets.length > 0) return selectedTargets;
    return selectedTarget ? [selectedTarget] : [];
  }, [selectedTarget, selectedTargets]);
  const selectedFaceIds = useMemo(() => {
    const set = new Set<string>();
    for (const item of effectiveSelectedTargets) {
      if (item.kind === "face") set.add(item.face.face_id);
    }
    return set;
  }, [effectiveSelectedTargets]);
  const selectedEdgeIds = useMemo(() => {
    const set = new Set<string>();
    for (const item of effectiveSelectedTargets) {
      if (item.kind === "edge") set.add(item.edge.edge_id);
    }
    return set;
  }, [effectiveSelectedTargets]);
  const handleDragRef = useRef<{
    handle: EditHandle | null;
    source?: string;
    startX: number;
    startValue: number;
    latestValue: number;
    moved: boolean;
  }>({
    handle: null,
    startX: 0,
    startValue: 0,
    latestValue: 0,
    moved: false,
  });

  const drawingSvg = useMemo(() => viewportTab === "drawing" && sceneMesh.faces.length > 0
    ? generateFallbackDrawingSvg(sceneMesh, metrics, {}) : undefined,
  [viewportTab, sceneMesh, metrics]);
  const drawingSheet = useMemo(() => (
    drawingSvg
      ? buildAssociativeDrawingSheet(sceneMesh, metrics, drawingSvg, rollbackRefs)
      : null
  ), [drawingSvg, metrics, rollbackRefs, sceneMesh]);
  const selectDrawingTopoRef = useCallback((topoRefId: string) => {
    const face = sceneMesh.faces.find((item) => item.face_id === topoRefId);
    if (!face) return;
    onSelectTarget({ kind: "face", face });
    if (!face.ast_span) return;
    onFocusAstSpan(face.ast_span, "Drawing: " + face.face_id);
  }, [onFocusAstSpan, onSelectTarget, sceneMesh.faces]);
  const [viewportSize, setViewportSize] = useState({ width: 760, height: 440 });
  const dpr = typeof window !== "undefined" ? Math.min(window.devicePixelRatio || 1, 2) : 1;
  const [camera, setCamera] = useState<CameraState>({ ...VIEW_PRESETS.iso, panX: 0, panY: 0 });
  const cameraRef = useRef<CameraState>(camera);
  cameraRef.current = camera;
  const cameraAnimRef = useRef<number | null>(null);
  const cancelCameraAnim = useCallback(() => {
    if (cameraAnimRef.current !== null && typeof window !== "undefined") {
      window.cancelAnimationFrame(cameraAnimRef.current);
      cameraAnimRef.current = null;
    }
  }, []);
  const animateCameraTo = useCallback((target: CameraState, durationMs = 210) => {
    cancelCameraAnim();
    if (typeof window === "undefined" || typeof window.requestAnimationFrame !== "function" || durationMs <= 0) {
      setCamera(target);
      return;
    }
    const start = cameraRef.current;
    const startTime = performance.now();
    const step = (now: number) => {
      const elapsed = now - startTime;
      const raw = Math.min(1, elapsed / durationMs);
      const t = 1 - Math.pow(1 - raw, 3);
      setCamera({
        yaw: lerpAngle(start.yaw, target.yaw, t),
        pitch: start.pitch + (target.pitch - start.pitch) * t,
        zoom: start.zoom + (target.zoom - start.zoom) * t,
        panX: start.panX + (target.panX - start.panX) * t,
        panY: start.panY + (target.panY - start.panY) * t,
      });
      if (raw < 1) {
        cameraAnimRef.current = window.requestAnimationFrame(step);
      } else {
        cameraAnimRef.current = null;
      }
    };
    cameraAnimRef.current = window.requestAnimationFrame(step);
  }, [cancelCameraAnim]);
  useEffect(() => cancelCameraAnim, [cancelCameraAnim]);
  useEffect(() => {
    onCameraOrientationChange?.({ yaw: camera.yaw, pitch: camera.pitch });
  }, [camera.yaw, camera.pitch, onCameraOrientationChange]);
  const [activeHandleId, setActiveHandleId] = useState<string>(
    sceneMesh.edit_handles[0]?.handle_id ?? "",
  );
  const [handlePanelOpen, setHandlePanelOpen] = useState<boolean>(false);
  const [draftValues, setDraftValues] = useState<Readonly<Record<string, number>>>({});
  const sliderDrag = useRef<{ pointerId: number; value?: number } | null>(null);
  const [draggingBadge, setDraggingBadge] = useState<string | null>(null);
  const selectedHole = viewportTab === "3d" && !measurementMode && !holeOffsetPending &&
    selectedTarget?.kind === "face" && sceneMesh.faces.includes(selectedTarget.face)
    ? sceneMesh.hole_offsets?.find((handle) => handle.face_id === selectedTarget.face.face_id)
    : undefined;
  const [rulerState, setRulerState] = useState<RulerState | null>(null);
  const [rulerHint, setRulerHint] = useState<{ scene: SceneMeshPayload; text: string } | null>(null);
  const [copyFeedback, setCopyFeedback] = useState<{ result: DistanceMeasurement; text: string } | null>(null);
  // Picks belong to a snapshot of the evaluated geometry; leave camera, view and tool mode intact.
  const ruler = rulerState?.scene === sceneMesh ? rulerState : null;
  const rulerSelection = ruler?.selection;
  const rulerResult = ruler?.result;
  const circleModeActive = measurementMode === "circle";
  const circles = useMemo(() => circleModeActive ? collectCircles(sceneMesh) : [], [circleModeActive, sceneMesh]);

  const componentOrder = useMemo(() => {
    const map = new Map<string, number>();
    if (sceneMesh.components) {
      sceneMesh.components.forEach((comp, idx) => map.set(comp.component_id, idx));
    }
    for (const face of sceneMesh.faces) {
      if (face.component_id && !map.has(face.component_id)) {
        map.set(face.component_id, map.size);
      }
    }
    return map;
  }, [sceneMesh]);

  const meshBounds = useMemo(() => {
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    let minZ = Infinity;
    let maxZ = -Infinity;
    let maxR = 25;
    for (const face of sceneMesh.faces) {
      for (let i = 0; i + 2 < face.vertices.length; i += 3) {
        const x = face.vertices[i];
        const y = face.vertices[i + 1];
        const z = face.vertices[i + 2];
        minX = Math.min(minX, x);
        maxX = Math.max(maxX, x);
        minY = Math.min(minY, y);
        maxY = Math.max(maxY, y);
        minZ = Math.min(minZ, z);
        maxZ = Math.max(maxZ, z);
        maxR = Math.max(maxR, Math.hypot(x, y));
      }
    }
    if (!Number.isFinite(minX)) {
      return {
        minX: -50, maxX: 50, minY: -50, maxY: 50, minZ: 0, maxZ: 50,
        maxR: 50, radius: 75, center: [0, 0, 25] as Vec3,
      };
    }
    const cx = (minX + maxX) / 2;
    const cy = (minY + maxY) / 2;
    const cz = (minZ + maxZ) / 2;
    const dx = maxX - minX;
    const dy = maxY - minY;
    const dz = maxZ - minZ;
    const radius = Math.max(10, Math.hypot(dx, dy, dz) / 2);
    return {
      minX, maxX, minY, maxY, minZ, maxZ, maxR, radius,
      center: [cx, cy, cz] as Vec3,
    };
  }, [sceneMesh]);

  const computeFitZoom = useCallback(() => {
    const minDim = Math.max(120, Math.min(viewportSize.width, viewportSize.height));
    return Math.max(0.05, Math.min(40, (minDim * 0.34) / Math.max(10, meshBounds.radius)));
  }, [meshBounds.radius, viewportSize.height, viewportSize.width]);

  const activeHandle: EditHandle | undefined = useMemo(
    () =>
      sceneMesh.edit_handles.find((h) => h.handle_id === activeHandleId) ??
      sceneMesh.edit_handles[0],
    [sceneMesh.edit_handles, activeHandleId],
  );

  // Clear stale slider drafts when the source or evaluated mesh changes.
  useEffect(() => {
    sliderDrag.current = null;
    setDraftValues({});
    setDraggingBadge(null);
  }, [kclSource, sceneMesh]);

  useEffect(() => {
    setRulerState(null);
    setRulerHint(null);
    setCopyFeedback(null);
  }, [sceneMesh]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (viewportTab !== "3d" || !canvas) return;

    const observer = new ResizeObserver(([entry]) => {
      const width = Math.max(1, Math.round(entry.contentRect.width));
      const height = Math.max(1, Math.round(entry.contentRect.height));
      setViewportSize((size) =>
        size.width === width && size.height === height ? size : { width, height },
      );
    });
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [viewportTab]);

  useEffect(() => {
    const canvas = cms2WebgpuCanvasRef.current;
    if (!canvas || viewportTab !== "3d" || !compactScene) {
      setWebgpuRendererReady(false);
      return;
    }

    let cancelled = false;
    let ownedRenderer: Cms2WebgpuRenderer | null = null;
    setWebgpuDeviceLost(false);

    void (async () => {
      const capability = await detectWebGpuCapability();
      if (cancelled) return;
      setWebgpuCapability(capability);
      if (!capability.available) {
        setWebgpuRendererReady(false);
        return;
      }

      const renderer = await createCms2WebgpuRenderer(canvas, {
        onDeviceLost: () => {
          cms2WebgpuRendererRef.current = null;
          cms2WebgpuFrameRef.current = false;
          setWebgpuDeviceLost(true);
          setWebgpuRendererReady(false);
          setGraphicsRevision((revision) => revision + 1);
        },
      });
      if (cancelled) {
        renderer?.dispose();
        return;
      }
      if (!renderer) {
        setWebgpuCapability({
          available: false,
          backend: "webgl2-fallback",
          reason: "Failed to initialize WebGPU canvas context; using WebGL2 fallback",
        });
        setWebgpuRendererReady(false);
        return;
      }

      ownedRenderer = renderer;
      cms2WebgpuRendererRef.current = renderer;
      if (renderer.updateScene(compactScene)) {
        setWebgpuRendererReady(true);
        setGraphicsRevision((revision) => revision + 1);
      } else {
        setWebgpuRendererReady(false);
      }
    })();

    return () => {
      cancelled = true;
      if (ownedRenderer) {
        if (cms2WebgpuRendererRef.current === ownedRenderer) {
          cms2WebgpuRendererRef.current = null;
        }
        cms2WebgpuFrameRef.current = false;
        ownedRenderer.dispose();
      }
      setWebgpuRendererReady(false);
    };
  }, [compactScene, viewportTab]);

  const renderBackendStatus = useMemo(
    () =>
      selectViewportRenderBackend({
        compactScene,
        webgpuCapability,
        webgpuRendererReady,
        webgpuDeviceLost,
        displayState: resolvedDisplay,
        lastFrameStats: cms2LastPlanRef.current?.stats ?? null,
      }),
    [compactScene, resolvedDisplay, webgpuCapability, webgpuDeviceLost, webgpuRendererReady, graphicsRevision],
  );

  useEffect(() => {
    onRenderBackendChange?.(renderBackendStatus);
  }, [onRenderBackendChange, renderBackendStatus]);

  useEffect(() => {
    const canvas = cadWebglCanvasRef.current;
    if (!canvas || viewportTab !== "3d") return;
    const renderer = createCadWebglRenderer(canvas, {
      onContextLost: () => setGraphicsRevision((revision) => revision + 1),
    });
    if (!renderer) return;
    if (!renderer.updateScene(sceneMesh, componentOrder)) {
      renderer.dispose();
      return;
    }
    cadWebglRendererRef.current = renderer;
    setGraphicsRevision((revision) => revision + 1);
    return () => {
      cadWebglRendererRef.current = null;
      renderer.dispose();
    };
  }, [componentOrder, sceneMesh, viewportTab]);

  useEffect(() => {
    const canvas = assemblyCanvasRef.current;
    if (!canvas || !isAssembly || viewportTab !== "3d") return;
    const result = createAssemblyRenderer(canvas, {
      onContextLost: () => setGraphicsRevision((revision) => revision + 1),
    });
    if (result.status !== "ready") return;
    const { renderer } = result;
    if (renderer.updateScene(sceneMesh).status !== "ready") {
      renderer.dispose();
      return;
    }
    assemblyRendererRef.current = renderer;
    return () => {
      assemblyRendererRef.current = null;
      renderer.dispose();
    };
  }, [isAssembly, sceneMesh, viewportTab]);

  const centerZ = meshBounds.center[2];
  const sectionAxisIndex: 0 | 1 | 2 =
    sectionCut.axis === "Z" ? 2 : sectionCut.axis === "Y" ? 1 : 0;
  const cutCoord = sectionCut.axis === "Z"
    ? meshBounds.minZ + sectionCut.ratio * (meshBounds.maxZ - meshBounds.minZ)
    : sectionCut.axis === "Y"
      ? meshBounds.minY + sectionCut.ratio * (meshBounds.maxY - meshBounds.minY)
      : meshBounds.minX + sectionCut.ratio * (meshBounds.maxX - meshBounds.minX);
  const sectionFlip = Boolean(sectionCut.flip);

  const projectTriangles = useCallback((cx: number, cy: number, pick?: Point2D) => {
    const triangles: ProjectedTriangle[] = [];
    const cosY = Math.cos(camera.yaw);
    const sinY = Math.sin(camera.yaw);
    const cosP = Math.cos(camera.pitch);
    const sinP = Math.sin(camera.pitch);
    // Camera-space 3-point key & fill lights (so front/side/back always read clearly)
    const keyL: readonly [number, number, number] = [0.43, 0.59, 0.68];
    const fillL: readonly [number, number, number] = [-0.71, -0.34, 0.44];

    for (const face of sceneMesh.faces) {
      const faceDisplay = resolveFaceDisplay(resolvedDisplay, face);
      if (!faceDisplay.visible || !faceDisplay.drawSurface) continue;
      const [ox, oy, oz] = computeComponentExplodeOffset(
        face.component_id,
        componentOrder,
        explodeFactor,
      );
      const verts = face.vertices;
      const norms = face.normals;
      const idx = face.indices;
      for (let t = 0; t + 2 < idx.length; t += 3) {
        const i0 = idx[t] * 3;
        const i1 = idx[t + 1] * 3;
        const i2 = idx[t + 2] * 3;

        const v0x = verts[i0] + ox;
        const v0y = verts[i0 + 1] + oy;
        const v0z = verts[i0 + 2] + oz;
        const v1x = verts[i1] + ox;
        const v1y = verts[i1 + 1] + oy;
        const v1z = verts[i1 + 2] + oz;
        const v2x = verts[i2] + ox;
        const v2y = verts[i2 + 1] + oy;
        const v2z = verts[i2 + 2] + oz;

        const points: readonly Vec3[] = [[v0x, v0y, v0z], [v1x, v1y, v1z], [v2x, v2y, v2z]];
        const visible = sectionCut.enabled
          ? clipToSection(points, sectionAxisIndex, cutCoord, true, sectionFlip)
          : points;
        if (visible.length < 3) continue;
        const projected = visible.map((point) =>
          projectPoint(
            ...point,
            camera,
            cx,
            cy,
            meshBounds.center,
            resolvedDisplay.projection,
            meshBounds.radius,
          ),
        );

        const nx = (norms[i0] + norms[i1] + norms[i2]) / 3;
        const ny = (norms[i0 + 1] + norms[i1 + 1] + norms[i2 + 1]) / 3;
        const nz = (norms[i0 + 2] + norms[i1 + 2] + norms[i2 + 2]) / 3;
        const nLen = Math.hypot(nx, ny, nz) || 1;
        const unx = nx / nLen;
        const uny = ny / nLen;
        const unz = nz / nLen;
        // Transform normal to camera space (right, up, toward-eye)
        const vnx = unx * cosY - uny * sinY;
        const ny1 = unx * sinY + uny * cosY;
        const vny = ny1 * sinP + unz * cosP;
        const vnz = ny1 * cosP - unz * sinP;
        const isBackFacing = vnz < -0.02;
        const fnx = isBackFacing ? -vnx : vnx;
        const fny = isBackFacing ? -vny : vny;
        const fnz = isBackFacing ? -vnz : vnz;
        const keyDot = Math.max(0, (fnx * keyL[0] + fny * keyL[1] + fnz * keyL[2] + 0.22) / 1.22);
        const fillDot = Math.max(0, fnx * fillL[0] + fny * fillL[1] + fnz * fillL[2]);
        const shade = Math.min(1, Math.max(0.18, 0.24 + 0.62 * keyDot + 0.16 * fillDot));

        for (let i = 1; i + 1 < projected.length; i += 1) {
          const [a, b, c] = [projected[0], projected[i], projected[i + 1]];
          const pts: ProjectedTriangle["pts"] = [[a[0], a[1]], [b[0], b[1]], [c[0], c[1]]];
          if (pick && !triangleHitWeights(...pick, ...pts)) continue;
          triangles.push({
            face,
            pts,
            worldPoints: measurementMode === "face" ? [visible[0], visible[i], visible[i + 1]] : undefined,
            depth: (a[2] + b[2] + c[2]) / 3,
            shade,
            isBackFacing,
            opacity: faceDisplay.opacity,
            visualStyle: faceDisplay.visualStyle,
            colorOverride: faceDisplay.colorOverride,
            isSemanticHighlighted: faceDisplay.isSemanticHighlighted,
          });
        }
      }
    }

    return triangles.sort((a, b) => a.depth - b.depth);
  }, [
    camera,
    componentOrder,
    cutCoord,
    explodeFactor,
    measurementMode,
    meshBounds.center,
    meshBounds.radius,
    resolvedDisplay,
    sceneMesh,
    sectionAxisIndex,
    sectionCut.enabled,
    sectionFlip,
  ]);

  const projectEdges = useCallback((cx: number, cy: number): ProjectedEdgeSegment[] => {
    const segments: ProjectedEdgeSegment[] = [];
    for (const edge of sceneMesh.edges) {
      const edgeDisplay = resolveEdgeDisplay(resolvedDisplay, edge);
      if (!edgeDisplay.visible) continue;
      const [ox, oy, oz] = computeComponentExplodeOffset(
        edge.component_id,
        componentOrder,
        explodeFactor,
      );
      const points = normalizePolyline(edge.polyline).map(([x, y, z]): Vec3 => [x + ox, y + oy, z + oz]);
      for (let i = 1; i < points.length; i += 1) {
        const segment = [points[i - 1], points[i]];
        const visible = sectionCut.enabled
          ? clipToSection(segment, sectionAxisIndex, cutCoord, false, sectionFlip)
          : segment;
        if (visible.length < 2) continue;
        const a = projectPoint(
          visible[0][0], visible[0][1], visible[0][2],
          camera, cx, cy, meshBounds.center, resolvedDisplay.projection, meshBounds.radius,
        );
        const b = projectPoint(
          visible[1][0], visible[1][1], visible[1][2],
          camera, cx, cy, meshBounds.center, resolvedDisplay.projection, meshBounds.radius,
        );
        segments.push({
          edge,
          a: [a[0], a[1]],
          b: [b[0], b[1]],
          depth: (a[2] + b[2]) / 2,
        });
      }
    }
    return segments;
  }, [
    camera,
    componentOrder,
    cutCoord,
    explodeFactor,
    meshBounds.center,
    meshBounds.radius,
    resolvedDisplay,
    sceneMesh.edges,
    sectionAxisIndex,
    sectionCut.enabled,
    sectionFlip,
  ]);

  const ensurePickProjection = useCallback((width: number, height: number) => {
    const key = [
      camera.yaw.toFixed(4),
      camera.pitch.toFixed(4),
      camera.zoom.toFixed(4),
      camera.panX.toFixed(1),
      camera.panY.toFixed(1),
      width,
      height,
      resolvedDisplay.visualStyle,
      resolvedDisplay.projection,
      sectionCut.enabled ? `${sectionCut.axis}:${sectionCut.ratio}:${sectionFlip}` : "off",
      explodeFactor.toFixed(2),
      sceneMesh.faces.length,
      sceneMesh.edges.length,
    ].join("|");
    if (pickProjectionKeyRef.current === key) return;
    const cx = width / 2;
    const cy = height / 2 + 12;
    projectedTrisRef.current = projectTriangles(cx, cy);
    projectedEdgesRef.current = projectEdges(cx, cy);
    pickProjectionKeyRef.current = key;
  }, [
    camera.panX,
    camera.panY,
    camera.pitch,
    camera.yaw,
    camera.zoom,
    explodeFactor,
    projectEdges,
    projectTriangles,
    resolvedDisplay.projection,
    resolvedDisplay.visualStyle,
    sceneMesh.edges.length,
    sceneMesh.faces.length,
    sectionCut.axis,
    sectionCut.enabled,
    sectionCut.ratio,
    sectionFlip,
  ]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const cms2WebgpuCanvas = cms2WebgpuCanvasRef.current;
    const cadWebglCanvas = cadWebglCanvasRef.current;
    const assemblyCanvas = assemblyCanvasRef.current;
    if (viewportTab !== "3d" || !canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const width = viewportSize.width;
    const height = viewportSize.height;
    const cx = width / 2;
    const cy = height / 2 + 12;
    const isHlrGlobal = resolvedDisplay.visualStyle === "hlr";
    const bgColor = isHlrGlobal ? "#e9eef3" : "#1c2229";

    const measuredFaces = new Set<FaceMesh>();
    if (rulerSelection?.kind === "face") {
      measuredFaces.add(rulerSelection.first.face);
      if (rulerSelection.second) measuredFaces.add(rulerSelection.second.face);
    }

    // Prefer zero-copy CMS2 WebGPU renderer when WebGPU & DecodedCompactScene are active,
    // degrading cleanly to the existing WebGL2 / assembly / 2D fallback pipeline.
    let cms2WebgpuFrame = false;
    const cms2Renderer = cms2WebgpuRendererRef.current;
    if (renderBackendStatus.backend === "webgpu-cms2" && cms2Renderer && compactScene) {
      const cms2Camera: Cms2CameraState = {
        yaw: camera.yaw,
        pitch: camera.pitch,
        zoom: camera.zoom,
        panX: camera.panX,
        panY: camera.panY,
        cx,
        cy,
        width,
        height,
        projection: resolvedDisplay.projection,
        center: meshBounds.center,
        modelRadius: meshBounds.radius,
      };
      const cms2FrameState = buildCms2FrameState({
        scene: compactScene,
        displayState: resolvedDisplay,
        sectionCut,
        cutCoord,
        explodeFactor,
        selectedTarget,
        hoveredTarget,
        selectedComponentId,
        changePreview,
        wboitOverlays,
      });
      const plan = cms2Renderer.render(cms2Camera, cms2FrameState);
      cms2LastPlanRef.current = plan;
      cms2WebgpuFrame = plan !== null;
    } else {
      cms2LastPlanRef.current = null;
    }
    cms2WebgpuFrameRef.current = cms2WebgpuFrame;
    if (cms2WebgpuCanvas) cms2WebgpuCanvas.style.visibility = cms2WebgpuFrame ? "visible" : "hidden";

    // Universal hardware Z-buffer WebGL2 CAD renderer fallback for crisp lighting,
    // front/back distinction, depth-tested edges, and smooth surfaces on all models.
    let cadGpuFrame = false;
    const cadRenderer = cadWebglRendererRef.current;
    if (!cms2WebgpuFrame && cadRenderer && sceneMesh.faces.length > 0) {
      cadGpuFrame = cadRenderer.render(
        {
          yaw: camera.yaw,
          pitch: camera.pitch,
          zoom: camera.zoom,
          panX: camera.panX,
          panY: camera.panY,
          cx,
          cy,
          width,
          height,
          projection: resolvedDisplay.projection,
          center: meshBounds.center,
          modelRadius: meshBounds.radius,
        },
        {
          displayState: resolvedDisplay,
          sectionCut,
          cutCoord,
          explodeFactor,
          selectedTarget,
          hoveredTarget,
          selectedComponentId,
          measuredFaces,
        },
      );
    }
    cadWebglFrameRef.current = cadGpuFrame;
    if (cadWebglCanvas) cadWebglCanvas.style.visibility = cadGpuFrame ? "visible" : "hidden";

    const renderer = assemblyRendererRef.current;
    let gpuFrame = false;
    if (
      !cms2WebgpuFrame &&
      !cadGpuFrame &&
      renderer &&
      isFastAssemblyGpuEligible(resolvedDisplay) &&
      !sectionCut.enabled &&
      !explodeFactor &&
      !measurementMode &&
      !selectedComponentId &&
      !selectedTarget &&
      !hoveredTarget
    ) {
      renderer.updateCamera({ ...camera, cx, cy, centerZ, width, height });
      renderer.updateDisplayOptions?.({
        visualStyle: resolvedDisplay.visualStyle,
        semanticHighlights: resolvedDisplay.categories.semanticHighlights,
      });
      gpuFrame = renderer.render().status === "ready";
    }
    assemblyFrameRef.current = gpuFrame;
    if (assemblyCanvas) assemblyCanvas.style.visibility = gpuFrame ? "visible" : "hidden";

    const anyGpuFrame = cms2WebgpuFrame || cadGpuFrame || gpuFrame;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    if (!anyGpuFrame) {
      const grad = ctx.createRadialGradient(cx, cy * 0.85, Math.min(width, height) * 0.08, cx, cy, Math.max(width, height) * 0.78);
      if (isHlrGlobal) {
        grad.addColorStop(0, "#f5f7fa");
        grad.addColorStop(1, "#dde5ee");
      } else {
        grad.addColorStop(0, "#26303b");
        grad.addColorStop(1, "#161b21");
      }
      ctx.fillStyle = grad;
      ctx.fillRect(0, 0, width, height);
    }
    const project = (pt: Vec3) =>
      projectPoint(
        pt[0],
        pt[1],
        pt[2],
        camera,
        cx,
        cy,
        meshBounds.center,
        resolvedDisplay.projection,
        meshBounds.radius,
      );
    const drawLine = (from: Vec3, to: Vec3) => {
      const a = project(from);
      const b = project(to);
      ctx.beginPath();
      ctx.moveTo(a[0], a[1]);
      ctx.lineTo(b[0], b[1]);
      ctx.stroke();
    };

    // Grid is depth-tested inside WebGL2 when cadGpuFrame is active; draw in 2D otherwise.
    if (resolvedDisplay.categories.grid && !cadGpuFrame) {
      ctx.strokeStyle = isHlrGlobal ? "rgba(71, 85, 105, 0.14)" : "rgba(110, 140, 180, 0.15)";
      ctx.lineWidth = 1;
      for (let g = -80; g <= 80; g += 20) {
        if (g === 0) continue;
        drawLine([g, -80, 0], [g, 80, 0]);
        drawLine([-80, g, 0], [80, g, 0]);
      }
      ctx.strokeStyle = isHlrGlobal ? "rgba(239, 68, 68, 0.38)" : "rgba(248, 113, 113, 0.34)";
      drawLine([-80, 0, 0], [80, 0, 0]);
      ctx.strokeStyle = isHlrGlobal ? "rgba(34, 197, 94, 0.38)" : "rgba(74, 222, 128, 0.34)";
      drawLine([0, -80, 0], [0, 80, 0]);
    }
    if (showKeepInBox && keepInBbox && resolvedDisplay.categories.keepInBox) {
      const [minX, minY, minZ] = keepInBbox.min;
      const [maxX, maxY, maxZ] = keepInBbox.max;
      const corners: readonly Vec3[] = [
        [minX, minY, minZ], [maxX, minY, minZ], [maxX, maxY, minZ], [minX, maxY, minZ],
        [minX, minY, maxZ], [maxX, minY, maxZ], [maxX, maxY, maxZ], [minX, maxY, maxZ],
      ];
      const edges = [[0, 1], [1, 2], [2, 3], [3, 0], [4, 5], [5, 6], [6, 7], [7, 4], [0, 4], [1, 5], [2, 6], [3, 7]];
      ctx.save();
      ctx.setLineDash([4, 4]);
      ctx.strokeStyle = isHlrGlobal ? "rgba(2, 132, 199, 0.45)" : "rgba(56, 189, 248, 0.32)";
      ctx.lineWidth = 1;
      for (const [i, j] of edges) drawLine(corners[i], corners[j]);
      ctx.restore();
    }

    const triangles = anyGpuFrame ? [] : projectTriangles(cx, cy);
    if (!anyGpuFrame) {
      projectedTrisRef.current = triangles;
    } else {
      pickProjectionKeyRef.current = "";
    }
    for (const tri of triangles) {
      const isSelectedFace = selectedFaceIds.has(tri.face.face_id);
      const isHoveredFace =
        !isSelectedFace &&
        hoveredTarget?.kind === "face" &&
        hoveredTarget.face.face_id === tri.face.face_id;
      const isSelectedComp =
        Boolean(selectedComponentId) &&
        tri.face.component_id === selectedComponentId;
      const isMeasuredFace = rulerSelection?.kind === "face" &&
        (rulerSelection.first.face === tri.face || rulerSelection.second?.face === tri.face);

      ctx.save();
      if (tri.opacity < 0.99 && !isSelectedFace && !isMeasuredFace) {
        ctx.globalAlpha = tri.opacity;
      }

      if (isMeasuredFace) {
        ctx.fillStyle = `rgb(36, ${Math.round(116 + tri.shade * 75)}, ${Math.round(126 + tri.shade * 75)})`;
      } else if (isSelectedFace || isSelectedComp) {
        ctx.fillStyle = `rgba(250, 204, 21, ${(0.55 + tri.shade * 0.35).toFixed(2)})`;
      } else if (isHoveredFace) {
        ctx.fillStyle = `rgba(56, 189, 248, ${(0.45 + tri.shade * 0.32).toFixed(2)})`;
      } else if (tri.visualStyle === "hlr") {
        const tone = Math.round((tri.isBackFacing ? 210 : 228) + tri.shade * 22);
        ctx.fillStyle = `rgb(${tone}, ${Math.min(255, tone + 2)}, ${Math.min(255, tone + 5)})`;
      } else if (tri.colorOverride) {
        ctx.fillStyle = tri.colorOverride;
      } else if (tri.isSemanticHighlighted) {
        const g = Math.round(135 + tri.shade * 70);
        const b = Math.round(180 + tri.shade * 60);
        ctx.fillStyle = `rgb(68, ${g}, ${b})`;
      } else if (tri.isBackFacing) {
        const base = Math.round(68 + tri.shade * 64);
        ctx.fillStyle = `rgb(${Math.round(base * 1.05)}, ${Math.round(base * 0.88)}, ${Math.round(base * 0.82)})`;
      } else {
        const base = Math.round(88 + tri.shade * 108);
        ctx.fillStyle = `rgb(${base}, ${Math.round(base * 1.08)}, ${Math.round(base * 1.18)})`;
      }

      ctx.beginPath();
      ctx.moveTo(tri.pts[0][0], tri.pts[0][1]);
      ctx.lineTo(tri.pts[1][0], tri.pts[1][1]);
      ctx.lineTo(tri.pts[2][0], tri.pts[2][1]);
      ctx.closePath();
      ctx.fill();
      ctx.restore();
    }

    // In WebGL2 mode, edges are depth-tested in hardware; draw only selected/hovered edge
    // in 2D overlay so it stands out with crisp 2.6px stroke width.
    const edgesToDraw2d = anyGpuFrame
      ? sceneMesh.edges.filter(
          (edge) =>
            selectedEdgeIds.has(edge.edge_id) ||
            (hoveredTarget?.kind === "edge" && hoveredTarget.edge.edge_id === edge.edge_id),
        )
      : sceneMesh.edges;
    const nextProjectedEdges: ProjectedEdgeSegment[] = [];
    for (const edge of edgesToDraw2d) {
      const edgeDisplay = resolveEdgeDisplay(resolvedDisplay, edge);
      if (!edgeDisplay.visible) continue;
      const [ox, oy, oz] = computeComponentExplodeOffset(
        edge.component_id,
        componentOrder,
        explodeFactor,
      );
      const isSelectedEdge = selectedEdgeIds.has(edge.edge_id);
      const isHoveredEdge =
        !isSelectedEdge &&
        hoveredTarget?.kind === "edge" &&
        hoveredTarget.edge.edge_id === edge.edge_id;

      ctx.save();
      if (edgeDisplay.opacity < 0.99 && !isSelectedEdge && !isHoveredEdge) {
        ctx.globalAlpha = edgeDisplay.opacity;
      }
      ctx.strokeStyle = isSelectedEdge
        ? "#facc15"
        : isHoveredEdge
          ? "#38bdf8"
          : edgeDisplay.colorOverride
            ? edgeDisplay.colorOverride
            : isHlrGlobal
              ? "#1e293b"
              : resolvedDisplay.categories.semanticHighlights &&
                  edge.provenance_tags.includes("bearing_seat_wall")
                ? "#38bdf8"
                : resolvedDisplay.visualStyle === "wireframe"
                  ? "#93c5fd"
                  : "#cbd5e1";
      ctx.lineWidth = isSelectedEdge ? 2.8 : isHoveredEdge ? 2.4 : isHlrGlobal ? 1.45 : 1.35;

      const points = normalizePolyline(edge.polyline).map(([x, y, z]): Vec3 => [x + ox, y + oy, z + oz]);
      ctx.beginPath();
      for (let i = 1; i < points.length; i += 1) {
        const segment = [points[i - 1], points[i]];
        const visible = sectionCut.enabled
          ? clipToSection(segment, sectionAxisIndex, cutCoord, false, sectionFlip)
          : segment;
        if (visible.length < 2) continue;
        const [a, b] = visible.map((point) => project(point));
        nextProjectedEdges.push({
          edge,
          a: [a[0], a[1]],
          b: [b[0], b[1]],
          depth: (a[2] + b[2]) / 2,
        });
        ctx.moveTo(a[0], a[1]);
        ctx.lineTo(b[0], b[1]);
      }
      ctx.stroke();
      ctx.restore();
    }
    if (!anyGpuFrame) {
      projectedEdgesRef.current = nextProjectedEdges;
    }

    // Section Cut Plane wireframe indicator
    if (sectionCut.enabled && resolvedDisplay.categories.sectionPlane) {
      const r = meshBounds.maxR * 1.15;
      const quad: readonly Vec3[] =
        sectionCut.axis === "Z"
          ? [
              [-r, -r, cutCoord],
              [r, -r, cutCoord],
              [r, r, cutCoord],
              [-r, r, cutCoord],
            ]
          : sectionCut.axis === "Y"
            ? [
                [-r, cutCoord, meshBounds.minZ - 4],
                [r, cutCoord, meshBounds.minZ - 4],
                [r, cutCoord, meshBounds.maxZ + 4],
                [-r, cutCoord, meshBounds.maxZ + 4],
              ]
            : [
                [cutCoord, -r, meshBounds.minZ - 4],
                [cutCoord, r, meshBounds.minZ - 4],
                [cutCoord, r, meshBounds.maxZ + 4],
                [cutCoord, -r, meshBounds.maxZ + 4],
              ];
      ctx.save();
      ctx.strokeStyle = "rgba(34, 211, 238, 0.85)";
      ctx.fillStyle = "rgba(34, 211, 238, 0.08)";
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      quad.forEach((pt, idx) => {
        const p = project(pt);
        if (idx === 0) {
          ctx.moveTo(p[0], p[1]);
        } else {
          ctx.lineTo(p[0], p[1]);
        }
      });
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
      ctx.restore();
    }

    // Render Tier 1/2/3 interactive 3D edit handle anchors & store screen coords for direct dragging
    const anchors: ProjectedHandleAnchor[] = [];
    if (resolvedDisplay.categories.editHandles) {
      sceneMesh.edit_handles.forEach((handle, idx) => {
        const [hx, hy, hz] = resolveHandleAnchor(handle, idx, meshBounds);
        const p = project([hx, hy, hz]);
        const isActive = activeHandle?.handle_id === handle.handle_id;
        const radius = isActive ? 8 : 6;
        anchors.push({ handle, x: p[0], y: p[1], radius });

        ctx.save();
        ctx.beginPath();
        if (handle.tier === 1) {
          ctx.fillStyle = isActive ? "#60a5fa" : "#2563eb";
          ctx.strokeStyle = "#eff6ff";
          ctx.lineWidth = isActive ? 2.5 : 1.5;
          ctx.arc(p[0], p[1], radius, 0, Math.PI * 2);
          ctx.fill();
          ctx.stroke();
        } else if (handle.tier === 2) {
          ctx.strokeStyle = "#f59e0b";
          ctx.lineWidth = 2;
          ctx.arc(p[0], p[1], radius + 1.5, 0, Math.PI * 2);
          ctx.stroke();
          ctx.beginPath();
          ctx.fillStyle = "#0ea5e9";
          ctx.arc(p[0], p[1], 3.8, 0, Math.PI * 2);
          ctx.fill();
        } else {
          ctx.fillStyle = "#64748b";
          ctx.strokeStyle = "#e2e8f0";
          ctx.lineWidth = 1.5;
          ctx.rect(p[0] - 5.5, p[1] - 5.5, 11, 11);
          ctx.fill();
          ctx.stroke();
        }
        ctx.restore();
      });
    }
    projectedHandlesRef.current = anchors;

    projectedHoleRef.current = null;
    if (selectedHole && selectedTarget?.kind === "face") {
      if (anyGpuFrame) {
        ensurePickProjection(width, height);
      }
      const holeTriangles = anyGpuFrame ? projectedTrisRef.current : triangles;
      // Use the frontmost projected triangle on the selected wall that fits inside the viewport.
      for (let index = holeTriangles.length - 1; index >= 0; index -= 1) {
        const tri = holeTriangles[index];
        if (tri.face !== selectedTarget.face) continue;
        const [a, b, c] = tri.pts;
        const x = (a[0] + b[0] + c[0]) / 3;
        const y = (a[1] + b[1] + c[1]) / 3;
        if (x < 12 || y < 12 || x > width - 12 || y > height - 12) continue;
        projectedHoleRef.current = { handle: selectedHole, x, y };
        ctx.save();
        ctx.beginPath();
        ctx.arc(x, y, 9, 0, Math.PI * 2);
        ctx.fillStyle = "#2563eb";
        ctx.strokeStyle = "#eff6ff";
        ctx.lineWidth = 1.5;
        ctx.fill();
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(x - 5, y);
        ctx.lineTo(x + 5, y);
        ctx.moveTo(x - 2, y - 3);
        ctx.lineTo(x - 5, y);
        ctx.lineTo(x - 2, y + 3);
        ctx.moveTo(x + 2, y - 3);
        ctx.lineTo(x + 5, y);
        ctx.lineTo(x + 2, y + 3);
        ctx.stroke();
        ctx.restore();
        break;
      }
    }

    const projectMeasurementPoint = (point: Vec3, componentId?: string) => {
      const [ox, oy, oz] = computeComponentExplodeOffset(componentId, componentOrder, explodeFactor);
      return project([point[0] + ox, point[1] + oy, point[2] + oz]);
    };
    const circleSegments: ProjectedCircleSegment[] = [];
    ctx.save();
    for (const circle of circles) {
      const selected = rulerSelection?.kind === "circle" &&
        (rulerSelection.first === circle || rulerSelection.second === circle);
      ctx.strokeStyle = selected ? "#67e8f9" : "rgba(103, 232, 249, 0.58)";
      ctx.lineWidth = selected ? 3 : 1.7;
      const [ox, oy, oz] = computeComponentExplodeOffset(circle.componentId, componentOrder, explodeFactor);
      const points = circle.polyline.map(([x, y, z]): Vec3 => [x + ox, y + oy, z + oz]);
      ctx.beginPath();
      for (let i = 1; i < points.length; i += 1) {
        const segment = [points[i - 1], points[i]];
        const visible = sectionCut.enabled
          ? clipToSection(segment, sectionAxisIndex, cutCoord, false, sectionFlip) : segment;
        if (visible.length < 2) continue;
        const [a, b] = visible.map((point) => project(point));
        circleSegments.push({ circle, a, b });
        ctx.moveTo(a[0], a[1]);
        ctx.lineTo(b[0], b[1]);
      }
      ctx.stroke();
    }
    projectedCirclesRef.current = circleSegments;

    const markers = rulerResult?.points ?? (rulerSelection ? [rulerSelection.first] : []);
    const [a, b] = markers.map(({ point, componentId }) => projectMeasurementPoint(point, componentId));
    ctx.strokeStyle = "#67e8f9";
    ctx.lineWidth = 2;
    if (a && b) {
      ctx.setLineDash([5, 3]);
      ctx.beginPath();
      ctx.moveTo(a[0], a[1]);
      ctx.lineTo(b[0], b[1]);
      ctx.stroke();
      ctx.setLineDash([]);
    }
    for (const point of [a, b]) {
      if (!point) continue;
      ctx.beginPath();
      ctx.arc(point[0], point[1], 4, 0, 2 * Math.PI);
      ctx.fillStyle = bgColor;
      ctx.fill();
      ctx.stroke();
    }
    ctx.restore();

    // NX/Creo Window (left->right blue solid) / Crossing (right->left green dashed) Box Selection
    if (boxSelectRect) {
      const mode = resolveBoxSelectMode(boxSelectRect.startX, boxSelectRect.endX);
      const bx = Math.min(boxSelectRect.startX, boxSelectRect.endX);
      const by = Math.min(boxSelectRect.startY, boxSelectRect.endY);
      const bw = Math.abs(boxSelectRect.endX - boxSelectRect.startX);
      const bh = Math.abs(boxSelectRect.endY - boxSelectRect.startY);
      ctx.save();
      ctx.fillStyle = mode === "window" ? "rgba(59, 130, 246, 0.14)" : "rgba(34, 197, 94, 0.14)";
      ctx.strokeStyle = mode === "window" ? "#3b82f6" : "#22c55e";
      ctx.lineWidth = 1.4;
      if (mode === "crossing") ctx.setLineDash([5, 3]);
      ctx.fillRect(bx, by, bw, bh);
      ctx.strokeRect(bx, by, bw, bh);
      ctx.restore();
    }

    // Axis triad gizmo in bottom-left corner
    if (resolvedDisplay.categories.axisTriad) {
      const origin = [42, height - 38] as const;
      const axisLen = 22;
      const drawAxis = (
        ax: number,
        ay: number,
        az: number,
        color: string,
        label: string,
      ) => {
        const p = projectPoint(
          ax * axisLen,
          ay * axisLen,
          az * axisLen,
          { yaw: camera.yaw, pitch: camera.pitch, zoom: 1, panX: 0, panY: 0 },
          origin[0],
          origin[1],
          [0, 0, 0],
        );
        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(origin[0], origin[1]);
        ctx.lineTo(p[0], p[1]);
        ctx.stroke();
        ctx.fillStyle = color;
        ctx.font = "11px monospace";
        ctx.fillText(label, p[0] + 3, p[1] + 3);
      };
      drawAxis(1, 0, 0, "#f87171", "X");
      drawAxis(0, 1, 0, "#4ade80", "Y");
      drawAxis(0, 0, 1, "#60a5fa", "Z");
    }
  }, [
    viewportTab,
    viewportSize,
    sceneMesh,
    camera,
    keepInBbox,
    showKeepInBox,
    resolvedDisplay,
    selectedTarget,
    selectedFaceIds,
    selectedEdgeIds,
    boxSelectRect,
    hoveredTarget,
    selectedHole,
    selectedComponentId,
    activeHandle,
    sectionCut,
    sectionAxisIndex,
    sectionFlip,
    explodeFactor,
    componentOrder,
    meshBounds,
    measurementMode,
    circles,
    rulerSelection,
    rulerResult,
    centerZ,
    cutCoord,
    dpr,
    ensurePickProjection,
    graphicsRevision,
    projectTriangles,
    compactScene,
    changePreview,
    wboitOverlays,
    renderBackendStatus.backend,
  ]);

  const commitParameter = (handle: EditHandle, nextVal: number) => {
    setDraftValues((prev) => ({ ...prev, [handle.handle_id]: nextVal }));
    onUpdateParameter(handle.param_name, nextVal, handle.unit);
    onFocusAstSpan(handle.ast_span, handle.param_name);
  };

  const expandTargetByIntent = useCallback(
    (base: NonNullable<SelectionTarget>): readonly NonNullable<SelectionTarget>[] => {
      if (selectionIntent === "single") return [base];
      if (base.kind === "edge") {
        if (selectionIntent === "feature") {
          return sceneMesh.edges
            .filter(
              (edge) =>
                edge.ast_span.start_line === base.edge.ast_span.start_line &&
                edge.ast_span.end_line === base.edge.ast_span.end_line,
            )
            .map((edge) => ({ kind: "edge" as const, edge }));
        }
        if (selectionIntent === "tangent" || selectionIntent === "loop") {
          const baseZ = normalizePolyline(base.edge.polyline)[0]?.[2];
          return sceneMesh.edges
            .filter((edge) => {
              if (edge.edge_id === base.edge.edge_id) return true;
              if (selectionIntent === "tangent") {
                return edge.curve_type === base.edge.curve_type && edge.ast_span.start_line === base.edge.ast_span.start_line;
              }
              const ez = normalizePolyline(edge.polyline)[0]?.[2];
              return baseZ !== undefined && ez !== undefined && Math.abs(ez - baseZ) < 0.25;
            })
            .map((edge) => ({ kind: "edge" as const, edge }));
        }
      } else if (base.kind === "face" && selectionIntent === "feature") {
        return sceneMesh.faces
          .filter(
            (face) =>
              face.ast_span.start_line === base.face.ast_span.start_line &&
              face.ast_span.end_line === base.face.ast_span.end_line,
          )
          .map((face) => ({ kind: "face" as const, face }));
      }
      return [base];
    },
    [sceneMesh.edges, sceneMesh.faces, selectionIntent],
  );

  const applySelectionPick = useCallback(
    (
      picked: SelectionTarget,
      additive: boolean,
      miniAnchor?: { readonly x: number; readonly y: number } | null,
    ) => {
      setQuickPickState(null);
      if (!picked) {
        setMiniToolbarPos(null);
        onSelectComponent?.(null);
        onSelectTarget(null);
        onChangeSelectedTargets?.([]);
        return;
      }

      const expanded = expandTargetByIntent(picked);
      let nextList: NonNullable<SelectionTarget>[];
      if (additive) {
        const keyOf = (t: NonNullable<SelectionTarget>) =>
          t.kind === "face" ? `f:${t.face.face_id}` : `e:${t.edge.edge_id}`;
        const existingMap = new Map<string, NonNullable<SelectionTarget>>();
        for (const item of effectiveSelectedTargets) existingMap.set(keyOf(item), item);
        for (const item of expanded) {
          const k = keyOf(item);
          if (existingMap.has(k) && expanded.length === 1) {
            existingMap.delete(k);
          } else {
            existingMap.set(k, item);
          }
        }
        nextList = Array.from(existingMap.values());
      } else {
        nextList = [...expanded];
      }

      const primary = nextList[nextList.length - 1] ?? null;
      onSelectTarget(primary);
      onChangeSelectedTargets?.(nextList);
      if (primary && miniAnchor && !dashboardActive && !measurementMode) {
        setMiniToolbarPos(miniAnchor);
      } else if (!primary) {
        setMiniToolbarPos(null);
      }
      if (primary) {
        const span = primary.kind === "face" ? primary.face.ast_span : primary.edge.ast_span;
        const label = primary.kind === "face" ? `Face: ${primary.face.face_id}` : `Edge: ${primary.edge.edge_id}`;
        onFocusAstSpan(span, label);
      }
    },
    [
      dashboardActive,
      effectiveSelectedTargets,
      expandTargetByIntent,
      measurementMode,
      onChangeSelectedTargets,
      onFocusAstSpan,
      onSelectComponent,
      onSelectTarget,
    ],
  );

  const collectQuickPickCandidates = useCallback(
    (px: number, py: number): readonly QuickPickCandidate[] => {
      if (cadWebglFrameRef.current || cms2WebgpuFrameRef.current) {
        ensurePickProjection(viewportSize.width, viewportSize.height);
      }
      const candidates: QuickPickCandidate[] = [];
      const seen = new Set<string>();

      if (selectionFilter === "all" || selectionFilter === "edge") {
        const tol = selectionFilter === "edge" ? 10 : 7;
        for (const seg of projectedEdgesRef.current) {
          if (seen.has(`e:${seg.edge.edge_id}`)) continue;
          const dx = seg.b[0] - seg.a[0];
          const dy = seg.b[1] - seg.a[1];
          const lenSq = dx * dx + dy * dy;
          const t = lenSq > 0 ? Math.min(1, Math.max(0, ((px - seg.a[0]) * dx + (py - seg.a[1]) * dy) / lenSq)) : 0;
          const dist = Math.hypot(px - seg.a[0] - t * dx, py - seg.a[1] - t * dy);
          if (dist <= tol) {
            seen.add(`e:${seg.edge.edge_id}`);
            candidates.push({
              id: `e:${seg.edge.edge_id}`,
              target: { kind: "edge", edge: seg.edge },
              label: seg.edge.edge_id,
              detail: `边 · ${seg.edge.curve_type}`,
            });
          }
        }
      }

      if (selectionFilter !== "edge") {
        const tris = projectedTrisRef.current;
        for (let i = tris.length - 1; i >= 0; i -= 1) {
          const tri = tris[i];
          if (seen.has(`f:${tri.face.face_id}`)) continue;
          if (triangleHitWeights(px, py, ...tri.pts)) {
            seen.add(`f:${tri.face.face_id}`);
            candidates.push({
              id: `f:${tri.face.face_id}`,
              target: { kind: "face", face: tri.face },
              label: tri.face.face_id,
              detail: `面 · ${tri.face.surface_type}`,
            });
          }
        }
      }
      return candidates.slice(0, 8);
    },
    [ensurePickProjection, selectionFilter, viewportSize.height, viewportSize.width],
  );

  // Tab key invokes NX QuickPick / Creo Query Select cycle on hovered overlapping geometry.
  useEffect(() => {
    if (viewportTab !== "3d") return;
    const handleTabQuery = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || event.ctrlKey || event.metaKey || event.altKey) return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable)
      ) {
        return;
      }
      const hover = lastHoverLocalRef.current;
      if (!hover) return;
      const cands = quickPickState?.candidates ?? collectQuickPickCandidates(hover.x, hover.y);
      if (cands.length < 2) return;
      event.preventDefault();
      const nextIndex = quickPickState ? (quickPickState.activeIndex + 1) % cands.length : 1;
      const nextCand = cands[nextIndex];
      setQuickPickState({
        position: { x: hover.x, y: hover.y },
        candidates: cands,
        activeIndex: nextIndex,
      });
      if (nextCand) setHoveredTarget(nextCand.target);
    };
    window.addEventListener("keydown", handleTabQuery);
    return () => window.removeEventListener("keydown", handleTabQuery);
  }, [collectQuickPickCandidates, quickPickState, viewportTab]);

  // Dismiss cursor MiniToolbar when selection clears or Dashboard opens.
  useEffect(() => {
    if (!selectedTarget || dashboardActive || measurementMode) {
      setMiniToolbarPos(null);
    }
  }, [dashboardActive, measurementMode, selectedTarget]);

  const handleCanvasPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary) return;
    if (event.button !== 0 && event.button !== 1 && event.button !== 2) return;
    if (event.button === 1) event.preventDefault();
    cancelCameraAnim();
    setQuickPickState(null);

    const canvas = event.currentTarget;
    holeDragRef.current = null;
    canvas.setPointerCapture(event.pointerId);
    const rect = canvas.getBoundingClientRect();
    const px = rect.width > 0 ? (event.clientX - rect.left) * (viewportSize.width / rect.width) : 0;
    const py = rect.height > 0 ? (event.clientY - rect.top) * (viewportSize.height / rect.height) : 0;

    if (event.button === 0) {
      const holeAnchor = projectedHoleRef.current;
      if (selectedHole && holeAnchor?.handle === selectedHole && Math.hypot(px - holeAnchor.x, py - holeAnchor.y) <= 13) {
        holeDragRef.current = {
          pointerId: event.pointerId, startX: event.clientX, scene: sceneMesh, handle: selectedHole, commit: onOffsetHole,
        };
        orbitDragRef.current.active = false;
        orbitDragRef.current.moved = true;
        return;
      }

      // Check if pointer hit a 3D Edit Handle anchor for direct 3D dragging
      for (const anchor of measurementMode ? [] : projectedHandlesRef.current) {
        const dist = Math.hypot(px - anchor.x, py - anchor.y);
        if (dist <= anchor.radius + 5) {
          const currentVal =
            draftValues[anchor.handle.handle_id] ?? anchor.handle.current_value;
          setActiveHandleId(anchor.handle.handle_id);
          setHandlePanelOpen(true);
          onFocusAstSpan(
            anchor.handle.ast_span,
            `3D 手柄: ${anchor.handle.param_name}`,
          );
          orbitDragRef.current.moved = true;
          handleDragRef.current = {
            handle: anchor.handle,
            source: kclSource,
            startX: event.clientX,
            startValue: currentVal,
            latestValue: currentVal,
            moved: false,
          };
          return;
        }
      }
    }

    const dragAction = resolvePointerDragAction(navigationPreset, {
      button: event.button,
      buttons: event.buttons,
      shiftKey: event.shiftKey,
      ctrlKey: event.ctrlKey || event.metaKey,
      altKey: event.altKey,
    });

    if (dragAction !== "none" && dragAction !== "box-select") {
      setMiniToolbarPos(null);
    }
    if (dragAction === "pan" && (event.buttons & 2) !== 0) {
      // NX chord pan (RMB held + MMB drag): suppress stray context menu when RMB releases.
      suppressNextContextMenuRef.current = true;
    }

    orbitDragRef.current = {
      active: dragAction !== "none",
      mode: dragAction,
      button: event.button,
      moved: false,
      startX: px,
      startY: py,
      x: event.clientX,
      y: event.clientY,
    };
  };

  const handleCanvasPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary) return;
    const holeDrag = holeDragRef.current;
    if (holeDrag?.pointerId === event.pointerId) {
      if (holeDrag.scene !== sceneMesh || holeDrag.handle !== selectedHole) {
        setDraggingBadge(null);
        return;
      }
      const delta = holeDragDelta(event.clientX - holeDrag.startX, holeDrag.handle.radius_mm);
      const radius = Number((holeDrag.handle.radius_mm + delta).toPrecision(6));
      setDraggingBadge(`孔半径 ${radius} mm`);
      return;
    }
    const activeDrag = handleDragRef.current;
    if (activeDrag.handle) {
      const dx = event.clientX - activeDrag.startX;
      if (Math.abs(dx) > 2) {
        activeDrag.moved = true;
      }
      if (!activeDrag.moved || activeDrag.source !== kclSource) return;
      const h = activeDrag.handle;
      const [minV, maxV] = handleRange(h);
      const next = Number((activeDrag.startValue + dx * 0.25).toFixed(2));
      const nextVal = Math.max(minV, Math.min(maxV, next));
      activeDrag.latestValue = nextVal;
      setDraftValues((prev) => ({ ...prev, [h.handle_id]: nextVal }));
      setDraggingBadge(`${h.param_name}: ${nextVal}${h.unit}`);
      return;
    }

    const canvas = canvasRef.current;
    const rect = canvas?.getBoundingClientRect();
    const localX = rect && rect.width > 0 ? (event.clientX - rect.left) * (viewportSize.width / rect.width) : 0;
    const localY = rect && rect.height > 0 ? (event.clientY - rect.top) * (viewportSize.height / rect.height) : 0;
    lastHoverLocalRef.current = { x: localX, y: localY, clientX: event.clientX, clientY: event.clientY };

    if (!orbitDragRef.current.active) {
      // Hover pre-highlight respecting active selection filter
      if (!measurementMode && !assemblyFrameRef.current && rect && rect.width > 0 && rect.height > 0) {
        if (cadWebglFrameRef.current || cms2WebgpuFrameRef.current) {
          ensurePickProjection(viewportSize.width, viewportSize.height);
        }
        if (selectionFilter === "all" || selectionFilter === "edge") {
          const edgeTol = selectionFilter === "edge" ? 9 : 6;
          const hitEdge = pickProjectedEdge(projectedEdgesRef.current, localX, localY, edgeTol);
          if (hitEdge) {
            setHoveredTarget((prev) =>
              prev?.kind === "edge" && prev.edge.edge_id === hitEdge.edge_id
                ? prev
                : { kind: "edge", edge: hitEdge },
            );
            return;
          }
          if (selectionFilter === "edge") {
            setHoveredTarget((prev) => (prev === null ? prev : null));
            return;
          }
        }
        const tris = projectedTrisRef.current;
        for (let i = tris.length - 1; i >= 0; i -= 1) {
          if (triangleHitWeights(localX, localY, ...tris[i].pts)) {
            const hitFace = tris[i].face;
            setHoveredTarget((prev) =>
              prev?.kind === "face" && prev.face.face_id === hitFace.face_id
                ? prev
                : { kind: "face", face: hitFace },
            );
            return;
          }
        }
        setHoveredTarget((prev) => (prev === null ? prev : null));
      }
      return;
    }

    const dx = event.clientX - orbitDragRef.current.x;
    const dy = event.clientY - orbitDragRef.current.y;
    const mode = orbitDragRef.current.mode;

    if (mode === "radial-gesture") {
      const totalDx = localX - orbitDragRef.current.startX;
      const totalDy = localY - orbitDragRef.current.startY;
      if (Math.hypot(totalDx, totalDy) >= 16) {
        orbitDragRef.current.moved = true;
        suppressNextContextMenuRef.current = true;
        const nextRadial = {
          origin: { x: orbitDragRef.current.startX, y: orbitDragRef.current.startY },
          direction: resolveRadialDirection(totalDx, totalDy, 20),
        };
        radialStateRef.current = nextRadial;
        setRadialState(nextRadial);
      }
      return;
    }

    if (mode === "box-select") {
      if (measurementMode) return;
      const dist = Math.hypot(localX - orbitDragRef.current.startX, localY - orbitDragRef.current.startY);
      if (dist >= 6) {
        orbitDragRef.current.moved = true;
        const nextRect = {
          startX: orbitDragRef.current.startX,
          startY: orbitDragRef.current.startY,
          endX: localX,
          endY: localY,
        };
        boxSelectRectRef.current = nextRect;
        setBoxSelectRect(nextRect);
      }
      return;
    }

    const moved = orbitDragRef.current.moved || Math.abs(dx) > 2 || Math.abs(dy) > 2;
    if (measurementMode && !moved) return;
    orbitDragRef.current = {
      ...orbitDragRef.current,
      active: true,
      moved,
      x: event.clientX,
      y: event.clientY,
    };

    if (mode === "pan") {
      setCamera((prev) => ({
        ...prev,
        panX: prev.panX + dx,
        panY: prev.panY + dy,
      }));
    } else if (mode === "zoom") {
      const fitZoom = computeFitZoom();
      const minZoom = Math.max(0.05, fitZoom * 0.15);
      const maxZoom = Math.min(80, fitZoom * 12);
      const factor = Math.exp(-dy * 0.008);
      setCamera((prev) => ({
        ...prev,
        zoom: Math.max(minZoom, Math.min(maxZoom, prev.zoom * factor)),
      }));
    } else if (mode === "orbit") {
      setCamera((prev) => ({
        ...prev,
        yaw: prev.yaw + dx * 0.01,
        pitch: Math.max(-1.45, Math.min(1.45, prev.pitch + dy * 0.01)),
      }));
    }
  };

  const finishBoxSelect = (rect: { readonly startX: number; readonly startY: number; readonly endX: number; readonly endY: number }, additive: boolean) => {
    if (cadWebglFrameRef.current || cms2WebgpuFrameRef.current) {
      ensurePickProjection(viewportSize.width, viewportSize.height);
    }
    const mode = resolveBoxSelectMode(rect.startX, rect.endX);
    const minX = Math.min(rect.startX, rect.endX);
    const maxX = Math.max(rect.startX, rect.endX);
    const minY = Math.min(rect.startY, rect.endY);
    const maxY = Math.max(rect.startY, rect.endY);
    const inside = (x: number, y: number) => x >= minX && x <= maxX && y >= minY && y <= maxY;

    const picked: NonNullable<SelectionTarget>[] = [];
    const seen = new Set<string>();

    if (selectionFilter === "edge") {
      for (const seg of projectedEdgesRef.current) {
        if (seen.has(seg.edge.edge_id)) continue;
        const aIn = inside(seg.a[0], seg.a[1]);
        const bIn = inside(seg.b[0], seg.b[1]);
        const matched = mode === "window" ? (aIn && bIn) : (aIn || bIn);
        if (matched) {
          seen.add(seg.edge.edge_id);
          picked.push({ kind: "edge", edge: seg.edge });
        }
      }
    } else {
      for (const tri of projectedTrisRef.current) {
        if (seen.has(tri.face.face_id)) continue;
        const flags = tri.pts.map(([x, y]) => inside(x, y));
        const matched = mode === "window" ? flags.every(Boolean) : flags.some(Boolean);
        if (matched) {
          seen.add(tri.face.face_id);
          picked.push({ kind: "face", face: tri.face });
        }
      }
    }

    if (picked.length === 0) {
      if (!additive) applySelectionPick(null, false, null);
      return;
    }

    const baseList = additive ? [...effectiveSelectedTargets, ...picked] : picked;
    const dedup = new Map<string, NonNullable<SelectionTarget>>();
    for (const item of baseList) {
      dedup.set(item.kind === "face" ? `f:${item.face.face_id}` : `e:${item.edge.edge_id}`, item);
    }
    const finalTargets = Array.from(dedup.values());
    const primary = finalTargets[finalTargets.length - 1] ?? null;
    onSelectTarget(primary);
    onChangeSelectedTargets?.(finalTargets);
    if (primary && !dashboardActive && !measurementMode) {
      setMiniToolbarPos({ x: rect.endX, y: rect.endY });
    }
  };

  const finishCanvasPointer = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary) return;
    const holeDrag = holeDragRef.current;
    if (holeDrag?.pointerId === event.pointerId) {
      holeDragRef.current = null;
      setDraggingBadge(null);
      if (event.type === "pointerup" && !measurementMode) {
        const delta = holeDragDelta(event.clientX - holeDrag.startX, holeDrag.handle.radius_mm);
        if (delta !== 0) void holeDrag.commit(holeDrag.scene, holeDrag.handle, delta);
      }
      return;
    }
    if (handleDragRef.current.handle) {
      const { handle, source, startValue, latestValue, moved } = handleDragRef.current;
      handleDragRef.current = {
        handle: null,
        startX: 0,
        startValue: 0,
        latestValue: 0,
        moved: false,
      };
      setDraggingBadge(null);
      if (event.type === "pointerup" && moved && source === kclSource && latestValue !== startValue) {
        commitParameter(handle, latestValue);
      } else {
        setDraftValues((prev) => {
          const next = { ...prev };
          delete next[handle.handle_id];
          return next;
        });
      }
      return;
    }

    const drag = orbitDragRef.current;
    orbitDragRef.current.active = false;

    if (drag.mode === "radial-gesture") {
      const latestRadial = radialStateRef.current;
      radialStateRef.current = null;
      if (event.type === "pointerup" && latestRadial?.direction) {
        const slot = RADIAL_MENU_SLOTS.find((s) => s.direction === latestRadial.direction);
        if (slot) onExecuteViewportCommand?.(slot.commandId);
      }
      setRadialState(null);
      return;
    }

    if (drag.mode === "box-select") {
      const latestRect = boxSelectRectRef.current;
      boxSelectRectRef.current = null;
      if (event.type === "pointerup" && latestRect) {
        finishBoxSelect(latestRect, event.ctrlKey || event.metaKey);
      }
      setBoxSelectRect(null);
      return;
    }

    // NX / Creo Middle Mouse Button gestures when clicked without dragging:
    // - Double-click MMB -> Fit to View
    // - Single-click MMB when Dashboard/active tool is open -> Confirm / OK!
    if (event.type === "pointerup" && drag.button === 1 && !drag.moved) {
      const now = Date.now();
      if (now - lastMmbClickAtRef.current < 320) {
        lastMmbClickAtRef.current = 0;
        fitToView();
      } else {
        lastMmbClickAtRef.current = now;
        if (navigationPreset === "nx-creo" && onConfirmActiveTool && dashboardActive) {
          onConfirmActiveTool();
        }
      }
    }
  };

  const handleCanvasClick = (event: ReactMouseEvent<HTMLCanvasElement>) => {
    if (orbitDragRef.current.moved) {
      orbitDragRef.current.moved = false;
      return;
    }
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    const rect = canvas.getBoundingClientRect();
    const scaleX = rect.width > 0 ? viewportSize.width / rect.width : 1;
    const scaleY = rect.height > 0 ? viewportSize.height / rect.height : 1;
    const px = (event.clientX - rect.left) * scaleX;
    const py = (event.clientY - rect.top) * scaleY;

    const rejectPick = (text: string) => setRulerHint({ scene: sceneMesh, text });
    if (measurementMode === "circle") {
      const pick = pickCircle(projectedCirclesRef.current, px, py);
      if (!pick) return rejectPick("请点击高亮的圆或圆弧边缘。");
      const first = rulerSelection?.kind === "circle" && !rulerResult ? rulerSelection.first : null;
      if (first === pick) return rejectPick("请选择另一条圆边。");
      setRulerHint(null);
      setRulerState(first ? {
        scene: sceneMesh, selection: { kind: "circle", first, second: pick },
        result: measureCircles(first, pick, sceneMesh.units),
      } : { scene: sceneMesh, selection: { kind: "circle", first: pick } });
      return;
    }

    const additive = event.ctrlKey || event.metaKey;
    const miniAnchor = { x: px, y: py };

    if (!measurementMode && cms2WebgpuFrameRef.current && cms2WebgpuRendererRef.current && compactScene) {
      const requestId = ++cms2PickRequestIdRef.current;
      const cms2Camera: Cms2CameraState = {
        yaw: camera.yaw,
        pitch: camera.pitch,
        zoom: camera.zoom,
        panX: camera.panX,
        panY: camera.panY,
        cx: viewportSize.width / 2,
        cy: viewportSize.height / 2 + 12,
        width: viewportSize.width,
        height: viewportSize.height,
        projection: resolvedDisplay.projection,
        center: meshBounds.center,
        modelRadius: meshBounds.radius,
      };
      const cms2FrameState = buildCms2FrameState({
        scene: compactScene,
        displayState: resolvedDisplay,
        sectionCut,
        cutCoord,
        explodeFactor,
        selectedTarget,
        hoveredTarget,
        selectedComponentId,
        changePreview,
        wboitOverlays,
      });
      void cms2WebgpuRendererRef.current
        .pickPixel(cms2Camera, px, py, cms2FrameState)
        .then((pick) => {
          if (cms2PickRequestIdRef.current !== requestId) return;
          const resolved = resolveCms2PickToViewportTarget(pick, sceneMesh, selectionFilter);
          if (resolved && resolved.target) {
            onSelectComponent?.(resolved.componentId);
            applySelectionPick(resolved.target, additive, miniAnchor);
          } else if (!additive) {
            applySelectionPick(null, false, null);
          }
        })
        .catch(() => {
          if (cms2PickRequestIdRef.current !== requestId) return;
          if (!additive) applySelectionPick(null, false, null);
        });
      return;
    }

    if (cadWebglFrameRef.current || cms2WebgpuFrameRef.current) {
      ensurePickProjection(viewportSize.width, viewportSize.height);
    }

    // Direct 3D Edge Picking when not measuring (respects selectionFilter & multi-select)
    if (!measurementMode && !assemblyFrameRef.current && (selectionFilter === "all" || selectionFilter === "edge")) {
      const edgeTol = selectionFilter === "edge" ? 9 : 6;
      const hitEdge = pickProjectedEdge(projectedEdgesRef.current, px, py, edgeTol);
      if (hitEdge) {
        onSelectComponent?.(null);
        applySelectionPick({ kind: "edge", edge: hitEdge }, additive, miniAnchor);
        return;
      }
      if (selectionFilter === "edge") {
        if (!additive) applySelectionPick(null, false, null);
        return;
      }
    }

    const tris = assemblyFrameRef.current
      ? projectTriangles(viewportSize.width / 2, viewportSize.height / 2 + 12, [px, py])
      : projectedTrisRef.current;
    for (let i = tris.length - 1; i >= 0; i -= 1) {
      const tri = tris[i];
      const weights = triangleHitWeights(px, py, ...tri.pts);
      if (!weights) continue;
      if (measurementMode === "face") {
        if (!tri.worldPoints) return;
        const [a, b, c] = tri.worldPoints;
        const [ox, oy, oz] = computeComponentExplodeOffset(tri.face.component_id, componentOrder, explodeFactor);
        const point: Vec3 = [
          a[0] * weights[0] + b[0] * weights[1] + c[0] * weights[2] - ox,
          a[1] * weights[0] + b[1] * weights[1] + c[1] * weights[2] - oy,
          a[2] * weights[0] + b[2] * weights[1] + c[2] * weights[2] - oz,
        ];
        const pick = makePlanePick(tri.face, point, sceneMesh.units);
        if (!pick) return rejectPick("面间距仅支持平面，请选择平整的面。");
        const first = rulerSelection?.kind === "face" && !rulerResult ? rulerSelection.first : null;
        if (first?.face === pick.face) return rejectPick("请选择另一个平面。");
        if (first) {
          const result = measurePlanes(first, pick, sceneMesh.units);
          if (!result) return rejectPick("两面不平行，请选择与第一个面平行的面。");
          setRulerState({ scene: sceneMesh, selection: { kind: "face", first, second: pick }, result });
        } else {
          setRulerState({ scene: sceneMesh, selection: { kind: "face", first: pick } });
        }
        setRulerHint(null);
      } else if (selectionFilter === "body") {
        if (tri.face.component_id) {
          onSelectComponent?.(tri.face.component_id);
        }
        applySelectionPick({ kind: "face", face: tri.face }, additive, miniAnchor);
      } else {
        onSelectComponent?.(null);
        applySelectionPick({ kind: "face", face: tri.face }, additive, miniAnchor);
      }
      return;
    }
    if (measurementMode === "face") return rejectPick("请点击视口中的平面。");
    if (!additive) applySelectionPick(null, false, null);
  };

  const clearRuler = useCallback(() => {
    setRulerState(null);
    setRulerHint(null);
    setCopyFeedback(null);
  }, []);

  const fitToView = useCallback(() => {
    const zoom = computeFitZoom();
    animateCameraTo({ ...cameraRef.current, zoom, panX: 0, panY: 0 });
  }, [animateCameraTo, computeFitZoom]);

  const normalToSelection = useCallback(() => {
    if (selectedTarget?.kind !== "face") return;
    const norms = selectedTarget.face.normals;
    if (norms.length < 3) return;
    let nx = 0;
    let ny = 0;
    let nz = 0;
    const count = Math.floor(norms.length / 3);
    for (let i = 0; i < count * 3; i += 3) {
      nx += norms[i];
      ny += norms[i + 1];
      nz += norms[i + 2];
    }
    const len = Math.hypot(nx, ny, nz);
    if (len < 1e-6) return;
    nx /= len;
    ny /= len;
    nz /= len;
    const pitch = Math.max(-1.55, Math.min(1.55, Math.asin(Math.max(-1, Math.min(1, nz)))));
    const yaw = Math.hypot(nx, ny) > 1e-4 ? yawForOutwardNormal(nx, ny) : 0;
    animateCameraTo({ ...cameraRef.current, yaw, pitch, panX: 0, panY: 0 });
  }, [animateCameraTo, selectedTarget]);

  const zoomBy = useCallback((factor: number) => {
    const fitZoom = computeFitZoom();
    const minZoom = Math.max(0.05, fitZoom * 0.15);
    const maxZoom = Math.min(80, fitZoom * 12);
    const nextZoom = Math.max(minZoom, Math.min(maxZoom, cameraRef.current.zoom * factor));
    animateCameraTo({ ...cameraRef.current, zoom: nextZoom }, 150);
  }, [animateCameraTo, computeFitZoom]);

  const rotateStep = useCallback((deltaYaw: number, deltaPitch = 0) => {
    const cur = cameraRef.current;
    animateCameraTo({
      ...cur,
      yaw: cur.yaw + deltaYaw,
      pitch: Math.max(-1.55, Math.min(1.55, cur.pitch + deltaPitch)),
    }, 180);
  }, [animateCameraTo]);

  const orbitBy = useCallback((deltaYaw: number, deltaPitch: number) => {
    cancelCameraAnim();
    setCamera((prev) => ({
      ...prev,
      yaw: prev.yaw + deltaYaw,
      pitch: Math.max(-1.55, Math.min(1.55, prev.pitch + deltaPitch)),
    }));
  }, [cancelCameraAnim]);

  useImperativeHandle(ref, () => ({
    applyPreset: (preset) => {
      const base = VIEW_PRESETS[preset];
      animateCameraTo({ yaw: base.yaw, pitch: base.pitch, zoom: computeFitZoom(), panX: 0, panY: 0 });
    },
    fitToView,
    normalToSelection,
    clearMeasurement: clearRuler,
    zoomBy,
    rotateStep,
    orbitBy,
  }), [animateCameraTo, clearRuler, computeFitZoom, fitToView, normalToSelection, orbitBy, rotateStep, zoomBy]);

  const copyMeasurement = async () => {
    if (!measurementMode || !rulerResult) return;
    try {
      await navigator.clipboard.writeText(measurementConst(measurementMode, rulerResult.distanceMm));
      setCopyFeedback({ result: rulerResult, text: "已复制 DSL 常量。" });
    } catch {
      setCopyFeedback({ result: rulerResult, text: "无法访问剪贴板，请手动复制。" });
    }
  };

  const currentHandleValue = activeHandle
    ? (draftValues[activeHandle.handle_id] ?? activeHandle.current_value)
    : 0;
  const measurementFeedback = copyFeedback?.result === rulerResult ? copyFeedback?.text : undefined;
  const measurementHint = rulerHint?.scene === sceneMesh ? rulerHint.text : undefined;
  const measurementPrompt = measurementMode === "circle"
    ? (circles.length === 0 ? "当前模型没有可测量的圆边。" : `请选择${rulerSelection ? "第二" : "第一"}条圆边或圆弧。圆心由视口采样点计算。`)
    : `请选择第${rulerSelection ? "二" : "一"}个平面。仅测量平行平面之间的垂直距离。`;

  const activeFilterMeta = SELECTION_FILTER_OPTIONS.find((item) => item.id === selectionFilter) ?? SELECTION_FILTER_OPTIONS[0];
  const isHlrStage = resolvedDisplay.visualStyle === "hlr";
  const backingWidth = Math.max(1, Math.round(viewportSize.width * dpr));
  const backingHeight = Math.max(1, Math.round(viewportSize.height * dpr));

  return (
    <main className="panel panel-center" aria-label="3D 双轨视口与参数手柄交互区">
      {/* 3D 视口画布 / 2D 工程图画布 */}
      <div className={`canvas-stage${isHlrStage ? " is-hlr" : ""}`}>
        {measurementMode && viewportTab === "3d" && (
          <section className="viewport-measurement viewport-measurement-floating" aria-label="测量结果">
            <div role="status">
              {rulerResult ? (
                <>
                  <strong>{measurementMode === "circle" ? "圆心距" : "平行面间距"} ≈ {measurementDecimal(rulerResult.distanceMm)} mm</strong>
                  <span> · {measurementMode === "circle" ? "按视口采样点计算" : "按平面法向计算"}</span>
                  {explodeFactor > 0 && isAssembly && <span> · 不计爆炸图位移</span>}
                  {measurementFeedback && <span> · {measurementFeedback}</span>}
                </>
              ) : (measurementHint ?? measurementPrompt)}
            </div>
            {rulerResult && (
              <>
                <button type="button" className="pill-btn" onClick={copyMeasurement}>复制为 DSL const</button>
                <input
                  className="viewport-measurement-const"
                  aria-label="测量 DSL 常量"
                  readOnly
                  value={measurementConst(measurementMode, rulerResult.distanceMm)}
                  onFocus={(event) => event.currentTarget.select()}
                />
              </>
            )}
            {rulerSelection && <button type="button" className="pill-btn" onClick={clearRuler}>清除测量</button>}
            <button
              type="button"
              className="pill-btn viewport-exit-tool"
              onClick={() => {
                clearRuler();
                onExitMeasurement();
              }}
            >
              退出测量 Esc
            </button>
          </section>
        )}
        {viewportTab === "3d" ? (
          <>
            <canvas
              ref={cms2WebgpuCanvasRef}
              width={backingWidth}
              height={backingHeight}
              className="viewport-canvas webgpu-canvas"
              aria-hidden="true"
            />
            <canvas
              ref={cadWebglCanvasRef}
              width={backingWidth}
              height={backingHeight}
              className="viewport-canvas webgl-canvas"
              aria-hidden="true"
            />
            {isAssembly && <canvas
              ref={assemblyCanvasRef}
              width={backingWidth}
              height={backingHeight}
              className="viewport-canvas assembly-canvas"
              aria-hidden="true"
            />}
            <canvas
              ref={canvasRef}
              width={backingWidth}
              height={backingHeight}
              aria-label="交互式三维模型"
              className={`viewport-canvas${measurementMode ? " measuring" : ""}${holeDragRef.current && draggingBadge ? " resizing-hole" : ""}`}
              onClick={handleCanvasClick}
              onContextMenu={(event) => {
                event.preventDefault();
                if (suppressNextContextMenuRef.current) {
                  suppressNextContextMenuRef.current = false;
                  return;
                }
                if (onViewportContextMenu) {
                  onViewportContextMenu(event);
                }
              }}
              onPointerDown={handleCanvasPointerDown}
              onPointerMove={handleCanvasPointerMove}
              onPointerUp={finishCanvasPointer}
              onPointerCancel={finishCanvasPointer}
              onPointerLeave={() => {
                setHoveredTarget(null);
                lastHoverLocalRef.current = null;
              }}
              onLostPointerCapture={finishCanvasPointer}
              onDoubleClick={() => {
                if (navigationPreset === "web-trackpad" || !selectedTarget) {
                  fitToView();
                }
              }}
              onWheel={(e) => {
                e.preventDefault();
                cancelCameraAnim();
                const rect = e.currentTarget.getBoundingClientRect();
                const cursorX = rect.width > 0 ? (e.clientX - rect.left) * (viewportSize.width / rect.width) : viewportSize.width / 2;
                const cursorY = rect.height > 0 ? (e.clientY - rect.top) * (viewportSize.height / rect.height) : viewportSize.height / 2;
                const fitZoom = computeFitZoom();
                const minZoom = Math.max(0.05, fitZoom * 0.15);
                const maxZoom = Math.min(80, fitZoom * 12);
                const factor = Math.exp(-e.deltaY * 0.0012);
                setCamera((prev) => {
                  const nextZoom = Math.max(minZoom, Math.min(maxZoom, prev.zoom * factor));
                  const zoomed = zoomAroundCursor({
                    cursorX,
                    cursorY,
                    cx: viewportSize.width / 2,
                    cy: viewportSize.height / 2 + 12,
                    panX: prev.panX,
                    panY: prev.panY,
                    prevZoom: prev.zoom,
                    nextZoom,
                  });
                  return {
                    ...prev,
                    zoom: zoomed.zoom,
                    panX: zoomed.panX,
                    panY: zoomed.panY,
                  };
                });
              }}
            />
            {draggingBadge && (
              <div className="drag-hud-badge">拖拽参数: {draggingBadge}</div>
            )}
            {topViewportOverlay}
            <MiniToolbar
              target={selectedTarget}
              position={miniToolbarPos}
              selectedCount={effectiveSelectedTargets.length}
              onExecute={(commandId) => onExecuteViewportCommand?.(commandId)}
              onDismiss={() => setMiniToolbarPos(null)}
            />
            <RadialMenu
              origin={radialState?.origin ?? null}
              activeDirection={radialState?.direction ?? null}
            />
            <QuickPickPopover
              position={quickPickState?.position ?? null}
              candidates={quickPickState?.candidates ?? []}
              activeIndex={quickPickState?.activeIndex ?? 0}
              onHoverCandidate={(cand, idx) => {
                setQuickPickState((cur) => (cur ? { ...cur, activeIndex: idx } : null));
                setHoveredTarget(cand.target);
              }}
              onPickCandidate={(cand) => {
                const pos = quickPickState?.position ?? null;
                applySelectionPick(cand.target, false, pos);
              }}
              onClose={() => setQuickPickState(null)}
            />
          </>
        ) : (
          <div
            className="drawing-stage"
            aria-label="2D 工程图纸视图"
            style={{ alignItems: "flex-start", justifyContent: "flex-start", overflow: "auto", paddingTop: 52 }}
          >
            <div style={{ width: "100%", minWidth: 0 }}>
              <AssociativeDrawingPanel
                sheet={drawingSheet}
                onSelectTopoRef={selectDrawingTopoRef}
              />
            </div>
          </div>
        )}
        {hudOverlay}
        {rollbackLabel ? (
          <div
            className="rollback-topo-badge status-pill is-error"
            role="status"
            data-node-id={rollbackNode ?? undefined}
            title={rollbackNode
              ? "节点 " + rollbackNode + " 已回退，视口仍显示上一次成功的实体"
              : "视口仍显示上一次成功的实体"}
            style={{
              position: "absolute",
              left: "50%",
              top: 54,
              transform: "translateX(-50%)",
              zIndex: 16,
              pointerEvents: "none",
              maxWidth: "calc(100% - 120px)",
              padding: "6px 12px",
              border: "1px solid #f87171",
              background: "rgba(127, 29, 29, 0.94)",
              color: "#fecaca",
              fontSize: 12,
              fontWeight: 600,
              lineHeight: 1.35,
              whiteSpace: "normal",
              textAlign: "center",
              boxShadow: "var(--shadow-md, 0 8px 20px rgba(0, 0, 0, 0.28))",
            }}
          >
            {rollbackLabel}
          </div>
        ) : null}

        {activeHandle && viewportTab === "3d" && (
          <section className={`handle-panel handle-panel-floating${handlePanelOpen ? " is-open" : " is-collapsed"}`} aria-label="模型参数">
            {handlePanelOpen ? (
              <>
                <div className="handle-overlay-header">
                  <span className={`tier-pill tier-${activeHandle.tier}`}>
                    Tier {activeHandle.tier}
                  </span>
                  <span className="handle-title">
                    {activeHandle.label ?? activeHandle.param_name}
                  </span>
                  <span className="handle-value-badge" style={{ marginLeft: "auto" }}>
                    {currentHandleValue.toFixed(1)} {activeHandle.unit}
                  </span>
                  <button
                    type="button"
                    className="pill-btn"
                    onClick={() => setHandlePanelOpen(false)}
                  >
                    收起
                  </button>
                </div>

                <div className="handle-controls-row">
                  <input
                    type="range"
                    min={activeHandle.min_value ?? 5}
                    max={activeHandle.max_value ?? 160}
                    step={0.5}
                    value={currentHandleValue}
                    onPointerDown={(event) => {
                      if (!event.isPrimary || event.button !== 0) return;
                      sliderDrag.current = { pointerId: event.pointerId };
                      event.currentTarget.setPointerCapture(event.pointerId);
                    }}
                    onPointerUp={(event) => {
                      const drag = sliderDrag.current;
                      if (drag?.pointerId !== event.pointerId) return;
                      sliderDrag.current = null;
                      if (drag.value !== undefined) commitParameter(activeHandle, drag.value);
                    }}
                    onLostPointerCapture={(event) => {
                      if (sliderDrag.current?.pointerId !== event.pointerId) return;
                      sliderDrag.current = null;
                      setDraftValues((prev) => {
                        const next = { ...prev };
                        delete next[activeHandle.handle_id];
                        return next;
                      });
                    }}
                    onChange={(event) => {
                      const value = Number(event.currentTarget.value);
                      if (sliderDrag.current) {
                        // Keep pointer scrubbing local; source commits replace the current mesh.
                        sliderDrag.current.value = value;
                        setDraftValues((prev) => ({ ...prev, [activeHandle.handle_id]: value }));
                      } else {
                        commitParameter(activeHandle, value);
                      }
                    }}
                    aria-label={`${activeHandle.param_name} 滑块`}
                  />
                </div>

                <div className="handle-selector-chips">
                  {sceneMesh.edit_handles.map((h) => (
                    <button
                      key={h.handle_id}
                      type="button"
                      className={`handle-chip tier-${h.tier} ${
                        h.handle_id === activeHandle.handle_id ? "active" : ""
                      }`}
                      onClick={() => {
                        setActiveHandleId(h.handle_id);
                        onFocusAstSpan(h.ast_span, h.param_name);
                      }}
                    >
                      T{h.tier} · {h.param_name}
                    </button>
                  ))}
                </div>
              </>
            ) : (
              <div className="handle-selector-chips">
                <button
                  type="button"
                  className="handle-chip active"
                  title="展开 3D 参数手柄快速调节器"
                  onClick={() => setHandlePanelOpen(true)}
                >
                  参数手柄 ({sceneMesh.edit_handles.length}) · {activeHandle.param_name}: {currentHandleValue.toFixed(1)}{activeHandle.unit}
                </button>
              </div>
            )}
          </section>
        )}

        <section className="selection-status-bar selection-status-floating" aria-label="选择状态">
          <div className="selection-summary">
            {viewportTab === "3d" && (
              <span
                className="badge badge-backend"
                data-render-backend={renderBackendStatus.backend}
                title={renderBackendStatus.reason}
              >
                {renderBackendStatus.label}
              </span>
            )}
            {viewportTab === "3d" && selectionFilter !== "all" && (
              <span className="badge badge-filter" title={activeFilterMeta.description}>
                过滤：{activeFilterMeta.label}
              </span>
            )}
            {selectedTarget ? (
              selectedTarget.kind === "face" ? (
                <>
                  <span className="badge badge-accent">FACE</span>
                  <strong>{selectedTarget.face.face_id}</strong>
                  <span>({selectedTarget.face.surface_type})</span>
                  {selectedHole && <span className="hole-offset-hint">左右拖动蓝色手柄调整孔半径</span>}
                  <span className="tags-inline">
                    Tags: {selectedTarget.face.provenance_tags.join(", ")}
                  </span>
                  <span className="diag-loc">
                    AST L{selectedTarget.face.ast_span.start_line}..L
                    {selectedTarget.face.ast_span.end_line}
                  </span>
                </>
              ) : (
                <>
                  <span className="badge badge-sync">EDGE</span>
                  <strong>{selectedTarget.edge.edge_id}</strong>
                  <span>({selectedTarget.edge.curve_type})</span>
                  <span className="tags-inline">
                    Tags: {selectedTarget.edge.provenance_tags.join(", ")}
                  </span>
                </>
              )
            ) : hoveredTarget ? (
              <span className="muted-text">
                悬停预览：{hoveredTarget.kind === "face"
                  ? `面 ${hoveredTarget.face.face_id} (${hoveredTarget.face.surface_type})`
                  : `边 ${hoveredTarget.edge.edge_id} (${hoveredTarget.edge.curve_type})`}
              </span>
            ) : (
              <span className="muted-text">
                未选择几何{viewportTab === "3d" && ` · ${getNavigationPresetMeta(navigationPreset).idleHint}`}
              </span>
            )}
          </div>
        </section>
      </div>
    </main>
  );
}
