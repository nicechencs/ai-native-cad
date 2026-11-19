import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  applyTier2Choice,
  applyTier3ExtractParameter,
  computeWritebackPlan,
  type Tier2ChoiceKind,
  type Tier2WritebackResult,
  type Tier3WritebackResult,
} from "../sync/astWriteback.ts";
import type {
  BoundingBox3D,
  EditHandle,
  FaceMesh,
  SceneMeshPayload,
  SectionCutState,
  SelectionTarget,
  SourceSpan,
  TopoEdge,
  Vec3,
} from "../types/scene.ts";
import { clipToSection, handleRange, normalizePolyline } from "../utils/studioHelpers.ts";

export interface CenterViewportProps {
  readonly kclSource: string;
  readonly sceneMesh: SceneMeshPayload;
  readonly keepInBbox: BoundingBox3D;
  readonly drawingSvg?: string;
  readonly selectedComponentId?: string | null;
  readonly selectedTarget: SelectionTarget;
  readonly onSelectTarget: (target: SelectionTarget) => void;
  readonly onCommitKclSource: (nextSource: string, summary: string) => void;
  readonly onFocusAstSpan: (span: SourceSpan, label: string) => void;
}

interface ProjectedTriangle {
  readonly face: FaceMesh;
  readonly pts: readonly [
    readonly [number, number],
    readonly [number, number],
    readonly [number, number],
  ];
  readonly depth: number;
  readonly shade: number;
}

interface ProjectedHandleAnchor {
  readonly handle: EditHandle;
  readonly x: number;
  readonly y: number;
  readonly radius: number;
}

interface CameraState {
  readonly yaw: number;
  readonly pitch: number;
  readonly zoom: number;
}

type ViewPreset = "iso" | "top" | "front" | "right";
type ViewportTab = "3d" | "drawing";

const VIEW_PRESETS: Readonly<Record<ViewPreset, CameraState>> = {
  iso: { yaw: 0.65, pitch: 0.58, zoom: 2.35 },
  top: { yaw: 0, pitch: Math.PI / 2 - 0.001, zoom: 2.4 },
  front: { yaw: 0, pitch: 0.02, zoom: 2.4 },
  right: { yaw: Math.PI / 2, pitch: 0.02, zoom: 2.4 },
};

function projectPoint(
  x: number,
  y: number,
  z: number,
  camera: CameraState,
  cx: number,
  cy: number,
  centerZ: number,
): readonly [number, number, number] {
  const cz = z - centerZ;
  const cosY = Math.cos(camera.yaw);
  const sinY = Math.sin(camera.yaw);
  const cosP = Math.cos(camera.pitch);
  const sinP = Math.sin(camera.pitch);

  const x1 = x * cosY - y * sinY;
  const y1 = x * sinY + y * cosY;
  const y2 = y1 * cosP - cz * sinP;
  const z2 = y1 * sinP + cz * cosP;

  return [cx + x1 * camera.zoom, cy - z2 * camera.zoom, y2];
}

