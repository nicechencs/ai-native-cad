import type { FaceMesh, SceneMeshPayload, TopoEdge, Vec3 } from "../types/scene.ts";
import { normalizePolyline } from "./studioHelpers.ts";

export type MeasurementMode = "circle" | "face";

type MeasuredPoint = { readonly point: Vec3; readonly componentId?: string };

export interface CirclePick extends MeasuredPoint {
  readonly radius: number;
  readonly polyline: readonly Vec3[];
}

export interface PlanePick extends MeasuredPoint {
  readonly face: FaceMesh;
  readonly normal: Vec3;
}

export interface DistanceMeasurement {
  readonly distanceMm: number;
  readonly points: readonly [MeasuredPoint, MeasuredPoint];
}

const MM_PER_UNIT: Readonly<Record<SceneMeshPayload["units"], number>> = {
  mm: 1,
  m: 1000,
  in: 25.4,
};
const sub = (a: Vec3, b: Vec3): Vec3 => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const add = (a: Vec3, b: Vec3): Vec3 => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
const scale = (a: Vec3, k: number): Vec3 => [a[0] * k, a[1] * k, a[2] * k];
const dot = (a: Vec3, b: Vec3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: Vec3, b: Vec3): Vec3 => [
  a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0],
];
const length = (a: Vec3) => Math.hypot(...a);
const vertex = (face: FaceMesh, i: number): Vec3 =>
  [face.vertices[3 * i], face.vertices[3 * i + 1], face.vertices[3 * i + 2]];

/** Use well-separated samples and reject noncoplanar / noncircular data. */
function fitCircle(points: readonly Vec3[], tolerance: number) {
  if (points.length < 3 || points.some((p) => !p.every(Number.isFinite))) return null;
  const origin = points[0];
  let u: Vec3 = [0, 0, 0];
  let v: Vec3 = [0, 0, 0];
  for (const pt of points) {
    const d = sub(pt, origin);
    if (dot(d, d) > dot(u, u)) u = d;
  }
  for (const pt of points) {
    const d = sub(pt, origin);
    const c = cross(u, d);
    const best = cross(u, v);
    if (dot(c, c) > dot(best, best)) v = d;
  }
  const n = cross(u, v);
  const n2 = dot(n, n);
  if (n2 <= (dot(u, u) * 1e-8) ** 2) return null;
  const offset = scale(add(scale(cross(v, n), dot(u, u)), scale(cross(n, u), dot(v, v))), 1 / (2 * n2));
  const point = add(origin, offset);
  const radius = length(offset);
  const normal = scale(n, 1 / Math.sqrt(n2));
  if (!Number.isFinite(radius) || radius <= tolerance) return null;
  const error = Math.max(tolerance, radius * 1e-5);
  if (points.some((pt) => {
    const d = sub(pt, point);
    return Math.abs(dot(d, normal)) > error || Math.abs(length(d) - radius) > error;
  })) return null;
  return { point, radius, normal };
}

function cylinderAxis(face: FaceMesh): Vec3 | null {
  const first: Vec3 = [face.normals[0], face.normals[1], face.normals[2]];
  let axis: Vec3 = [0, 0, 0];
  for (let i = 3; i + 2 < face.normals.length; i += 3) {
    const candidate = cross(first, [face.normals[i], face.normals[i + 1], face.normals[i + 2]]);
    if (dot(candidate, candidate) > dot(axis, axis)) axis = candidate;
  }
  const magnitude = length(axis);
  if (magnitude < 1e-5) return null;
  return scale(axis, 1 / magnitude);
}

/** Native imports provide boundary segments, not typed circles. Weld circular
 * boundary segments of neighboring cylindrical faces within one component.
 * STEP often stores each half of a cylinder as a separate face. */
function cylinderLoops(faces: readonly FaceMesh[], tolerance: number): Vec3[][] {
  const ids = new Map<string, number>();
  const points: Vec3[] = [];
  const adjacency = new Map<number, Set<number>>();
  const weld = (p: Vec3) => {
    const key = p.map((v) => Math.round(v / tolerance)).join(":");
    const id = ids.get(key);
    if (id !== undefined) return id;
    ids.set(key, points.length);
    return points.push(p) - 1;
  };
  for (const face of faces) {
    const axis = cylinderAxis(face);
    if (!axis) continue;
    const edges = new Map<string, { a: number; b: number; count: number }>();
    for (let i = 0; i + 2 < face.indices.length; i += 3) {
      const tri = [face.indices[i], face.indices[i + 1], face.indices[i + 2]];
      if (new Set(tri).size !== 3) continue;
      for (let j = 0; j < 3; j += 1) {
        const x = tri[j];
        const y = tri[(j + 1) % 3];
        const a = Math.min(x, y);
        const b = Math.max(x, y);
        const key = `${a}:${b}`;
        const edge = edges.get(key);
        if (edge) edge.count += 1;
        else edges.set(key, { a, b, count: 1 });
      }
    }
    for (const { a, b, count } of edges.values()) {
      if (count !== 1) continue;
      const start = vertex(face, a);
      const end = vertex(face, b);
      // A circular rim's endpoints have the same axial coordinate; skip seams
      // and obliquely trimmed boundaries before joining the rim fragments.
      if (!(Math.abs(dot(sub(start, end), axis)) <= tolerance)) continue;
      const u = weld(start);
      const v = weld(end);
      if (u === v) continue;
      if (!adjacency.has(u)) adjacency.set(u, new Set());
      if (!adjacency.has(v)) adjacency.set(v, new Set());
      adjacency.get(u)!.add(v);
      adjacency.get(v)!.add(u);
    }
  }
  const seen = new Set<number>();
  const loops: Vec3[][] = [];
  for (const start of adjacency.keys()) {
    const loop: Vec3[] = [];
    let previous = -1;
    let current = start;
    while (!seen.has(current)) {
      seen.add(current);
      const neighbors = adjacency.get(current)!;
      if (neighbors.size !== 2) break;
      loop.push(points[current]);
      const [a, b] = neighbors;
      const next = a === previous ? b : a;
      previous = current;
      current = next;
    }
    if (current === start && loop.length >= 8) loops.push(loop);
  }
  return loops;
}