function pointInTriangle(
  px: number,
  py: number,
  a: readonly [number, number],
  b: readonly [number, number],
  c: readonly [number, number],
): boolean {
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
  if (Math.abs(denom) < 1e-6) {
    return false;
  }
  const invDenom = 1 / denom;
  const u = (dot11 * dot02 - dot01 * dot12) * invDenom;
  const v = (dot00 * dot12 - dot01 * dot02) * invDenom;
  return u >= 0 && v >= 0 && u + v <= 1;
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
  kclSource,
  sceneMesh,
  keepInBbox,
  drawingSvg,
  selectedComponentId,
  selectedTarget,
  onSelectTarget,
  onCommitKclSource,
  onFocusAstSpan,
}: CenterViewportProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const projectedTrisRef = useRef<readonly ProjectedTriangle[]>([]);
  const projectedHandlesRef = useRef<readonly ProjectedHandleAnchor[]>([]);
  const orbitDragRef = useRef<{ active: boolean; moved: boolean; x: number; y: number }>({
    active: false,
    moved: false,
    x: 0,
    y: 0,
  });
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

  const [viewportTab, setViewportTab] = useState<ViewportTab>("3d");
  const [camera, setCamera] = useState<CameraState>(VIEW_PRESETS.iso);
  const [showKeepInBox, setShowKeepInBox] = useState(true);
  const [sectionCut, setSectionCut] = useState<SectionCutState>({
    enabled: false,
    axis: "Z",
    ratio: 0.55,
  });
  const [explodeFactor, setExplodeFactor] = useState<number>(0);
  const [activeHandleId, setActiveHandleId] = useState<string>(
    sceneMesh.edit_handles[0]?.handle_id ?? "",
  );
  const [draftValues, setDraftValues] = useState<Readonly<Record<string, number>>>({});
  const [draggingBadge, setDraggingBadge] = useState<string | null>(null);
  const [pendingTier2, setPendingTier2] = useState<Tier2WritebackResult | null>(null);
  const [pendingTier3, setPendingTier3] = useState<Tier3WritebackResult | null>(null);
  const [extractNameInput, setExtractNameInput] = useState<string>("fillet_r_override");

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

  const isAssembly =
    sceneMesh.kind === "assembly" ||
    (sceneMesh.components?.length ?? 0) > 1 ||
    componentOrder.size > 1;

  const meshBounds = useMemo(() => {
    let minX = Infinity;
    let maxX = -Infinity;
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
        minZ = Math.min(minZ, z);
        maxZ = Math.max(maxZ, z);
        maxR = Math.max(maxR, Math.hypot(x, y));
      }
    }
    if (!Number.isFinite(minX)) {
      return { minX: -50, maxX: 50, minZ: 0, maxZ: 50, maxR: 50 };
    }
    return { minX, maxX, minZ, maxZ, maxR };
  }, [sceneMesh]);

  const activeHandle: EditHandle | undefined = useMemo(
    () =>
      sceneMesh.edit_handles.find((h) => h.handle_id === activeHandleId) ??
      sceneMesh.edit_handles[0],
    [sceneMesh.edit_handles, activeHandleId],
  );

  // R19: Clear stale draft slider values and pending writeback popovers whenever
  // source or evaluated mesh updates (e.g. demo reset or upstream parameter edit).
  useEffect(() => {
    setDraftValues({});
    setDraggingBadge(null);
    setPendingTier2(null);
    setPendingTier3(null);
  }, [kclSource, sceneMesh]);

  // Render 3D scene mesh + B-Rep edges + Section Cut + Exploded View + Direct 3D Handle anchors
  useEffect(() => {
    if (viewportTab !== "3d") {
      return;
    }
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      return;
    }

    const width = canvas.width;
    const height = canvas.height;
    const cx = width / 2;
    const cy = height / 2 + 12;
    const centerZ = (meshBounds.minZ + meshBounds.maxZ) / 2;

    const bg = ctx.createRadialGradient(cx, cy - 30, 30, cx, cy, width * 0.72);
    bg.addColorStop(0, "#162032");
    bg.addColorStop(1, "#090d14");
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, width, height);

    // Subtle ground grid on Z = 0
    ctx.strokeStyle = "rgba(110, 140, 180, 0.14)";
    ctx.lineWidth = 1;
    for (let g = -80; g <= 80; g += 20) {
      const p1 = projectPoint(g, -80, 0, camera, cx, cy, centerZ);
      const p2 = projectPoint(g, 80, 0, camera, cx, cy, centerZ);
      ctx.beginPath();
      ctx.moveTo(p1[0], p1[1]);
      ctx.lineTo(p2[0], p2[1]);
      ctx.stroke();

      const q1 = projectPoint(-80, g, 0, camera, cx, cy, centerZ);
      const q2 = projectPoint(80, g, 0, camera, cx, cy, centerZ);
      ctx.beginPath();
      ctx.moveTo(q1[0], q1[1]);
      ctx.lineTo(q2[0], q2[1]);
      ctx.stroke();
    }

    // Keep-In bounding box wireframe
    if (showKeepInBox) {
      const [minX, minY, minZ] = keepInBbox.min;
      const [maxX, maxY, maxZ] = keepInBbox.max;
      const corners: readonly (readonly [number, number, number])[] = [
        [minX, minY, minZ],
        [maxX, minY, minZ],
        [maxX, maxY, minZ],
        [minX, maxY, minZ],
        [minX, minY, maxZ],
        [maxX, minY, maxZ],
        [maxX, maxY, maxZ],
        [minX, maxY, maxZ],
      ];
      const boxEdges: readonly (readonly [number, number])[] = [
        [0, 1], [1, 2], [2, 3], [3, 0],
        [4, 5], [5, 6], [6, 7], [7, 4],
        [0, 4], [1, 5], [2, 6], [3, 7],
      ];
      ctx.save();
      ctx.setLineDash([4, 4]);
      ctx.strokeStyle = "rgba(56, 189, 248, 0.32)";
      ctx.lineWidth = 1;
      for (const [i, j] of boxEdges) {
        const pa = projectPoint(corners[i][0], corners[i][1], corners[i][2], camera, cx, cy, centerZ);
        const pb = projectPoint(corners[j][0], corners[j][1], corners[j][2], camera, cx, cy, centerZ);
        ctx.beginPath();
        ctx.moveTo(pa[0], pa[1]);
        ctx.lineTo(pb[0], pb[1]);
        ctx.stroke();
      }
      ctx.restore();
    }

    const cutCoord =
      sectionCut.axis === "Z"
        ? meshBounds.minZ + sectionCut.ratio * (meshBounds.maxZ - meshBounds.minZ)
        : meshBounds.minX + sectionCut.ratio * (meshBounds.maxX - meshBounds.minX);

    const triangles: ProjectedTriangle[] = [];
    const lightDir: readonly [number, number, number] = [0.42, -0.52, 0.74];

    for (const face of sceneMesh.faces) {
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
          ? clipToSection(points, sectionCut.axis === "Z" ? 2 : 0, cutCoord)
          : points;
        if (visible.length < 3) continue;
        const projected = visible.map((point) => projectPoint(...point, camera, cx, cy, centerZ));

        const nx = (norms[i0] + norms[i1] + norms[i2]) / 3;
        const ny = (norms[i0 + 1] + norms[i1 + 1] + norms[i2 + 1]) / 3;
        const nz = (norms[i0 + 2] + norms[i1 + 2] + norms[i2 + 2]) / 3;
        const dot = Math.abs(nx * lightDir[0] + ny * lightDir[1] + nz * lightDir[2]);
        const shade = Math.min(1, 0.32 + dot * 0.68);

        for (let i = 1; i + 1 < projected.length; i += 1) {
          const [a, b, c] = [projected[0], projected[i], projected[i + 1]];
          triangles.push({
            face,
            pts: [[a[0], a[1]], [b[0], b[1]], [c[0], c[1]]],
            depth: (a[2] + b[2] + c[2]) / 3,
            shade,
          });
        }
      }
    }

    triangles.sort((a, b) => a.depth - b.depth);
    projectedTrisRef.current = triangles;

    for (const tri of triangles) {
      const isSelectedFace =
        selectedTarget?.kind === "face" &&
        selectedTarget.face.face_id === tri.face.face_id;
      const isSelectedComp =
        Boolean(selectedComponentId) &&
        tri.face.component_id === selectedComponentId;
      const isPrecisionBore = tri.face.provenance_tags.includes("bearing_seat_wall");

      if (isSelectedFace || isSelectedComp) {
        ctx.fillStyle = `rgba(250, 204, 21, ${(0.55 + tri.shade * 0.35).toFixed(2)})`;
      } else if (isPrecisionBore) {
        const g = Math.round(135 + tri.shade * 70);
        const b = Math.round(180 + tri.shade * 60);
        ctx.fillStyle = `rgb(68, ${g}, ${b})`;
      } else {
        const base = Math.round(92 + tri.shade * 95);
        ctx.fillStyle = `rgb(${base}, ${Math.round(base * 1.07)}, ${Math.round(base * 1.18)})`;
      }

      ctx.beginPath();
      ctx.moveTo(tri.pts[0][0], tri.pts[0][1]);
      ctx.lineTo(tri.pts[1][0], tri.pts[1][1]);
      ctx.lineTo(tri.pts[2][0], tri.pts[2][1]);
      ctx.closePath();
      ctx.fill();
    }

    // Render B-Rep topological edges (respecting Explode + Section Cut)
    for (const edge of sceneMesh.edges) {
      const [ox, oy, oz] = computeComponentExplodeOffset(
        edge.component_id,
        componentOrder,
        explodeFactor,
      );
      const isSelectedEdge =
        selectedTarget?.kind === "edge" &&
        selectedTarget.edge.edge_id === edge.edge_id;

      ctx.strokeStyle = isSelectedEdge
        ? "#facc15"
        : edge.provenance_tags.includes("bearing_seat_wall")
          ? "#38bdf8"
          : "#cbd5e1";
      ctx.lineWidth = isSelectedEdge ? 2.8 : 1.35;

      const points = normalizePolyline(edge.polyline).map(([x, y, z]): Vec3 => [x + ox, y + oy, z + oz]);
      ctx.beginPath();
      for (let i = 1; i < points.length; i += 1) {
        const segment = [points[i - 1], points[i]];
        const visible = sectionCut.enabled
          ? clipToSection(segment, sectionCut.axis === "Z" ? 2 : 0, cutCoord, false)
          : segment;
        if (visible.length < 2) continue;
        const [a, b] = visible.map((point) => projectPoint(...point, camera, cx, cy, centerZ));
        ctx.moveTo(a[0], a[1]);
        ctx.lineTo(b[0], b[1]);
      }
      ctx.stroke();
    }

    // Section Cut Plane wireframe indicator
    if (sectionCut.enabled) {
      const r = meshBounds.maxR * 1.15;
      const quad: readonly Vec3[] =
        sectionCut.axis === "Z"
          ? [
              [-r, -r, cutCoord],
              [r, -r, cutCoord],
              [r, r, cutCoord],
              [-r, r, cutCoord],
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
        const p = projectPoint(pt[0], pt[1], pt[2], camera, cx, cy, centerZ);
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
    sceneMesh.edit_handles.forEach((handle, idx) => {
      const [hx, hy, hz] = resolveHandleAnchor(handle, idx, meshBounds);
      const p = projectPoint(hx, hy, hz, camera, cx, cy, centerZ);
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
    projectedHandlesRef.current = anchors;

    // Axis triad gizmo in bottom-left corner
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
        { ...camera, zoom: 1 },
        origin[0],
        origin[1],
        0,
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
  }, [
    viewportTab,
    sceneMesh,
    camera,
    keepInBbox,
    showKeepInBox,
    selectedTarget,
    selectedComponentId,
    activeHandle,
    sectionCut,
    explodeFactor,
    componentOrder,
    meshBounds,
  ]);

  const handleTriggerEdit = (handle: EditHandle, nextVal: number) => {
    setDraftValues((prev) => ({ ...prev, [handle.handle_id]: nextVal }));
    const plan = computeWritebackPlan(
      kclSource,
      handle,
      nextVal,
      extractNameInput,
    );

    if (plan.kind === "tier1") {
      setPendingTier2(null);
      setPendingTier3(null);
      onCommitKclSource(
        plan.updatedSource,
        `Tier 1 原地无损更新 ${plan.paramName}: ${plan.previousText} -> ${plan.replacementText}`,
      );
      onFocusAstSpan(handle.ast_span, `Tier 1: ${handle.param_name}`);
    } else if (plan.kind === "tier2") {
      setPendingTier3(null);
      setPendingTier2(plan);
      onFocusAstSpan(handle.ast_span, `Tier 2 选择: ${handle.param_name}`);
    } else {
      setPendingTier2(null);
      setPendingTier3(plan);
      onFocusAstSpan(
        plan.highlightedSpan,
        `Tier 3 锁定保护: ${handle.param_name}`,
      );
    }
  };

  const handleCanvasPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary || event.button !== 0) return;
    const canvas = event.currentTarget;
    canvas.setPointerCapture(event.pointerId);
    const rect = canvas.getBoundingClientRect();
    const px = (event.clientX - rect.left) * (canvas.width / rect.width);
    const py = (event.clientY - rect.top) * (canvas.height / rect.height);

    // Check if pointer hit a 3D Edit Handle anchor for direct 3D dragging
    for (const anchor of projectedHandlesRef.current) {
      const dist = Math.hypot(px - anchor.x, py - anchor.y);
      if (dist <= anchor.radius + 5) {
        const currentVal =
          draftValues[anchor.handle.handle_id] ?? anchor.handle.current_value;
        setActiveHandleId(anchor.handle.handle_id);
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

    orbitDragRef.current = {
      active: true,
      moved: false,
      x: event.clientX,
      y: event.clientY,
    };
  };

  const handleCanvasPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary) return;
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

    if (!orbitDragRef.current.active) {
      return;
    }
    const dx = event.clientX - orbitDragRef.current.x;
    const dy = event.clientY - orbitDragRef.current.y;
    const moved =
      orbitDragRef.current.moved || Math.abs(dx) > 2 || Math.abs(dy) > 2;
    orbitDragRef.current = {
      active: true,
      moved,
      x: event.clientX,
      y: event.clientY,
    };
    setCamera((prev) => ({
      ...prev,
      yaw: prev.yaw + dx * 0.01,
      pitch: Math.max(-1.45, Math.min(1.45, prev.pitch + dy * 0.01)),
    }));
  };

  const finishCanvasPointer = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!event.isPrimary) return;
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
        handleTriggerEdit(handle, latestValue);
      } else {
        setDraftValues((prev) => {
          const next = { ...prev };
          delete next[handle.handle_id];
          return next;
        });
      }
      return;
    }
    orbitDragRef.current.active = false;
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
    const scaleX = canvas.width / rect.width;
    const scaleY = canvas.height / rect.height;
    const px = (event.clientX - rect.left) * scaleX;
    const py = (event.clientY - rect.top) * scaleY;

    const tris = projectedTrisRef.current;
    for (let i = tris.length - 1; i >= 0; i -= 1) {
      const tri = tris[i];
      if (pointInTriangle(px, py, tri.pts[0], tri.pts[1], tri.pts[2])) {
        onSelectTarget({ kind: "face", face: tri.face });
        onFocusAstSpan(tri.face.ast_span, `Face: ${tri.face.face_id}`);
        return;
      }
    }
    onSelectTarget(null);
  };

  const handleConfirmTier2 = (choice: Tier2ChoiceKind) => {
    if (!pendingTier2 || !activeHandle) {
      return;
    }
    const applied = applyTier2Choice(
      kclSource,
      activeHandle,
      pendingTier2.targetValue,
      choice,
    );
    setPendingTier2(null);
    onCommitKclSource(
      applied.updatedSource,
      `Tier 2 (${choice}) 同步更新 ${applied.option.targetParam} = ${applied.option.nextValue}${applied.option.unit}`,
    );
  };

  const handleApplyTier3Extract = () => {
    if (!pendingTier3 || !activeHandle) {
      return;
    }
    const applied = applyTier3ExtractParameter(
      kclSource,
      activeHandle,
      pendingTier3.requestedValue,
      extractNameInput,
    );
    setPendingTier3(null);
    onCommitKclSource(
      applied.updatedSource,
      `Tier 3 重构：已提取独立参数 \`${applied.refactor.extractedParamName} = ${applied.refactor.extractedValue}${applied.refactor.unit}\``,
    );
  };

  const currentHandleValue = activeHandle
    ? (draftValues[activeHandle.handle_id] ?? activeHandle.current_value)
    : 0;

  return (
    <main className="panel panel-center" aria-label="3D 双轨视口与参数手柄交互区">
      {/* 视口工具栏 */}
      <div className="viewport-toolbar">
        <div className="pill-group">
          <button
            type="button"
            className={`pill-btn ${viewportTab === "3d" ? "active" : ""}`}
            onClick={() => setViewportTab("3d")}
          >
            3D 视口
          </button>
          <button
            type="button"
            className={`pill-btn ${viewportTab === "drawing" ? "active" : ""}`}
            onClick={() => setViewportTab("drawing")}
          >
            2D 工程图 (Drawing)
          </button>
          {(["iso", "top", "front", "right"] as const).map((preset) => (
            <button
              key={preset}
              type="button"
              className="pill-btn"
              onClick={() => {
                setViewportTab("3d");
                setCamera(VIEW_PRESETS[preset]);
              }}
            >
              {preset.toUpperCase()}
            </button>
          ))}
        </div>

        <div className="viewport-controls-inline">
          <label className="checkbox-inline">
            <input
              type="checkbox"
              checked={showKeepInBox}
              onChange={(e) => setShowKeepInBox(e.target.checked)}
            />
            Keep-In
          </label>
          <label className="checkbox-inline">
            <input
              type="checkbox"
              checked={sectionCut.enabled}
              onChange={(e) =>
                setSectionCut((prev) => ({ ...prev, enabled: e.target.checked }))
              }
            />
            剖切
          </label>
          {sectionCut.enabled && (
            <>
              <select
                className="mini-select"
                value={sectionCut.axis}
                onChange={(e) =>
                  setSectionCut((prev) => ({
                    ...prev,
                    axis: e.target.value === "X" ? "X" : "Z",
                  }))
                }
                aria-label="剖切轴向"
              >
                <option value="Z">Z 轴</option>
                <option value="X">X 轴</option>
              </select>
              <input
                type="range"
                min={0.05}
                max={0.95}
                step={0.02}
                value={sectionCut.ratio}
                onChange={(e) =>
                  setSectionCut((prev) => ({
                    ...prev,
                    ratio: Number(e.target.value),
                  }))
                }
                aria-label="剖切深度"
              />
            </>
          )}
          {isAssembly && (
            <label className="checkbox-inline">
              爆炸图
              <input
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={explodeFactor}
                onChange={(e) => setExplodeFactor(Number(e.target.value))}
                aria-label="装配体爆炸视图系数"
              />
            </label>
          )}
        </div>
      </div>

      {/* 3D 视口画布 / 2D 工程图画布 */}
      <div className="canvas-stage">
        {viewportTab === "3d" ? (
          <>
            <canvas
              ref={canvasRef}
              width={760}
              height={440}
              className="viewport-canvas"
              onClick={handleCanvasClick}
              onPointerDown={handleCanvasPointerDown}
              onPointerMove={handleCanvasPointerMove}
              onPointerUp={finishCanvasPointer}
              onPointerCancel={finishCanvasPointer}
              onLostPointerCapture={finishCanvasPointer}
              onWheel={(e) => {
                e.preventDefault();
                setCamera((prev) => ({
                  ...prev,
                  zoom: Math.max(1.1, Math.min(5.2, prev.zoom - e.deltaY * 0.002)),
                }));
              }}
            />
            {draggingBadge && (
              <div className="drag-hud-badge">拖拽参数: {draggingBadge}</div>
            )}
          </>
        ) : (
          <div className="drawing-stage" aria-label="2D 工程图纸视图">
            {drawingSvg ? (
              <img
                className="drawing-svg-inner"
                alt="2D 工程图纸"
                src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(drawingSvg)}`}
              />
            ) : (
              <div className="muted-text">暂无 2D 工程图 SVG，请在后端编译模式生成。</div>
            )}
          </div>
        )}

        {/* 悬浮 Tier 1/2/3 参数手柄控制器 */}
        {activeHandle && viewportTab === "3d" && (
          <div className="handle-overlay-card">
            <div className="handle-overlay-header">
              <span className={`tier-pill tier-${activeHandle.tier}`}>
                Tier {activeHandle.tier}
              </span>
              <span className="handle-title">
                {activeHandle.label ?? activeHandle.param_name}
              </span>
            </div>

            <div className="handle-controls-row">
              <input
                type="range"
                min={activeHandle.min_value ?? 5}
                max={activeHandle.max_value ?? 160}
                step={0.5}
                value={currentHandleValue}
                onChange={(e) =>
                  handleTriggerEdit(activeHandle, Number(e.target.value))
                }
                aria-label={`${activeHandle.param_name} 滑块`}
              />
              <span className="handle-value-badge">
                {currentHandleValue.toFixed(1)} {activeHandle.unit}
              </span>
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
                    setPendingTier2(null);
                    setPendingTier3(null);
                    onFocusAstSpan(h.ast_span, `Tier ${h.tier}: ${h.param_name}`);
                  }}
                >
                  T{h.tier} · {h.param_name}
                </button>
              ))}
            </div>

            {/* Tier 2 双环手柄双选补丁浮窗 */}
            {pendingTier2 && (
              <div className="sync-popover sync-popover-tier2">
                <div className="sync-popover-title">
                  Tier 2 仿射偏移消歧：请选择写回目标
                </div>
                <div className="sync-choice-list">
                  {pendingTier2.options.map((opt) => (
                    <button
                      key={opt.choice}
                      type="button"
                      className="sync-choice-btn"
                      onClick={() => handleConfirmTier2(opt.choice)}
                    >
                      <strong>{opt.label}</strong>
                      <span>{opt.description}</span>
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* Tier 3 锁定表达式溯源卡片与 Extract Parameter 重构按钮 */}
            {pendingTier3 && (
              <div className="sync-popover sync-popover-tier3">
                <div className="sync-popover-title">
                  Tier 3 只读保护：耦合表达式禁止直接覆写
                </div>
                <p className="sync-popover-reason">{pendingTier3.reason}</p>
                <div className="extract-row">
                  <input
                    type="text"
                    className="extract-input"
                    value={extractNameInput}
                    onChange={(e) => setExtractNameInput(e.target.value)}
                    aria-label="提取参数名称"
                  />
                  <button
                    type="button"
                    className="btn-primary"
                    onClick={handleApplyTier3Extract}
                  >
                    提取独立参数 (Extract Parameter)
                  </button>
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      {/* 面/边拾取双向高亮联动状态栏 */}
      <section className="card selection-status-bar">
        <div className="selection-summary">
          {selectedTarget ? (
            selectedTarget.kind === "face" ? (
              <>
                <span className="badge badge-accent">FACE</span>
                <strong>{selectedTarget.face.face_id}</strong>
                <span>({selectedTarget.face.surface_type})</span>
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
          ) : (
            <span className="muted-text">
              直接拖拽 3D 锚点调参 · 点击面/下方拓扑标签双向高亮源码
            </span>
          )}
        </div>
        <div className="topo-chip-row">
          {sceneMesh.faces.map((f: FaceMesh) => (
            <button
              key={f.face_id}
              type="button"
              className={`topo-chip ${
                selectedTarget?.kind === "face" &&
                selectedTarget.face.face_id === f.face_id
                  ? "active"
                  : ""
              }`}
              onClick={() => {
                onSelectTarget({ kind: "face", face: f });
                onFocusAstSpan(f.ast_span, `Face: ${f.face_id}`);
              }}
            >
              {f.face_id}
            </button>
          ))}
          {sceneMesh.edges.slice(0, 3).map((e: TopoEdge) => (
            <button
              key={e.edge_id}
              type="button"
              className={`topo-chip ${
                selectedTarget?.kind === "edge" &&
                selectedTarget.edge.edge_id === e.edge_id
                  ? "active"
                  : ""
              }`}
              onClick={() => {
                onSelectTarget({ kind: "edge", edge: e });
                onFocusAstSpan(e.ast_span, `Edge: ${e.edge_id}`);
              }}
            >
              {e.edge_id}
            </button>
          ))}
        </div>
      </section>
    </main>
  );
}