function coversCircle(loop: readonly Vec3[], circle: NonNullable<ReturnType<typeof fitCircle>>) {
  let sweep = 0;
  for (let i = 0; i < loop.length; i += 1) {
    const a = sub(loop[i], circle.point);
    const b = sub(loop[(i + 1) % loop.length], circle.point);
    const step = Math.atan2(dot(cross(a, b), circle.normal), dot(a, b));
    if (Math.abs(step) > Math.PI / 2 || sweep * step < 0) return false;
    sweep += step;
  }
  return Math.abs(Math.abs(sweep) - 2 * Math.PI) < 1e-5;
}

export function collectCircles(scene: SceneMeshPayload): readonly CirclePick[] {
  const tolerance = 1e-6 / MM_PER_UNIT[scene.units];
  const circles: CirclePick[] = [];
  const append = (
    polyline: readonly Vec3[], componentId: TopoEdge["component_id"], recovered: boolean,
  ) => {
    const circle = fitCircle(polyline, tolerance);
    if (!circle || (recovered && !coversCircle(polyline, circle))) return;
    // The same center may legitimately belong to concentric edges; keep different radii.
    if (recovered && circles.some((other) => other.componentId === componentId &&
      Math.abs(other.radius - circle.radius) <= tolerance && length(sub(other.point, circle.point)) <= tolerance)) return;
    const points = recovered ? [...polyline, polyline[0]] : polyline;
    circles.push({ point: circle.point, radius: circle.radius, polyline: points, componentId });
  };
  for (const edge of scene.edges) {
    if (edge.curve_type !== "circle" && edge.curve_type !== "arc") continue;
    const polyline = normalizePolyline(edge.polyline);
    const points = edge.curve_type === "circle" && polyline.length >= 3 &&
      length(sub(polyline[0], polyline.at(-1)!)) > tolerance ? [...polyline, polyline[0]] : polyline;
    append(points, edge.component_id, false);
  }
  const cylindersByComponent = new Map<string | undefined, FaceMesh[]>();
  for (const face of scene.faces) {
    if (face.surface_type !== "cylinder" || face.exact_interface === false) continue;
    if (!cylindersByComponent.has(face.component_id)) cylindersByComponent.set(face.component_id, []);
    cylindersByComponent.get(face.component_id)!.push(face);
  }
  for (const [componentId, faces] of cylindersByComponent) {
    for (const loop of cylinderLoops(faces, tolerance)) append(loop, componentId, true);
  }
  return circles;
}

export function makePlanePick(face: FaceMesh, point: Vec3, units: SceneMeshPayload["units"]): PlanePick | null {
  if (face.surface_type !== "plane" || !point.every(Number.isFinite)) return null;
  let areaNormal: Vec3 = [0, 0, 0];
  let origin: Vec3 = point;
  for (let i = 0; i + 2 < face.indices.length; i += 3) {
    const a = vertex(face, face.indices[i]);
    const n = cross(sub(vertex(face, face.indices[i + 1]), a), sub(vertex(face, face.indices[i + 2]), a));
    if (dot(n, n) > dot(areaNormal, areaNormal)) {
      areaNormal = n;
      origin = a;
    }
  }
  const area = length(areaNormal);
  if (!Number.isFinite(area) || area === 0) return null;
  const normal = scale(areaNormal, 1 / area);
  // Clustered mesh faces may be labeled plane without actually being coplanar.
  const tolerance = 1e-4 / MM_PER_UNIT[units];
  for (const index of new Set(face.indices)) {
    if (!(Math.abs(dot(sub(vertex(face, index), origin), normal)) <= tolerance)) return null;
  }
  return {
    face, normal, componentId: face.component_id,
    point: sub(point, scale(normal, dot(sub(point, origin), normal))),
  };
}

export function measurePlanes(a: PlanePick, b: PlanePick, units: SceneMeshPayload["units"]): DistanceMeasurement | null {
  if (length(cross(a.normal, b.normal)) > 1e-5) return null;
  const depth = dot(sub(b.point, a.point), b.normal);
  return {
    distanceMm: Math.abs(depth) * MM_PER_UNIT[units],
    points: [a, { point: add(a.point, scale(b.normal, depth)), componentId: b.componentId }],
  };
}

export function measureCircles(a: CirclePick, b: CirclePick, units: SceneMeshPayload["units"]): DistanceMeasurement {
  return { distanceMm: length(sub(a.point, b.point)) * MM_PER_UNIT[units], points: [a, b] };
}

// Keep display and copied DSL consistent and do not round a small nonzero distance to zero.
export function measurementDecimal(distanceMm: number): string {
  return distanceMm.toLocaleString("en-US", {
    useGrouping: false,
    ...(distanceMm > 0 && distanceMm < 1e-6 ? { maximumSignificantDigits: 3 } : { maximumFractionDigits: 6 }),
  });
}

export function measurementConst(mode: MeasurementMode, distanceMm: number): string {
  const name = mode === "circle" ? "circle_center_distance" : "face_distance";
  return `const ${name} = ${measurementDecimal(distanceMm)}mm`;
}
